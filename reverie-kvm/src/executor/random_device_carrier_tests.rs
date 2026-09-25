// Exercise the production syscall dispatcher; native Linux access-mode and
// random-device type contracts are independent of the deterministic byte oracle.
#[test]
fn random_carrier_access_modes_reject_io_before_bad_guest_memory() {
    let root = TestDir::new();
    let mut state = test_state(&root.0);
    let mut memory = GuestMemory::new(0, PAGE_SIZE as usize).unwrap();
    for path in [
        "/dev/random",
        "/dev/urandom",
        "/dev//random",
        "/dev/./urandom",
    ] {
        for mode in [libc::O_WRONLY, libc::O_PATH, libc::O_ACCMODE] {
            let fd = open_with_flags(&mut memory, &mut state, path, mode);
            assert!(fd >= 0, "{path} mode {mode:#x}: {fd}");
            for (number, args) in [
                (libc::SYS_read, [fd as u64, u64::MAX, 0, 0, 0, 0]),
                (libc::SYS_readv, [fd as u64, u64::MAX, 1025, 0, 0, 0]),
                (libc::SYS_pread64, [fd as u64, u64::MAX, 0, 0, 0, 0]),
                (libc::SYS_preadv, [fd as u64, u64::MAX, 1025, 0, 0, 0]),
            ] {
                assert_eq!(
                    syscall_result(&mut memory, &mut state, number, args),
                    negative_errno(libc::EBADF)
                );
            }
            assert_eq!(close(&mut state, fd as u64), 0);
        }
    }
}

#[test]
fn random_carrier_metadata_has_character_device_identity() {
    let root = TestDir::new();
    let mut state = test_state(&root.0);
    let mut memory = GuestMemory::new(0, PAGE_SIZE as usize).unwrap();
    for (path, minor) in [("/dev/random", 8), ("/dev/urandom", 9), ("/dev//random", 8)] {
        for mode in [
            libc::O_RDONLY,
            libc::O_WRONLY,
            libc::O_RDWR,
            libc::O_PATH,
            3,
        ] {
            let fd = open_with_flags(&mut memory, &mut state, path, mode);
            assert!(fd >= 0, "{path} mode {mode}: {fd}");
            let stat = guest_object_stat(&state, fd as i32, None).unwrap();
            assert_eq!(stat.st_mode & libc::S_IFMT, libc::S_IFCHR);
            assert_eq!(
                (libc::major(stat.st_rdev), libc::minor(stat.st_rdev)),
                (1, minor)
            );
            assert_eq!((stat.st_size, stat.st_blocks), (0, 0));
            assert_eq!(close(&mut state, fd as u64), 0);
        }
    }
}

#[test]
fn random_carrier_writes_preserve_bytes_and_read_cursor() {
    let root = TestDir::new();
    let mut state = test_state(&root.0);
    let mut memory = GuestMemory::new(0, 3 * PAGE_SIZE as usize).unwrap();
    memory
        .map_user_permissions(0, 2 * PAGE_SIZE, true, true)
        .unwrap();
    memory
        .map_user_permissions(2 * PAGE_SIZE, PAGE_SIZE, false, false)
        .unwrap();
    memory.enable_user_access();
    for path in ["/dev/random", "/dev/urandom"] {
        let fd = open_with_flags(&mut memory, &mut state, path, libc::O_RDWR);
        assert!(fd >= 0, "{path}: {fd}");
        let original = random_stream_carrier_bytes(&state, fd, 0, 65536);
        assert_eq!(
            random_stream_read(&mut memory, &mut state, fd, 0x300, 23),
            23
        );
        for number in [libc::SYS_write, libc::SYS_pwrite64] {
            assert_eq!(
                syscall_result(
                    &mut memory,
                    &mut state,
                    number,
                    [fd as u64, 2 * PAGE_SIZE - 7, 15, 101, 0, 0]
                ),
                7
            );
            assert_eq!(
                syscall_result(
                    &mut memory,
                    &mut state,
                    number,
                    [fd as u64, 2 * PAGE_SIZE, 1, 101, 0, 0]
                ),
                negative_errno(libc::EFAULT)
            );
        }
        write_guest_iovecs(
            &mut memory,
            0x100,
            &[(0x400, 7), (2 * PAGE_SIZE, 8), (0x500, 9)],
        );
        assert_eq!(
            syscall_result(
                &mut memory,
                &mut state,
                libc::SYS_writev,
                [fd as u64, 0x100, 3, 0, 0, 0]
            ),
            7
        );
        assert_eq!(
            random_stream_read(&mut memory, &mut state, fd, 0x600, 19),
            19
        );
        let expected: Vec<u8> = (23..42).map(|i| ((i * 73 + 41) & 255) as u8).collect();
        assert_eq!(random_stream_bytes(&memory, 0x600, 19), expected);
        assert_eq!(random_stream_carrier_bytes(&state, fd, 0, 65536), original);
        assert_eq!(
            file_status_flags(&state.files[&(fd as i32)]).unwrap() & libc::O_ACCMODE,
            libc::O_RDONLY
        );
        close(&mut state, fd as u64);
    }
}

fn random_carrier_flags(memory: &mut GuestMemory, state: &mut LoadedStaticElf, fd: i64) -> i64 {
    syscall_result(
        memory,
        state,
        libc::SYS_fcntl,
        [fd as u64, libc::F_GETFL as u64, 0, 0, 0, 0],
    )
}

#[test]
fn random_carrier_status_and_access_survive_alias_fork_exec_and_reuse() {
    let root = TestDir::new();
    std::fs::write(root.0.join("plain"), b"plain").unwrap();
    let mut memory = GuestMemory::new(0, PAGE_SIZE as usize).unwrap();
    for mode in [
        libc::O_RDONLY,
        libc::O_WRONLY,
        libc::O_RDWR,
        3,
        libc::O_PATH,
    ] {
        for opened_async in [0, libc::O_ASYNC] {
            let mut state = test_state(&root.0);
            let fd = open_with_flags(
                &mut memory,
                &mut state,
                "/dev/urandom",
                mode | opened_async | libc::O_NOFOLLOW,
            );
            assert!(fd >= 0);
            let base = if mode == libc::O_PATH {
                libc::O_PATH | libc::O_NOFOLLOW
            } else {
                0o100000 | mode | opened_async | libc::O_NOFOLLOW
            };
            assert_eq!(
                random_carrier_flags(&mut memory, &mut state, fd),
                i64::from(base)
            );
            let alias = syscall_result(
                &mut memory,
                &mut state,
                libc::SYS_dup,
                [fd as u64, 0, 0, 0, 0, 0],
            );
            let description = state.random_device_descriptions[&(fd as i32)].clone();
            let mut forked = state.try_clone_for_fork(2).unwrap();
            for requested in [
                libc::O_ASYNC | libc::O_APPEND | libc::O_NONBLOCK | libc::O_RDWR,
                libc::O_RDONLY,
            ] {
                let result = syscall_result(
                    &mut memory,
                    &mut forked,
                    libc::SYS_fcntl,
                    [
                        alias as u64,
                        libc::F_SETFL as u64,
                        requested as u64,
                        0,
                        0,
                        0,
                    ],
                );
                let expected = if mode == libc::O_PATH {
                    assert_eq!(result, negative_errno(libc::EBADF));
                    base
                } else {
                    assert_eq!(result, 0);
                    base | (requested & (libc::O_ASYNC | libc::O_APPEND | libc::O_NONBLOCK))
                };
                assert_eq!(
                    random_carrier_flags(&mut memory, &mut state, fd),
                    i64::from(expected)
                );
                assert_eq!(
                    random_carrier_flags(&mut memory, &mut forked, alias),
                    i64::from(expected)
                );
            }
            let mut replacement = test_state(&root.0);
            replacement.inherit_process_state(forked);
            assert!(Arc::ptr_eq(
                &description,
                &replacement.random_device_descriptions[&(alias as i32)]
            ));
            assert_eq!(
                random_carrier_flags(&mut memory, &mut replacement, alias),
                i64::from(base)
            );
            assert_eq!(close(&mut state, fd as u64), 0);
            let plain = open_readonly(&mut memory, &mut state, "plain");
            assert_eq!(plain, fd);
            assert!(
                !state
                    .random_device_descriptions
                    .contains_key(&(plain as i32))
            );
            assert!(
                state
                    .random_device_descriptions
                    .contains_key(&(alias as i32))
            );
        }
    }
}

#[test]
fn random_carrier_write_validation_precedes_copy_and_ignores_read_position() {
    let root = TestDir::new();
    let mut state = test_state(&root.0);
    let mut memory = GuestMemory::new(0, 2 * PAGE_SIZE as usize).unwrap();
    let fd = open_with_flags(&mut memory, &mut state, "/dev/urandom", libc::O_RDWR);
    assert!(fd >= 0);
    assert_eq!(
        syscall_result(
            &mut memory,
            &mut state,
            libc::SYS_lseek,
            [fd as u64, i64::MAX as u64, libc::SEEK_SET as u64, 0, 0, 0]
        ),
        i64::MAX
    );
    write_guest_iovecs(&mut memory, 0x100, &[(0x300, 7), (0x400, 11)]);
    for number in [libc::SYS_writev, libc::SYS_pwritev, libc::SYS_pwritev2] {
        assert_eq!(
            syscall_result(
                &mut memory,
                &mut state,
                number,
                [fd as u64, 0x100, (1 << 32) + 2, 0, 0, 0]
            ),
            18
        );
        assert_eq!(
            syscall_result(
                &mut memory,
                &mut state,
                number,
                [fd as u64, u64::MAX, 1 << 32, 0, 0, u32::MAX as u64]
            ),
            0
        );
        assert_eq!(
            syscall_result(
                &mut memory,
                &mut state,
                number,
                [fd as u64, u64::MAX, 1025, 0, 0, 0]
            ),
            negative_errno(libc::EINVAL)
        );
    }
    assert_eq!(
        syscall_result(
            &mut memory,
            &mut state,
            libc::SYS_write,
            [fd as u64, 0x300, 7, 0, 0, 0]
        ),
        7
    );
    assert_eq!(
        syscall_result(
            &mut memory,
            &mut state,
            libc::SYS_pwritev2,
            [fd as u64, 0x100, 1, i64::MAX as u64, 0, 0x8000]
        ),
        negative_errno(libc::EINVAL)
    );
    assert_eq!(
        syscall_result(
            &mut memory,
            &mut state,
            libc::SYS_pwritev2,
            [fd as u64, 0x100, 1, 0, 0, 0x8000]
        ),
        negative_errno(libc::EOPNOTSUPP)
    );
    assert_eq!(random_stream_position(&state, fd), i64::MAX);
}

#[test]
fn random_carrier_copy_poison_after_real_prefix_remains_terminal() {
    let root = TestDir::new();
    for after_copy in [false, true] {
        let mut state = test_state(&root.0);
        let mut memory = GuestMemory::new(0, 3 * PAGE_SIZE as usize).unwrap();
        let fd = open_with_flags(&mut memory, &mut state, "/dev/random", libc::O_RDWR);
        assert!(fd >= 0);
        let description = state.random_device_descriptions[&(fd as i32)].clone();
        let original = random_stream_carrier_bytes(&state, fd, 0, 65536);
        memory.write(0x1000, &[0x5a; 4109]).unwrap();
        let cause = Arc::new(crate::Error::EntryControl {
            operation: "random input copy",
            source: std::io::Error::from_raw_os_error(libc::EIO),
        });
        if after_copy {
            *description.poison_after_copy.lock().unwrap() = Some(cause.clone());
        } else {
            memory
                .entry_gate()
                .poison(None, crate::Error::SharedFailure(cause.clone()));
        }
        let mut executor = ElfExecutor::new(state, false);
        match executor.execute_checked(
            &SyscallRequest::new(libc::SYS_write as u64, [fd as u64, 0x1000, 4109, 0, 0, 0]),
            &memory,
        ) {
            Err(error) => assert!(error.retains_primary(&cause)),
            Ok(value) => panic!("terminal input-copy failure became {value}"),
        }
        assert_eq!(
            description.consumed_input.load(Ordering::SeqCst),
            if after_copy { 4096 } else { 0 }
        );
        assert_eq!(random_stream_position(&executor.state, fd), 0);
        assert_eq!(
            random_stream_carrier_bytes(&executor.state, fd, 0, 65536),
            original
        );
    }
}

#[test]
fn random_carrier_mmap_denial_keeps_memory_and_cursor_unchanged() {
    let root = TestDir::new();
    let mut state = test_state(&root.0);
    let mut memory = GuestMemory::new(0, (BOOT_RESERVED_END + 4 * PAGE_SIZE) as usize).unwrap();
    state.mmap_base = BOOT_RESERVED_END;
    state.mmap_next = BOOT_RESERVED_END;
    state.mmap_limit = BOOT_RESERVED_END + 4 * PAGE_SIZE;
    memory
        .map_user_permissions(BOOT_RESERVED_END, PAGE_SIZE, true, true)
        .unwrap();
    memory.write(BOOT_RESERVED_END, &[0xa5; 16]).unwrap();
    for mode in [
        libc::O_RDONLY,
        libc::O_WRONLY,
        libc::O_RDWR,
        3,
        libc::O_PATH,
    ] {
        let fd = open_with_flags(&mut memory, &mut state, "/dev/random", mode);
        assert!(fd >= 0);
        for (length, flags, error) in [
            (
                0,
                libc::MAP_PRIVATE,
                if mode == libc::O_PATH {
                    libc::EBADF
                } else {
                    libc::EINVAL
                },
            ),
            (
                PAGE_SIZE,
                libc::MAP_PRIVATE | libc::MAP_FIXED,
                if mode == libc::O_PATH {
                    libc::EBADF
                } else if mode == libc::O_WRONLY || mode == 3 {
                    libc::EACCES
                } else {
                    libc::ENODEV
                },
            ),
            (
                PAGE_SIZE,
                libc::MAP_PRIVATE | libc::MAP_FIXED_NOREPLACE,
                if mode == libc::O_PATH {
                    libc::EBADF
                } else {
                    libc::EEXIST
                },
            ),
        ] {
            assert_eq!(
                syscall_result(
                    &mut memory,
                    &mut state,
                    libc::SYS_mmap,
                    [
                        BOOT_RESERVED_END,
                        length,
                        libc::PROT_READ as u64,
                        flags as u64,
                        fd as u64,
                        0
                    ]
                ),
                negative_errno(error)
            );
            assert_eq!(
                random_stream_bytes(&memory, BOOT_RESERVED_END, 16),
                [0xa5; 16]
            );
            assert_eq!(state.mmap_next, BOOT_RESERVED_END);
        }
    }
}
