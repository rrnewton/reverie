// Included in executor::tests; these use the real descriptor and executor paths.
fn capture_native_stat(fd: RawFd) -> libc::stat {
    let mut stat = std::mem::MaybeUninit::<libc::stat>::zeroed();
    assert_eq!(unsafe { libc::fstat(fd, stat.as_mut_ptr()) }, 0);
    unsafe { stat.assume_init() }
}

fn capture_native_statfs(fd: RawFd) -> libc::statfs {
    let mut stat = std::mem::MaybeUninit::<libc::statfs>::zeroed();
    assert_eq!(unsafe { libc::fstatfs(fd, stat.as_mut_ptr()) }, 0);
    unsafe { stat.assume_init() }
}

fn statfs_bytes(stat: &libc::statfs) -> &[u8] {
    // SAFETY: stat is live for the returned borrow and every byte belongs to
    // the plain Linux ABI structure. Callers initialize the complete value.
    unsafe {
        std::slice::from_raw_parts(
            std::ptr::from_ref(stat).cast::<u8>(),
            std::mem::size_of::<libc::statfs>(),
        )
    }
}

#[test]
fn captured_output_identity_is_deterministic_and_preserves_proc_symlinks() {
    let root = TestDir::new();
    let mut state = test_state(&root.0);
    let mut memory = GuestMemory::new(0, 0x4000).unwrap();
    let mut output = CapturedOutput::try_new().unwrap();
    assert_eq!(
        syscall_result(
            &mut memory,
            &mut state,
            libc::SYS_pipe2,
            [0x1800, 0, 0, 0, 0, 0]
        ),
        0
    );
    let pipe: [i32; 2] = read_struct(&memory, 0x1800);
    let ordinary =
        assert_descriptor_stat_routes(&mut memory, &mut state, pipe[0], Some(&mut output));
    let ordinary_identity = &state.fd_object_inodes[&pipe[0]];
    assert_eq!(ordinary_identity.kind, GuestFileIdentityKind::Pipe);
    assert_eq!(
        (ordinary.st_dev, ordinary.st_ino),
        (
            synthetic_dev(SYNTHETIC_PIPE_DEV_MINOR),
            ordinary_identity.inode
        )
    );
    let mut captured_inodes = Vec::new();
    for fd in [1, 2] {
        let native_carrier = capture_native_stat(host_fd(&state, fd).unwrap());
        let captured =
            assert_descriptor_stat_routes(&mut memory, &mut state, fd, Some(&mut output));
        assert_eq!(captured.st_dev, synthetic_dev(SYNTHETIC_PIPE_DEV_MINOR));
        assert_eq!(
            captured.st_ino,
            if fd == 1 {
                capture_identity::CAPTURE_STDOUT_INODE
            } else {
                capture_identity::CAPTURE_STDERR_INODE
            }
        );
        assert_eq!(
            captured.st_dev, ordinary.st_dev,
            "all guest-visible pipe objects share one synthetic device"
        );
        assert_ne!(
            captured.st_ino, ordinary.st_ino,
            "the capture stream and guest-created pipe are distinct live objects"
        );
        assert_ne!(
            (captured.st_dev, captured.st_ino),
            (native_carrier.st_dev, native_carrier.st_ino),
            "the invoking process's output carrier identity must not be guest-visible"
        );
        write_c_string(&mut memory, 0x100, "");
        for (mask, returned_bit, mount_id) in [
            (
                libc::STATX_BASIC_STATS,
                libc::STATX_MNT_ID,
                SYNTHETIC_PIPE_MNT_ID,
            ),
            (
                libc::STATX_MNT_ID,
                libc::STATX_MNT_ID,
                SYNTHETIC_PIPE_MNT_ID,
            ),
            (
                STATX_MNT_ID_UNIQUE,
                STATX_MNT_ID_UNIQUE,
                SYNTHETIC_PIPE_UNIQUE_MNT_ID,
            ),
            (
                libc::STATX_MNT_ID | STATX_MNT_ID_UNIQUE,
                STATX_MNT_ID_UNIQUE,
                SYNTHETIC_PIPE_UNIQUE_MNT_ID,
            ),
        ] {
            assert_eq!(
                metadata_call(
                    &mut memory,
                    &mut state,
                    Some(&mut output),
                    libc::SYS_statx,
                    [fd as u64, 0x100, libc::AT_EMPTY_PATH as u64, mask as u64, 0x1800, 0],
                ),
                0
            );
            let extended: libc::statx = read_struct(&memory, 0x1800);
            assert_eq!(extended.stx_mask, libc::STATX_BASIC_STATS | returned_bit);
            assert_eq!(extended.stx_mnt_id, mount_id);
            assert_eq!(
                (extended.stx_dev_major, extended.stx_dev_minor, extended.stx_ino),
                (SYNTHETIC_DEV_MAJOR, SYNTHETIC_PIPE_DEV_MINOR, captured.st_ino)
            );
        }
        captured_inodes.push(captured.st_ino);
        write_c_string(&mut memory, 0x100, &format!("/proc/self/fd/{fd}"));
        for (number, args) in [
            (
                libc::SYS_newfstatat,
                [
                    libc::AT_FDCWD as u64,
                    0x100,
                    0x800,
                    libc::AT_SYMLINK_NOFOLLOW as u64,
                    0,
                    0,
                ],
            ),
            (
                libc::SYS_statx,
                [
                    libc::AT_FDCWD as u64,
                    0x100,
                    libc::AT_SYMLINK_NOFOLLOW as u64,
                    libc::STATX_BASIC_STATS as u64,
                    0x1000,
                    0,
                ],
            ),
        ] {
            assert_eq!(
                metadata_call(&mut memory, &mut state, Some(&mut output), number, args),
                0
            );
        }
        let link: libc::stat = read_struct(&memory, 0x800);
        let linkx: libc::statx = read_struct(&memory, 0x1000);
        assert_eq!(link.st_dev, synthetic_dev(SYNTHETIC_GUEST_FD_DEV_MINOR));
        assert_eq!(link.st_mode & libc::S_IFMT, libc::S_IFLNK);
        assert_eq!(
            (linkx.stx_dev_major, linkx.stx_dev_minor, linkx.stx_ino),
            (
                libc::major(link.st_dev),
                libc::minor(link.st_dev),
                link.st_ino
            )
        );
        assert_eq!(linkx.stx_mode & libc::S_IFMT as u16, libc::S_IFLNK as u16);
        let expected = format!("pipe:[{}]", captured.st_ino);
        let count = metadata_call(
            &mut memory,
            &mut state,
            Some(&mut output),
            libc::SYS_readlink,
            [0x100, 0x2000, 256, 0, 0, 0],
        );
        assert_eq!(count as usize, expected.len());
        let mut bytes = vec![0; count as usize];
        memory.read(0x2000, &mut bytes).unwrap();
        assert_eq!(bytes, expected.as_bytes());
    }
    assert_eq!(
        captured_inodes,
        [
            capture_identity::CAPTURE_STDOUT_INODE,
            capture_identity::CAPTURE_STDERR_INODE,
        ]
    );
}

fn capture_executor_stat(executor: &mut ElfExecutor, memory: &GuestMemory, fd: i32) -> (u64, u64) {
    assert_eq!(
        executor.execute(
            &SyscallRequest::new(libc::SYS_fstat as u64, [fd as u64, 0x800, 0, 0, 0, 0]),
            memory
        ),
        0
    );
    let stat: libc::stat = read_struct(memory, 0x800);
    (stat.st_dev, stat.st_ino)
}

#[test]
fn captured_output_fdinfo_is_synthetic_and_observes_the_current_alias() {
    const KERNEL_O_LARGEFILE: i64 = 0o100000;
    let mut fixture = FdinfoFixture::new(true);
    let expected_record = |flags: i64, inode: libc::ino_t| {
        assert!(flags >= 0);
        format!(
            "pos:\t0\nflags:\t0{flags:o}\nmnt_id:\t{}\nino:\t{inode}\n",
            SYNTHETIC_PIPE_MNT_ID
        )
        .into_bytes()
    };

    for (fd, inode) in [
        (libc::STDOUT_FILENO, capture_identity::CAPTURE_STDOUT_INODE),
        (libc::STDERR_FILENO, capture_identity::CAPTURE_STDERR_INODE),
    ] {
        let flags = fixture.call(
            libc::SYS_fcntl,
            [fd as u64, libc::F_GETFL as u64, 0, 0, 0, 0],
        );
        assert_eq!(flags, i64::from(libc::O_WRONLY));
        let info = fixture.info(i64::from(fd));
        assert_eq!(fixture.read(info, 4096), expected_record(flags, inode));
        assert_eq!(fixture.call(libc::SYS_close, [info as u64, 0, 0, 0, 0, 0]), 0);
    }

    let alias = fixture.call(libc::SYS_dup, [1, 0, 0, 0, 0, 0]);
    assert!(alias >= 3);
    assert_eq!(
        fixture.call(
            libc::SYS_fcntl,
            [
                alias as u64,
                libc::F_SETFL as u64,
                libc::O_NONBLOCK as u64,
                0,
                0,
                0,
            ],
        ),
        0
    );
    assert_eq!(
        fixture.call(
            libc::SYS_fcntl,
            [1, libc::F_GETFL as u64, 0, 0, 0, 0]
        ),
        i64::from(libc::O_WRONLY | libc::O_NONBLOCK),
        "dup must share captured stdout's virtual open description"
    );
    assert_eq!(
        fixture.call(
            libc::SYS_fcntl,
            [
                alias as u64,
                libc::F_SETFD as u64,
                libc::FD_CLOEXEC as u64,
                0,
                0,
                0,
            ],
        ),
        0
    );
    let alias_flags = i64::from(libc::O_WRONLY | libc::O_NONBLOCK | libc::O_CLOEXEC);
    let alias_info = fixture.info(alias);
    assert_eq!(
        fixture.read(alias_info, 4096),
        expected_record(alias_flags, capture_identity::CAPTURE_STDOUT_INODE)
    );

    fixture
        .memory
        .write(
            0x100,
            CString::new(format!("/proc/self/fd/{alias}"))
                .unwrap()
                .as_bytes_with_nul(),
        )
        .unwrap();
    let reopened = fixture.call(
        libc::SYS_openat,
        [
            libc::AT_FDCWD as u64,
            0x100,
            (libc::O_WRONLY | libc::O_APPEND) as u64,
            0,
            0,
            0,
        ],
    );
    assert!(reopened >= 3);
    assert_eq!(
        fixture.call(
            libc::SYS_fcntl,
            [alias as u64, libc::F_SETFL as u64, 0, 0, 0, 0]
        ),
        0
    );
    assert_eq!(
        fixture.call(
            libc::SYS_fcntl,
            [reopened as u64, libc::F_GETFL as u64, 0, 0, 0, 0]
        ),
        i64::from(libc::O_WRONLY | libc::O_APPEND) | KERNEL_O_LARGEFILE,
        "proc-fd reopen must own independent captured-output status flags"
    );
    let reopened_info = fixture.info(reopened);
    assert_eq!(
        fixture.read(reopened_info, 4096),
        expected_record(
            i64::from(libc::O_WRONLY | libc::O_APPEND) | KERNEL_O_LARGEFILE,
            capture_identity::CAPTURE_STDOUT_INODE,
        )
    );

    fixture
        .memory
        .write(
            0x100,
            CString::new(format!("/proc/self/fd/{alias}"))
                .unwrap()
                .as_bytes_with_nul(),
        )
        .unwrap();
    let path_only = fixture.call(
        libc::SYS_openat,
        [
            libc::AT_FDCWD as u64,
            0x100,
            (libc::O_PATH | libc::O_WRONLY | libc::O_APPEND | libc::O_NONBLOCK) as u64,
            0,
            0,
            0,
        ],
    );
    assert!(path_only >= 3, "O_PATH capture reopen returned {path_only}");
    assert_eq!(
        fixture.call(
            libc::SYS_fcntl,
            [path_only as u64, libc::F_GETFL as u64, 0, 0, 0, 0]
        ),
        i64::from(libc::O_PATH),
        "O_PATH suppresses ordinary access and status flags"
    );
    assert_eq!(
        fixture.call(
            libc::SYS_fcntl,
            [path_only as u64, libc::F_SETFL as u64, 0, 0, 0, 0]
        ),
        negative_errno(libc::EBADF)
    );
    let path_info = fixture.info(path_only);
    assert_eq!(
        fixture.read(path_info, 4096),
        expected_record(
            i64::from(libc::O_PATH),
            capture_identity::CAPTURE_STDOUT_INODE,
        )
    );

    // An already-open fdinfo description follows the current entry in its
    // bound file table. Replacing stdout with stderr changes the next record
    // to stderr's virtual pipe identity rather than retaining a stale snapshot.
    let live_info = fixture.info(1);
    assert_eq!(fixture.call(libc::SYS_dup2, [2, 1, 0, 0, 0, 0]), 1);
    assert_eq!(
        fixture.read(live_info, 4096),
        expected_record(
            i64::from(libc::O_WRONLY),
            capture_identity::CAPTURE_STDERR_INODE,
        )
    );

    let mut noncapture = FdinfoFixture::new(false);
    noncapture
        .memory
        .write(0x100, b"/proc/self/fd/1\0")
        .unwrap();
    let ordinary_path = noncapture.call(
        libc::SYS_openat,
        [
            libc::AT_FDCWD as u64,
            0x100,
            (libc::O_PATH | libc::O_WRONLY | libc::O_NONBLOCK) as u64,
            0,
            0,
            0,
        ],
    );
    assert!(ordinary_path >= 3);
    assert!(!noncapture
        .executor
        .state
        .capture_status_flags
        .contains_key(&(ordinary_path as i32)));
    assert_eq!(
        noncapture.call(
            libc::SYS_fcntl,
            [ordinary_path as u64, libc::F_GETFL as u64, 0, 0, 0, 0]
        ),
        i64::from(libc::O_PATH)
    );
    assert_eq!(
        noncapture.call(
            libc::SYS_fcntl,
            [ordinary_path as u64, libc::F_SETFL as u64, 0, 0, 0, 0]
        ),
        negative_errno(libc::EBADF),
        "non-capture proc-fd reopen must retain host O_PATH behavior"
    );
}

#[test]
fn captured_output_status_ignores_ambient_and_closed_supervisor_stdout() {
    const TEST: &str =
        "executor::tests::captured_output_status_ignores_ambient_and_closed_supervisor_stdout";
    const COMPLETE: &str = "capture virtual status control completed";
    if !capture_test_child(TEST, COMPLETE) {
        return;
    }

    let saved_stdout = unsafe { libc::fcntl(1, libc::F_DUPFD_CLOEXEC, 3) };
    assert!(saved_stdout >= 3);
    let original_flags = unsafe { libc::fcntl(1, libc::F_GETFL) };
    assert!(original_flags >= 0);
    assert_eq!(
        unsafe {
            libc::fcntl(
                1,
                libc::F_SETFL,
                original_flags | libc::O_APPEND | libc::O_NONBLOCK,
            )
        },
        0
    );

    let root = TestDir::new();
    let mut executor = ElfExecutor::new(test_state(&root.0), true);
    let mut memory = GuestMemory::new(0, 0x4000).unwrap();
    assert_eq!(
        executor.execute(
            &SyscallRequest::new(
                libc::SYS_fcntl as u64,
                [1, libc::F_GETFL as u64, 0, 0, 0, 0],
            ),
            &memory,
        ),
        i64::from(libc::O_WRONLY),
        "ambient supervisor status bits must not seed captured stdout"
    );
    let generation = executor
        .state
        .task_lifecycle
        .lock()
        .unwrap()
        .get(executor.state.tid)
        .unwrap()
        .generation;
    let description = FdinfoDescription {
        target_tid: executor.state.tid,
        target_generation: generation,
        target_fd: libc::STDOUT_FILENO,
        lifecycle: executor.state.task_lifecycle.clone(),
        capture_output: true,
        path: b"/proc/1/fdinfo/1".to_vec(),
        nofollow_status: false,
        sequence: Mutex::default(),
    };
    assert_eq!(unsafe { libc::close(1) }, 0);
    assert_eq!(
        executor.execute(
            &SyscallRequest::new(
                libc::SYS_fcntl as u64,
                [1, libc::F_GETFL as u64, 0, 0, 0, 0],
            ),
            &memory,
        ),
        i64::from(libc::O_WRONLY),
        "closed supervisor stdout must not close virtual captured stdout"
    );
    assert_eq!(
        description.observe().unwrap(),
        format!(
            "pos:\t0\nflags:\t0{:o}\nmnt_id:\t{}\nino:\t{}\n",
            libc::O_WRONLY,
            SYNTHETIC_PIPE_MNT_ID,
            capture_identity::CAPTURE_STDOUT_INODE,
        )
        .into_bytes()
    );

    let mut reference_pipe = [-1; 2];
    assert_eq!(
        unsafe { libc::pipe2(reference_pipe.as_mut_ptr(), libc::O_CLOEXEC) },
        0
    );
    let mut expected_filesystem = capture_native_statfs(reference_pipe[0]);
    // KVM applies the same deterministic fsid normalization used for every
    // host-backed statfs result. Pipe capacity counts are already zero.
    expected_filesystem.f_fsid = unsafe { std::mem::zeroed() };
    assert_eq!(unsafe { libc::close(reference_pipe[0]) }, 0);
    assert_eq!(unsafe { libc::close(reference_pipe[1]) }, 0);

    assert_eq!(
        executor.execute(
            &SyscallRequest::new(libc::SYS_fstatfs as u64, [1, 0x1000, 0, 0, 0, 0]),
            &memory,
        ),
        0,
        "capture fstatfs must not consult closed supervisor stdout"
    );
    let direct: libc::statfs = read_struct(&memory, 0x1000);
    assert_eq!(statfs_bytes(&direct), statfs_bytes(&expected_filesystem));

    write_c_string(&mut memory, 0x100, "/proc/self/fd/1");
    assert_eq!(
        executor.execute(
            &SyscallRequest::new(libc::SYS_statfs as u64, [0x100, 0x1800, 0, 0, 0, 0]),
            &memory,
        ),
        0,
        "capture proc-fd statfs must not consult closed supervisor stdout"
    );
    let through_path: libc::statfs = read_struct(&memory, 0x1800);
    assert_eq!(statfs_bytes(&through_path), statfs_bytes(&direct));
    assert_eq!(
        executor.execute(
            &SyscallRequest::new(libc::SYS_fstatfs as u64, [1, 0x5000, 0, 0, 0, 0]),
            &memory,
        ),
        negative_errno(libc::EFAULT)
    );
    assert_eq!(
        executor.execute(
            &SyscallRequest::new(
                libc::SYS_fstatfs as u64,
                [GUEST_NOFILE_LIMIT as u64, 0x5000, 0, 0, 0, 0],
            ),
            &memory,
        ),
        negative_errno(libc::EBADF)
    );

    assert_eq!(
        unsafe { libc::fcntl(saved_stdout, libc::F_SETFL, original_flags) },
        0
    );
    assert_eq!(unsafe { libc::dup2(saved_stdout, 1) }, 1);
    assert_eq!(unsafe { libc::close(saved_stdout) }, 0);
    eprintln!("{COMPLETE}");
}

#[test]
fn captured_alias_creation_ignores_closed_and_reused_supervisor_stdio() {
    const TEST: &str =
        "executor::tests::captured_alias_creation_ignores_closed_and_reused_supervisor_stdio";
    const COMPLETE: &str = "capture descriptor carrier control completed";
    if !capture_test_child(TEST, COMPLETE) {
        return;
    }

    fn assert_physical_standard_closed() {
        for fd in [libc::STDOUT_FILENO, libc::STDERR_FILENO] {
            let target = std::fs::read_link(format!("/proc/self/fd/{fd}"));
            assert_eq!(
                unsafe { libc::fcntl(fd, libc::F_GETFD) },
                -1,
                "physical supervisor fd {fd} was reopened as {target:?}"
            );
            assert_eq!(
                std::io::Error::last_os_error().raw_os_error(),
                Some(libc::EBADF)
            );
        }
    }

    fn next_private_fd(anchor: libc::c_int) -> libc::c_int {
        let next = unsafe { libc::fcntl(anchor, libc::F_DUPFD_CLOEXEC, 3) };
        assert!(next >= 3);
        assert_eq!(unsafe { libc::close(next) }, 0);
        next
    }

    let saved = [libc::STDOUT_FILENO, libc::STDERR_FILENO].map(|fd| {
        let duplicate = unsafe { libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, 3) };
        assert!(duplicate >= 3, "captured dup returned {duplicate}");
        unsafe { std::os::fd::OwnedFd::from_raw_fd(duplicate) }
    });
    let closed_root = TestDir::new();
    let closed_state = test_state(&closed_root.0);
    let reused_root = TestDir::new();
    let reused_state = test_state(&reused_root.0);
    let plain_root = TestDir::new();
    let plain_state = test_state(&plain_root.0);
    let live_plain_root = TestDir::new();
    let live_plain_state = test_state(&live_plain_root.0);
    let mut memory = GuestMemory::new(0, 0x8000).unwrap();
    memory.write(0x3000, b"xy").unwrap();
    let one_byte = libc::iovec {
        iov_base: 0x3000_usize as *mut libc::c_void,
        iov_len: 1,
    };
    assert_eq!(write_struct(&mut memory, 0x3100, &one_byte), 0);
    write_c_string(&mut memory, 0x100, "/proc/self/fd/1");
    let mut live_plain = ElfExecutor::new(live_plain_state, false);
    for access in [libc::O_RDONLY, libc::O_RDWR] {
        let reopened = live_plain.execute(
            &SyscallRequest::new(
                libc::SYS_openat as u64,
                [
                    libc::AT_FDCWD as u64,
                    0x100,
                    (access | libc::O_NONBLOCK | libc::O_CLOEXEC) as u64,
                    0,
                    0,
                    0,
                ],
            ),
            &memory,
        );
        assert!(reopened >= 3, "noncapture proc reopen returned {reopened}");
        assert_eq!(
            live_plain.execute(
                &SyscallRequest::new(
                    libc::SYS_close as u64,
                    [reopened as u64, 0, 0, 0, 0, 0],
                ),
                &memory,
            ),
            0
        );
    }
    drop(live_plain);
    let epoll_host_fd = unsafe { libc::epoll_create1(libc::EPOLL_CLOEXEC) };
    assert!(epoll_host_fd >= 3);
    let epoll_host = unsafe { std::fs::File::from_raw_fd(epoll_host_fd) };
    for fd in [libc::STDOUT_FILENO, libc::STDERR_FILENO] {
        assert_eq!(unsafe { libc::close(fd) }, 0);
    }

    let mut plain = ElfExecutor::new(plain_state, false);
    assert_eq!(
        plain.execute(
            &SyscallRequest::new(libc::SYS_dup as u64, [1, 0, 0, 0, 0, 0]),
            &memory,
        ),
        negative_errno(libc::EBADF)
    );
    write_c_string(&mut memory, 0x100, "/proc/self/fd/1");
    for access in [
        libc::O_RDONLY,
        libc::O_WRONLY,
        libc::O_RDWR,
        libc::O_ACCMODE,
    ] {
        assert_eq!(
            plain.execute(
                &SyscallRequest::new(
                    libc::SYS_openat as u64,
                    [
                        libc::AT_FDCWD as u64,
                        0x100,
                        (access | libc::O_NONBLOCK) as u64,
                        0,
                        0,
                        0,
                    ],
                ),
                &memory,
            ),
            negative_errno(libc::ENOENT)
        );
    }
    drop(plain);
    assert_physical_standard_closed();

    let mut executor = ElfExecutor::new(closed_state, true);
    assert_physical_standard_closed();
    let epoll_fd = insert_file_with_flags(&mut executor.state, epoll_host, true, None) as i32;
    assert!(epoll_fd >= 3);
    executor
        .file_table
        .lock()
        .unwrap()
        .update_from_elf(&executor.state)
        .unwrap();
    let carriers = executor.output.as_ref().unwrap().identities.descriptors();
    assert!(carriers.into_iter().all(|fd| fd >= 3));
    let carrier_capacities = [carriers[1], carriers[3]].map(|fd| {
        let capacity = unsafe { libc::fcntl(fd, libc::F_GETPIPE_SZ) };
        assert!(capacity > 0);
        capacity
    });

    let mut survivors = Vec::new();
    for (index, source) in [libc::STDOUT_FILENO, libc::STDERR_FILENO]
        .into_iter()
        .enumerate()
    {
        let identity = capture_executor_stat(&mut executor, &memory, source);
        for command in [libc::F_GETPIPE_SZ, libc::F_SETPIPE_SZ] {
            assert_eq!(
                executor.execute(
                    &SyscallRequest::new(
                        libc::SYS_fcntl as u64,
                        [source as u64, command as u64, PAGE_SIZE, 0, 0, 0],
                    ),
                    &memory,
                ),
                negative_errno(libc::ENOSYS)
            );
        }
        assert_eq!(
            unsafe { libc::fcntl(carriers[index * 2 + 1], libc::F_GETPIPE_SZ) },
            carrier_capacities[index],
            "capture capacity refusal mutated the private carrier"
        );
        let requested = libc::pollfd {
            fd: source,
            events: libc::POLLIN | libc::POLLOUT,
            revents: libc::POLLERR,
        };
        assert_eq!(write_struct(&mut memory, 0x4000, &requested), 0);
        assert_eq!(
            executor.execute(
                &SyscallRequest::new(libc::SYS_poll as u64, [0x4000, 1, 0, 0, 0, 0]),
                &memory,
            ),
            1
        );
        let observed: libc::pollfd = read_struct(&memory, 0x4000);
        assert_eq!(observed.fd, source);
        assert_eq!(observed.events, requested.events);
        assert_eq!(observed.revents, libc::POLLOUT);
        assert_eq!(write_struct(&mut memory, 0x4000, &requested), 0);
        let zero_timespec = libc::timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        assert_eq!(write_struct(&mut memory, 0x4380, &zero_timespec), 0);
        assert_eq!(
            executor.execute(
                &SyscallRequest::new(libc::SYS_ppoll as u64, [0x4000, 1, 0x4380, 0, 0, 0]),
                &memory,
            ),
            1
        );
        assert_eq!(read_struct::<libc::pollfd>(&memory, 0x4000).revents, libc::POLLOUT);

        let source_bit = 1_u64 << source;
        memory.write(0x4100, &source_bit.to_ne_bytes()).unwrap();
        memory.write(0x4200, &source_bit.to_ne_bytes()).unwrap();
        let timeout = libc::timeval {
            tv_sec: 0,
            tv_usec: 0,
        };
        assert_eq!(write_struct(&mut memory, 0x4300, &timeout), 0);
        assert_eq!(
            executor.execute(
                &SyscallRequest::new(
                    libc::SYS_select as u64,
                    [source as u64 + 1, 0x4100, 0x4200, 0, 0x4300, 0],
                ),
                &memory,
            ),
            1
        );
        assert_eq!(read_struct::<u64>(&memory, 0x4100), 0);
        assert_eq!(read_struct::<u64>(&memory, 0x4200), source_bit);
        assert_eq!(read_struct::<libc::timeval>(&memory, 0x4300).tv_sec, 0);
        assert_eq!(read_struct::<libc::timeval>(&memory, 0x4300).tv_usec, 0);

        let event = libc::epoll_event {
            events: (libc::EPOLLIN | libc::EPOLLOUT) as u32,
            u64: 0x1357_9bdf_2468_ace0,
        };
        assert_eq!(write_struct(&mut memory, 0x4400, &event), 0);
        assert_eq!(
            executor.execute(
                &SyscallRequest::new(
                    libc::SYS_epoll_ctl as u64,
                    [
                        epoll_fd as u64,
                        libc::EPOLL_CTL_ADD as u64,
                        source as u64,
                        0x4400,
                        0,
                        0,
                    ],
                ),
                &memory,
            ),
            negative_errno(libc::ENOSYS)
        );
        assert_eq!(
            executor.execute(
                &SyscallRequest::new(
                    libc::SYS_epoll_wait as u64,
                    [epoll_fd as u64, 0x4500, 1, 0, 0, 0],
                ),
                &memory,
            ),
            0,
            "refused capture registration mutated the host epoll set"
        );
        assert_physical_standard_closed();

        let target = 100 + index as i32 * 20;
        let duplicate = executor.execute(
            &SyscallRequest::new(libc::SYS_dup as u64, [source as u64, 0, 0, 0, 0, 0]),
            &memory,
        ) as i32;
        assert!(duplicate >= 0, "source={source} captured dup returned {duplicate}");
        assert_eq!(
            executor.execute(
                &SyscallRequest::new(
                    libc::SYS_dup2 as u64,
                    [source as u64, target as u64, 0, 0, 0, 0],
                ),
                &memory,
            ),
            i64::from(target)
        );
        let dup3_target = target + 1;
        assert_eq!(
            executor.execute(
                &SyscallRequest::new(
                    libc::SYS_dup3 as u64,
                    [
                        source as u64,
                        dup3_target as u64,
                        libc::O_CLOEXEC as u64,
                        0,
                        0,
                        0,
                    ],
                ),
                &memory,
            ),
            i64::from(dup3_target)
        );
        assert_eq!(
            executor.execute(
                &SyscallRequest::new(
                    libc::SYS_dup2 as u64,
                    [source as u64, source as u64, 0, 0, 0, 0],
                ),
                &memory,
            ),
            i64::from(source)
        );
        assert_eq!(
            executor.execute(
                &SyscallRequest::new(
                    libc::SYS_dup3 as u64,
                    [source as u64, source as u64, 0, 0, 0, 0],
                ),
                &memory,
            ),
            negative_errno(libc::EINVAL)
        );
        let fcntl_duplicate = executor.execute(
            &SyscallRequest::new(
                libc::SYS_fcntl as u64,
                [
                    source as u64,
                    libc::F_DUPFD as u64,
                    (target + 5) as u64,
                    0,
                    0,
                    0,
                ],
            ),
            &memory,
        ) as i32;
        let fcntl_cloexec = executor.execute(
            &SyscallRequest::new(
                libc::SYS_fcntl as u64,
                [
                    source as u64,
                    libc::F_DUPFD_CLOEXEC as u64,
                    (target + 10) as u64,
                    0,
                    0,
                    0,
                ],
            ),
            &memory,
        ) as i32;
        assert!(fcntl_duplicate >= target + 5 && fcntl_cloexec >= target + 10);

        write_c_string(&mut memory, 0x100, &format!("/proc/self/fd/{source}"));
        let files_before = executor.state.files.len();
        let identities_before = executor.state.fd_entry_ids.len();
        let object_inodes_before = executor.state.fd_object_inodes.len();
        let next_inode_before = executor
            .state
            .file_identity_table
            .lock()
            .unwrap()
            .next_inode;
        // Captured bytes are not queued in the identity pipe, so read-capable
        // reopens deliberately fail closed rather than expose false EOF/EAGAIN.
        // The host validation probe rejects access mode 3 for this FIFO with
        // Linux's EINVAL before the capture-specific ENOSYS boundary.
        for access in [libc::O_RDONLY, libc::O_RDWR] {
            let next_host_fd_before = next_private_fd(saved[0].as_raw_fd());
            let probe = open_host_fd_path(
                carriers[index * 2 + 1],
                (access | libc::O_NONBLOCK | libc::O_CLOEXEC) as u64,
            )
            .unwrap();
            drop(probe);
            assert_eq!(
                next_private_fd(saved[0].as_raw_fd()),
                next_host_fd_before,
                "capture reopen helper retained a host descriptor"
            );
        }
        for (access, expected) in [
            (libc::O_RDONLY, libc::ENOSYS),
            (libc::O_RDWR, libc::ENOSYS),
            (libc::O_ACCMODE, libc::EINVAL),
        ] {
            assert_eq!(
                executor.execute(
                    &SyscallRequest::new(
                        libc::SYS_openat as u64,
                        [
                            libc::AT_FDCWD as u64,
                            0x100,
                            (access | libc::O_NONBLOCK | libc::O_CLOEXEC) as u64,
                            0,
                            0,
                            0,
                        ],
                    ),
                    &memory,
                ),
                negative_errno(expected)
            );
            assert_eq!(executor.state.files.len(), files_before);
            assert_eq!(executor.state.fd_entry_ids.len(), identities_before);
            assert_eq!(executor.state.fd_object_inodes.len(), object_inodes_before);
            assert_eq!(
                executor
                    .state
                    .file_identity_table
                    .lock()
                    .unwrap()
                    .next_inode,
                next_inode_before
            );
            assert_physical_standard_closed();
        }
        for (flags, expected) in [
            (libc::O_RDONLY | libc::O_DIRECTORY, libc::ENOTDIR),
            (libc::O_RDONLY | libc::O_NOFOLLOW, libc::ELOOP),
        ] {
            assert_eq!(
                executor.execute(
                    &SyscallRequest::new(
                        libc::SYS_openat as u64,
                        [libc::AT_FDCWD as u64, 0x100, flags as u64, 0, 0, 0],
                    ),
                    &memory,
                ),
                negative_errno(expected)
            );
            assert_eq!(executor.state.files.len(), files_before);
            assert_eq!(executor.state.fd_entry_ids.len(), identities_before);
            assert_eq!(executor.state.fd_object_inodes.len(), object_inodes_before);
            assert_physical_standard_closed();
        }
        let reopened = executor.execute(
            &SyscallRequest::new(
                libc::SYS_openat as u64,
                [
                    libc::AT_FDCWD as u64,
                    0x100,
                    (libc::O_WRONLY | libc::O_CLOEXEC) as u64,
                    0,
                    0,
                    0,
                ],
            ),
            &memory,
        ) as i32;
        let path_only = executor.execute(
            &SyscallRequest::new(
                libc::SYS_openat as u64,
                [
                    libc::AT_FDCWD as u64,
                    0x100,
                    (libc::O_PATH | libc::O_CLOEXEC) as u64,
                    0,
                    0,
                    0,
                ],
            ),
            &memory,
        ) as i32;
        assert!(reopened >= 3 && path_only >= 3);

        let shared_status = executor.state.capture_status_flags[&source].clone();
        for (fd, cloexec) in [
            (duplicate, false),
            (target, false),
            (dup3_target, true),
            (fcntl_duplicate, false),
            (fcntl_cloexec, true),
        ] {
            assert_eq!(capture_executor_stat(&mut executor, &memory, fd), identity);
            assert!(Arc::ptr_eq(
                &executor.state.capture_status_flags[&fd],
                &shared_status
            ));
            assert_eq!(
                executor.execute(
                    &SyscallRequest::new(
                        libc::SYS_fcntl as u64,
                        [fd as u64, libc::F_GETFD as u64, 0, 0, 0, 0],
                    ),
                    &memory,
                ),
                if cloexec {
                    i64::from(libc::FD_CLOEXEC)
                } else {
                    0
                }
            );
            let host = executor.state.files[&fd].as_raw_fd();
            assert!(host >= 3);
            assert_eq!(
                unsafe { libc::fcntl(host, libc::F_GETFL) } & libc::O_ACCMODE,
                libc::O_WRONLY
            );
            assert_eq!(
                executor.execute(
                    &SyscallRequest::new(
                        libc::SYS_read as u64,
                        [fd as u64, 0x3000, 1, 0, 0, 0],
                    ),
                    &memory,
                ),
                negative_errno(libc::EBADF)
            );
            for command in [libc::F_GETPIPE_SZ, libc::F_SETPIPE_SZ] {
                assert_eq!(
                    executor.execute(
                        &SyscallRequest::new(
                            libc::SYS_fcntl as u64,
                            [fd as u64, command as u64, PAGE_SIZE, 0, 0, 0],
                        ),
                        &memory,
                    ),
                    negative_errno(libc::ENOSYS)
                );
            }
            assert_eq!(
                executor.execute(
                    &SyscallRequest::new(libc::SYS_lseek as u64, [fd as u64, 0, 0, 0, 0, 0]),
                    &memory,
                ),
                negative_errno(libc::ESPIPE)
            );
            assert_physical_standard_closed();
        }
        assert!(!Arc::ptr_eq(
            &executor.state.capture_status_flags[&reopened],
            &shared_status
        ));
        assert_eq!(
            executor.state.capture_status_flags[&reopened].load(Ordering::SeqCst)
                & libc::O_ACCMODE,
            libc::O_WRONLY
        );
        assert_eq!(
            executor.state.capture_status_flags[&path_only].load(Ordering::SeqCst),
            libc::O_PATH
        );
        for command in [libc::F_GETPIPE_SZ, libc::F_SETPIPE_SZ] {
            assert_eq!(
                executor.execute(
                    &SyscallRequest::new(
                        libc::SYS_fcntl as u64,
                        [reopened as u64, command as u64, PAGE_SIZE, 0, 0, 0],
                    ),
                    &memory,
                ),
                negative_errno(libc::ENOSYS)
            );
            assert_eq!(
                executor.execute(
                    &SyscallRequest::new(
                        libc::SYS_fcntl as u64,
                        [path_only as u64, command as u64, PAGE_SIZE, 0, 0, 0],
                    ),
                    &memory,
                ),
                negative_errno(libc::EBADF)
            );
        }
        for fd in [reopened, path_only] {
            assert_eq!(capture_executor_stat(&mut executor, &memory, fd), identity);
            assert!(executor.state.files[&fd].as_raw_fd() >= 3);
            assert_eq!(
                executor.execute(
                    &SyscallRequest::new(
                        libc::SYS_read as u64,
                        [fd as u64, 0x3000, 1, 0, 0, 0],
                    ),
                    &memory,
                ),
                negative_errno(libc::EBADF)
            );
            assert_physical_standard_closed();
        }
        for number in [libc::SYS_writev, libc::SYS_pwritev2] {
            assert_eq!(
                executor.execute(
                    &SyscallRequest::new(
                        number as u64,
                        [path_only as u64, 0x3100, 1, u64::MAX, 0, 0],
                    ),
                    &memory,
                ),
                negative_errno(libc::EBADF)
            );
        }
        assert_eq!(
            executor.execute(
                &SyscallRequest::new(
                    libc::SYS_lseek as u64,
                    [path_only as u64, 0, libc::SEEK_SET as u64, 0, 0, 0],
                ),
                &memory,
            ),
            negative_errno(libc::EBADF)
        );
        assert_physical_standard_closed();

        assert_eq!(
            executor.execute(
                &SyscallRequest::new(libc::SYS_close as u64, [source as u64, 0, 0, 0, 0, 0]),
                &memory,
            ),
            0
        );
        assert_eq!(
            executor.execute(
                &SyscallRequest::new(libc::SYS_dup as u64, [source as u64, 0, 0, 0, 0, 0]),
                &memory,
            ),
            negative_errno(libc::EBADF)
        );
        assert_eq!(
            executor.execute(
                &SyscallRequest::new(
                    libc::SYS_fcntl as u64,
                    [source as u64, libc::F_DUPFD as u64, 0, 0, 0, 0],
                ),
                &memory,
            ),
            negative_errno(libc::EBADF)
        );
        assert_eq!(
            executor.execute(
                &SyscallRequest::new(
                    libc::SYS_openat as u64,
                    [
                        libc::AT_FDCWD as u64,
                        0x100,
                        libc::O_WRONLY as u64,
                        0,
                        0,
                        0,
                    ],
                ),
                &memory,
            ),
            negative_errno(libc::ENOENT)
        );
        assert_eq!(capture_executor_stat(&mut executor, &memory, duplicate), identity);
        survivors.push((duplicate, source));
        assert_physical_standard_closed();
    }
    assert_eq!(
        executor.execute(
            &SyscallRequest::new(
                libc::SYS_close as u64,
                [epoll_fd as u64, 0, 0, 0, 0, 0],
            ),
            &memory,
        ),
        0
    );
    let snapshot = FileTableState::try_from_elf(&executor.state).unwrap();
    assert!(snapshot.files.values().all(|file| file.as_raw_fd() >= 3));
    assert_physical_standard_closed();
    drop(snapshot);
    for (fd, source) in survivors {
        for command in [libc::F_GETPIPE_SZ, libc::F_SETPIPE_SZ] {
            assert_eq!(
                executor.execute(
                    &SyscallRequest::new(
                        libc::SYS_fcntl as u64,
                        [fd as u64, command as u64, PAGE_SIZE, 0, 0, 0],
                    ),
                    &memory,
                ),
                negative_errno(libc::ENOSYS),
                "capture capacity became host-backed after closing its source"
            );
        }
        assert_eq!(
            executor.execute(
                &SyscallRequest::new(libc::SYS_write as u64, [fd as u64, 0x3000, 1, 0, 0, 0]),
                &memory,
            ),
            1
        );
        assert_physical_standard_closed();
        assert!(matches!(source, libc::STDOUT_FILENO | libc::STDERR_FILENO));
    }
    assert_eq!(executor.take_output(), (b"x".to_vec(), b"x".to_vec()));
    drop(executor);
    assert_physical_standard_closed();

    let sentinel_bytes = [b"stdout-sentinel", b"stderr-sentinel"];
    let mut sentinels = Vec::new();
    let mut snapshots = Vec::new();
    for (index, expected_fd) in [libc::STDOUT_FILENO, libc::STDERR_FILENO]
        .into_iter()
        .enumerate()
    {
        let raw = unsafe { libc::memfd_create(c"capture-sentinel".as_ptr(), libc::MFD_CLOEXEC) };
        assert_eq!(raw, expected_fd);
        assert_eq!(
            unsafe {
                libc::write(
                    raw,
                    sentinel_bytes[index].as_ptr().cast(),
                    sentinel_bytes[index].len(),
                )
            },
            sentinel_bytes[index].len() as isize
        );
        assert_eq!(unsafe { libc::lseek(raw, 1, libc::SEEK_SET) }, 1);
        let flags = unsafe { libc::fcntl(raw, libc::F_GETFL) };
        assert!(flags >= 0);
        assert_eq!(
            unsafe { libc::fcntl(raw, libc::F_SETFL, flags | libc::O_APPEND | libc::O_NONBLOCK) },
            0
        );
        let metadata = capture_native_stat(raw);
        snapshots.push((
            metadata.st_dev,
            metadata.st_ino,
            flags | libc::O_APPEND | libc::O_NONBLOCK,
            metadata.st_size,
            metadata.st_blocks,
            unsafe { libc::fcntl(raw, libc::F_GETFD) },
        ));
        sentinels.push(unsafe { std::fs::File::from_raw_fd(raw) });
    }

    let mut reused = ElfExecutor::new(reused_state, true);
    let mut reused_aliases = Vec::new();
    for (index, source) in [libc::STDOUT_FILENO, libc::STDERR_FILENO]
        .into_iter()
        .enumerate()
    {
        let duplicate = reused.execute(
            &SyscallRequest::new(libc::SYS_dup as u64, [source as u64, 0, 0, 0, 0, 0]),
            &memory,
        ) as i32;
        assert!(duplicate >= 3);
        write_c_string(&mut memory, 0x100, &format!("/proc/self/fd/{source}"));
        let reopened = reused.execute(
            &SyscallRequest::new(
                libc::SYS_openat as u64,
                [
                    libc::AT_FDCWD as u64,
                    0x100,
                    libc::O_WRONLY as u64,
                    0,
                    0,
                    0,
                ],
            ),
            &memory,
        ) as i32;
        assert!(reopened >= 3);
        let high_source = (1_u64 << 32) | source as u64;
        for (number, args, expected) in [
            (
                libc::SYS_ftruncate,
                [high_source, 0, 0, 0, 0, 0],
                negative_errno(libc::EINVAL),
            ),
            (
                libc::SYS_fallocate,
                [high_source, 0, 0, 4096, 0, 0],
                negative_errno(libc::ESPIPE),
            ),
            (
                libc::SYS_fsync,
                [high_source, 0, 0, 0, 0, 0],
                negative_errno(libc::EINVAL),
            ),
            (
                libc::SYS_fdatasync,
                [high_source, 0, 0, 0, 0, 0],
                negative_errno(libc::EINVAL),
            ),
            (
                libc::SYS_readahead,
                [high_source, 0, 1, 0, 0, 0],
                negative_errno(libc::EBADF),
            ),
            (
                libc::SYS_sync_file_range,
                [
                    high_source,
                    0,
                    4096,
                    libc::SYNC_FILE_RANGE_WRITE as u64,
                    0,
                    0,
                ],
                negative_errno(libc::ESPIPE),
            ),
        ] {
            assert_eq!(
                reused.execute(&SyscallRequest::new(number as u64, args), &memory),
                expected,
                "captured fd {source} syscall {number} reached its physical memfd"
            );
        }
        memory.write(0x3400, &0_i32.to_ne_bytes()).unwrap();
        assert_eq!(
            reused.execute(
                &SyscallRequest::new(
                    libc::SYS_ioctl as u64,
                    [
                        high_source,
                        (1_u64 << 32) | libc::FIONBIO,
                        0x3400,
                        0,
                        0,
                        0,
                    ],
                ),
                &memory,
            ),
            0
        );
        assert_eq!(
            reused.execute(
                &SyscallRequest::new(
                    libc::SYS_ioctl as u64,
                    [high_source, (1_u64 << 32) | libc::FIONCLEX, 0, 0, 0, 0],
                ),
                &memory,
            ),
            0
        );
        for fd in [duplicate, reopened] {
            let host = reused.state.files[&fd].as_raw_fd();
            assert!(host >= 3);
            assert_eq!(
                unsafe { libc::fcntl(host, libc::F_GETFL) } & libc::O_ACCMODE,
                libc::O_WRONLY
            );
            let backing = capture_native_stat(host);
            assert_ne!((backing.st_dev, backing.st_ino), (snapshots[index].0, snapshots[index].1));
            assert_eq!(
                reused.execute(
                    &SyscallRequest::new(
                        libc::SYS_read as u64,
                        [fd as u64, 0x3000, 1, 0, 0, 0],
                    ),
                    &memory,
                ),
                negative_errno(libc::EBADF)
            );
        }
        let mut bytes = vec![0; sentinel_bytes[index].len()];
        assert_eq!(
            unsafe {
                libc::pread(
                    source,
                    bytes.as_mut_ptr().cast(),
                    bytes.len(),
                    0,
                )
            },
            bytes.len() as isize
        );
        assert_eq!(bytes, sentinel_bytes[index]);
        assert_eq!(unsafe { libc::lseek(source, 0, libc::SEEK_CUR) }, 1);
        assert_eq!(unsafe { libc::fcntl(source, libc::F_GETFL) }, snapshots[index].2);
        let metadata = capture_native_stat(source);
        assert_eq!(
            (
                metadata.st_dev,
                metadata.st_ino,
                metadata.st_size,
                metadata.st_blocks,
                unsafe { libc::fcntl(source, libc::F_GETFD) },
            ),
            (
                snapshots[index].0,
                snapshots[index].1,
                snapshots[index].3,
                snapshots[index].4,
                snapshots[index].5,
            ),
            "captured operations changed physical supervisor fd {source}"
        );
        reused_aliases.push((duplicate, source));
    }
    memory.write(0x3000, b"yz").unwrap();
    for (index, (fd, source)) in reused_aliases.into_iter().enumerate() {
        assert_eq!(
            reused.execute(
                &SyscallRequest::new(
                    libc::SYS_write as u64,
                    [fd as u64, 0x3000 + index as u64, 1, 0, 0, 0],
                ),
                &memory,
            ),
            1
        );
        assert!(matches!(source, libc::STDOUT_FILENO | libc::STDERR_FILENO));
    }
    assert_eq!(reused.take_output(), (b"y".to_vec(), b"z".to_vec()));
    drop(reused);
    drop(sentinels);
    for (fd, saved) in [libc::STDOUT_FILENO, libc::STDERR_FILENO]
        .into_iter()
        .zip(&saved)
    {
        assert_eq!(unsafe { libc::dup2(saved.as_raw_fd(), fd) }, fd);
    }
    eprintln!("{COMPLETE}");
}

#[test]
fn missing_captured_status_is_a_typed_backend_failure() {
    let root = TestDir::new();
    let mut executor = ElfExecutor::new(test_state(&root.0), true);
    let mut memory = GuestMemory::new(0, 0x1000).unwrap();
    memory.write(0x100, &1_i32.to_ne_bytes()).unwrap();
    let capture = executor.output.as_ref().unwrap().metadata();
    let carrier = capture.descriptor_carrier(OutputAlias::Stdout);
    let original_flags = unsafe { libc::fcntl(carrier, libc::F_GETFL) };
    assert!(original_flags >= 0);

    executor.state.capture_status_flags.remove(&libc::STDOUT_FILENO);
    executor
        .file_table
        .lock()
        .unwrap()
        .capture_status_flags
        .remove(&libc::STDOUT_FILENO);
    for request in [
        SyscallRequest::new(
            libc::SYS_ioctl as u64,
            [
                libc::STDOUT_FILENO as u64,
                libc::FIONBIO,
                0x100,
                0,
                0,
                0,
            ],
        ),
        SyscallRequest::new(
            libc::SYS_fcntl as u64,
            [
                libc::STDOUT_FILENO as u64,
                libc::F_SETFL as u64,
                libc::O_NONBLOCK as u64,
                0,
                0,
                0,
            ],
        ),
        // accept4 validates flags and returns before ordinary scalar dispatch;
        // authoritative capture validation must still win without a socket.
        SyscallRequest::new(
            libc::SYS_accept4 as u64,
            [u64::MAX, 0, 0, u64::MAX, 0, 0],
        ),
    ] {
        let error = executor
            .execute_checked(&request, &memory)
            .expect_err("missing captured status must terminate the backend call");
        assert!(matches!(
            error,
            crate::Error::CapturedOutputStatusMissing(libc::STDOUT_FILENO)
        ));
    }
    assert_eq!(unsafe { libc::fcntl(carrier, libc::F_GETFL) }, original_flags);
}

#[test]
fn captured_output_alias_identity_survives_thread_fork_exec_and_replacement() {
    const HIGH_WORD: u64 = 0x5a5a_5a5a_0000_0000;
    const CLOSE_RANGE_CLOEXEC: u64 = 1 << 2;
    let root = TestDir::new();
    let owner = CapturedOutput::try_new().unwrap();
    let mut state = test_state(&root.0);
    let regular = std::fs::File::open("/dev/null").unwrap();
    let raw = capture_native_stat(regular.as_raw_fd());
    let ordinary = insert_file_with_flags(&mut state, regular, false, None) as i32;
    assert!(ordinary >= 3);
    let mut parent = ElfExecutor::with_output(state, Some(owner.clone()));
    let mut memory = GuestMemory::new(0, 0x4000).unwrap();
    let stdout = capture_executor_stat(&mut parent, &memory, 1);
    let stderr = capture_executor_stat(&mut parent, &memory, 2);
    assert_eq!(stdout.0, stderr.0);
    assert_ne!(stdout.1, stderr.1);
    let next_dynamic_inode = parent
        .state
        .file_identity_table
        .lock()
        .unwrap()
        .next_inode;
    let alias = parent.execute(
        &SyscallRequest::new(libc::SYS_dup as u64, [1, 0, 0, 0, 0, 0]),
        &memory,
    ) as i32;
    assert!(alias >= 3);
    assert_eq!(
        parent.state.file_identity_table.lock().unwrap().next_inode,
        next_dynamic_inode,
        "captured dup must not consume a dynamic object identity"
    );
    let alias_object = parent.state.fd_object_inodes[&alias].clone();
    let stdout_status = parent.state.capture_status_flags[&1].clone();
    assert!(Arc::ptr_eq(
        &parent.state.capture_status_flags[&alias],
        &stdout_status
    ));
    let fcntl_alias = parent.execute(
        &SyscallRequest::new(
            libc::SYS_fcntl as u64,
            [alias as u64, libc::F_DUPFD as u64, 20, 0, 0, 0],
        ),
        &memory,
    ) as i32;
    assert!(fcntl_alias >= 20);
    assert!(Arc::ptr_eq(
        &parent.state.fd_object_inodes[&fcntl_alias],
        &alias_object
    ));
    assert!(Arc::ptr_eq(
        &parent.state.capture_status_flags[&fcntl_alias],
        &stdout_status
    ));
    assert_eq!(
        parent.state.file_identity_table.lock().unwrap().next_inode,
        next_dynamic_inode,
        "captured F_DUPFD must not consume a dynamic object identity"
    );
    write_c_string(&mut memory, 0x100, &format!("/proc/self/fd/{alias}"));
    let reopened = parent.execute(
        &SyscallRequest::new(
            libc::SYS_open as u64,
            [0x100, libc::O_WRONLY as u64, 0, 0, 0, 0],
        ),
        &memory,
    ) as i32;
    assert!(reopened >= 3);
    assert!(Arc::ptr_eq(
        &parent.state.fd_object_inodes[&reopened],
        &alias_object
    ));
    assert!(!Arc::ptr_eq(
        &parent.state.capture_status_flags[&reopened],
        &stdout_status
    ));
    assert_eq!(
        parent.state.file_identity_table.lock().unwrap().next_inode,
        next_dynamic_inode,
        "captured proc-fd reopen must not consume a dynamic object identity"
    );
    for fd in [alias, fcntl_alias, reopened] {
        assert_eq!(capture_executor_stat(&mut parent, &memory, fd), stdout);
    }
    assert_eq!(
        parent.execute(
            &SyscallRequest::new(libc::SYS_pipe2 as u64, [0x3000, 0, 0, 0, 0, 0]),
            &memory,
        ),
        0
    );
    let pipe: [i32; 2] = read_struct(&memory, 0x3000);
    let pipe_identity = parent.state.fd_object_inodes[&pipe[0]].clone();
    assert!(Arc::ptr_eq(
        &pipe_identity,
        &parent.state.fd_object_inodes[&pipe[1]]
    ));
    assert_eq!(pipe_identity.kind, GuestFileIdentityKind::Pipe);
    assert_eq!(pipe_identity.inode, next_dynamic_inode);
    assert_eq!(
        parent.state.file_identity_table.lock().unwrap().next_inode,
        next_dynamic_inode + 1
    );
    for fd in pipe {
        assert_eq!(
            parent.execute(
                &SyscallRequest::new(libc::SYS_close as u64, [fd as u64, 0, 0, 0, 0, 0]),
                &memory,
            ),
            0
        );
    }
    assert_eq!(
        parent.state.file_identity_table.lock().unwrap().next_inode,
        next_dynamic_inode + 1,
        "object retirement must not roll back the deterministic allocator"
    );
    let mut sibling = parent.thread_child(2).unwrap();
    assert_eq!(
        sibling.execute(
            &SyscallRequest::new(libc::SYS_dup2 as u64, [2, 1, 0, 0, 0, 0]),
            &memory
        ),
        1
    );
    assert_eq!(capture_executor_stat(&mut parent, &memory, 1), stderr);
    assert_eq!(capture_executor_stat(&mut parent, &memory, alias), stdout);
    let mut child = parent.fork_child(3, false, false).unwrap();
    assert!(Arc::ptr_eq(
        &child.state.capture_status_flags[&alias],
        &stdout_status
    ));

    // Forked processes retain one open file description but do not share a
    // file-table lock. Concurrent F_SETFL and FIONBIO therefore have to update
    // the virtual status atomically. Every valid serialization keeps O_APPEND;
    // a split load/store can lose it when FIONBIO publishes last.
    let start = Arc::new(std::sync::Barrier::new(3));
    let finish = Arc::new(std::sync::Barrier::new(3));
    let iterations = 2_000;
    std::thread::scope(|scope| {
        let parent_start = start.clone();
        let parent_finish = finish.clone();
        let parent_executor = &mut parent;
        let parent_worker = scope.spawn(move || {
            let parent_memory = GuestMemory::new(0, 0x1000).unwrap();
            for _ in 0..iterations {
                parent_start.wait();
                assert_eq!(
                    parent_executor.execute(
                        &SyscallRequest::new(
                            libc::SYS_fcntl as u64,
                            [
                                alias as u64,
                                libc::F_SETFL as u64,
                                libc::O_APPEND as u64,
                                0,
                                0,
                                0,
                            ],
                        ),
                        &parent_memory,
                    ),
                    0
                );
                parent_finish.wait();
            }
        });
        let child_start = start.clone();
        let child_finish = finish.clone();
        let child_executor = &mut child;
        let child_worker = scope.spawn(move || {
            let mut child_memory = GuestMemory::new(0, 0x1000).unwrap();
            child_memory.write(0x100, &1_i32.to_ne_bytes()).unwrap();
            for _ in 0..iterations {
                child_start.wait();
                assert_eq!(
                    child_executor.execute(
                        &SyscallRequest::new(
                            libc::SYS_ioctl as u64,
                            [alias as u64, libc::FIONBIO, 0x100, 0, 0, 0],
                        ),
                        &child_memory,
                    ),
                    0
                );
                child_finish.wait();
            }
        });
        for _ in 0..iterations {
            stdout_status.store(libc::O_WRONLY, Ordering::SeqCst);
            start.wait();
            finish.wait();
            assert_ne!(
                stdout_status.load(Ordering::SeqCst) & libc::O_APPEND,
                0,
                "concurrent FIONBIO lost the serialized F_SETFL update"
            );
        }
        parent_worker.join().unwrap();
        child_worker.join().unwrap();
    });
    stdout_status.store(libc::O_WRONLY, Ordering::SeqCst);

    assert_eq!(
        child.execute(
            &SyscallRequest::new(
                libc::SYS_close_range as u64,
                [
                    HIGH_WORD | fcntl_alias as u64,
                    HIGH_WORD | fcntl_alias as u64,
                    HIGH_WORD | CLOSE_RANGE_CLOEXEC,
                    0,
                    0,
                    0,
                ],
            ),
            &memory,
        ),
        0
    );
    assert!(child.state.files.contains_key(&fcntl_alias));
    assert!(child.state.cloexec_fds.contains(&fcntl_alias));
    assert!(output_alias(&child.state, fcntl_alias).is_some());
    assert!(child.state.capture_status_flags.contains_key(&fcntl_alias));
    assert!(child.state.fd_object_inodes.contains_key(&fcntl_alias));
    assert!(!child.state.cloexec_fds.contains(&alias));

    let replacement = test_exec_replacement(&root.0, &child.state);
    child.replace_after_exec(replacement);
    assert!(!child.state.files.contains_key(&fcntl_alias));
    assert!(!child.state.cloexec_fds.contains(&fcntl_alias));
    assert!(output_alias(&child.state, fcntl_alias).is_none());
    assert!(!child.state.capture_status_flags.contains_key(&fcntl_alias));
    assert!(!child.state.fd_object_inodes.contains_key(&fcntl_alias));
    assert!(Arc::ptr_eq(
        &child.state.capture_status_flags[&alias],
        &stdout_status
    ));
    assert_eq!(capture_executor_stat(&mut child, &memory, alias), stdout);
    assert_eq!(capture_executor_stat(&mut child, &memory, 1), stderr);
    assert_eq!(
        parent.execute(
            &SyscallRequest::new(
                libc::SYS_close as u64,
                [HIGH_WORD | alias as u64, 0, 0, 0, 0, 0],
            ),
            &memory
        ),
        0
    );
    assert_eq!(
        parent.execute(
            &SyscallRequest::new(
                libc::SYS_dup2 as u64,
                [ordinary as u64, alias as u64, 0, 0, 0, 0]
            ),
            &memory
        ),
        i64::from(alias)
    );
    assert_eq!(
        capture_executor_stat(&mut parent, &memory, alias),
        (raw.st_dev, raw.st_ino)
    );
    assert!(!parent.state.capture_status_flags.contains_key(&alias));
    assert_eq!(
        capture_executor_stat(&mut sibling, &memory, alias),
        (raw.st_dev, raw.st_ino)
    );
    assert_eq!(
        capture_executor_stat(&mut child, &memory, alias),
        stdout,
        "private fork retains its old capture alias"
    );
    assert!(Arc::ptr_eq(
        &child.state.capture_status_flags[&alias],
        &stdout_status
    ));
    memory.write(0x200, b"a").unwrap();
    assert_eq!(
        child.execute(
            &SyscallRequest::new(libc::SYS_write as u64, [alias as u64, 0x200, 1, 0, 0, 0]),
            &memory
        ),
        1
    );
    assert_eq!(parent.take_output(), (b"a".to_vec(), Vec::new()));
    assert_eq!(
        capture_executor_stat(&mut child, &memory, alias),
        stdout,
        "draining output does not retire stream identity"
    );
}

fn capture_test_child(test: &str, marker: &str) -> bool {
    const ENV: &str = "REVERIE_CAPTURE_IDENTITY_CHILD";
    if std::env::var(ENV).as_deref() == Ok(test) {
        return true;
    }
    assert!(std::env::var_os(ENV).is_none());
    let output = std::process::Command::new("/usr/bin/timeout")
        .args(["--kill-after=2s", "10s"])
        .arg(std::env::current_exe().unwrap())
        .args(["--exact", test, "--test-threads=1", "--nocapture"])
        .env(ENV, test)
        .output()
        .unwrap();
    eprintln!(
        "capture child status={}\nstdout:\n{}\nstderr:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.status.success());
    assert_eq!(
        String::from_utf8_lossy(&output.stderr)
            .lines()
            .filter(|line| *line == marker)
            .count(),
        1
    );
    false
}

#[test]
fn capture_identity_keeps_closed_standard_descriptors_closed() {
    const TEST: &str = "executor::tests::capture_identity_keeps_closed_standard_descriptors_closed";
    const COMPLETE: &str = "capture closed standard descriptors completed";
    if !capture_test_child(TEST, COMPLETE) {
        return;
    }
    // Retain the child's real streams outside 0..2, then restore them before
    // reporting assertions. No parent or parallel libtest descriptor is touched.
    let saved = [0, 1, 2].map(|fd| {
        let duplicate = unsafe { libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, 3) };
        assert!(duplicate >= 3);
        unsafe { std::os::fd::OwnedFd::from_raw_fd(duplicate) }
    });
    for fd in [0, 1, 2] {
        assert_eq!(unsafe { libc::close(fd) }, 0);
    }
    let prepared = CapturedOutput::try_new();
    let closed = [0, 1, 2].map(|fd| unsafe { libc::fcntl(fd, libc::F_GETFD) } == -1
        && std::io::Error::last_os_error().raw_os_error() == Some(libc::EBADF));
    for (fd, saved) in [0, 1, 2].into_iter().zip(&saved) {
        assert_eq!(unsafe { libc::dup2(saved.as_raw_fd(), fd) }, fd);
    }
    let owner = prepared.unwrap();
    assert_eq!(closed, [true; 3]);
    let descriptors = owner.identities.descriptors();
    for (index, fd) in descriptors.into_iter().enumerate() {
        assert!(fd >= 3);
        assert_eq!(
            capture_native_stat(fd).st_mode & libc::S_IFMT,
            libc::S_IFIFO
        );
        assert_eq!(unsafe { libc::fcntl(fd, libc::F_GETFD) }, libc::FD_CLOEXEC);
        assert_eq!(
            unsafe { libc::fcntl(fd, libc::F_GETFL) } & libc::O_ACCMODE,
            if index % 2 == 0 {
                libc::O_RDONLY
            } else {
                libc::O_WRONLY
            }
        );
    }
    let metadata = descriptors.map(capture_native_stat);
    for pair in metadata.as_chunks::<2>().0 {
        assert_eq!(
            (pair[0].st_dev, pair[0].st_ino),
            (pair[1].st_dev, pair[1].st_ino)
        );
    }
    assert_ne!(
        (metadata[0].st_dev, metadata[0].st_ino),
        (metadata[2].st_dev, metadata[2].st_ino)
    );
    for fd in [descriptors[1], descriptors[3]] {
        let mut poll = libc::pollfd {
            fd,
            events: libc::POLLOUT,
            revents: 0,
        };
        assert_eq!(unsafe { libc::poll(&mut poll, 1, 0) }, 1);
        assert_eq!(poll.revents, libc::POLLOUT);
    }
    eprintln!("{COMPLETE}");
}

#[test]
fn captured_pipe_root_owner_releases_after_executor_cleanup_and_unwind() {
    const TEST: &str =
        "executor::tests::captured_pipe_root_owner_releases_after_executor_cleanup_and_unwind";
    const COMPLETE: &str = "capture final owner cleanup completed";
    if !capture_test_child(TEST, COMPLETE) {
        return;
    }
    for unwind in [false, true] {
        let root = TestDir::new();
        let owner = CapturedOutput::try_new().unwrap();
        let descriptors = owner.identities.descriptors();
        let metadata = descriptors.map(capture_native_stat);
        let weak = Arc::downgrade(&owner.identities);
        let mut parent = ElfExecutor::with_output(test_state(&root.0), Some(owner.clone()));
        let child = parent.thread_child(2).unwrap();
        let files = parent.file_table.clone();
        let transaction = parent.state.signal_transaction.clone();
        let observed = Arc::new(AtomicBool::new(false));
        let observed_by_drop = observed.clone();
        *owner.identities.drop_probe.lock().unwrap() = Some(Box::new(move |actual| {
            let _files = files
                .try_lock()
                .expect("final capture close held the file-table guard");
            let _transaction = transaction
                .try_lock()
                .expect("final capture close held the signal guard");
            assert_eq!(actual, descriptors);
            for (fd, expected) in actual.into_iter().zip(metadata) {
                let live = capture_native_stat(fd);
                assert_eq!(
                    (live.st_dev, live.st_ino),
                    (expected.st_dev, expected.st_ino)
                );
            }
            observed_by_drop.store(true, Ordering::SeqCst);
        }));
        assert_eq!(parent.take_output(), (Vec::new(), Vec::new()));
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
            let _parent = parent;
            let _child = child;
            if unwind {
                panic!("intentional capture cleanup unwind");
            }
        }));
        assert_eq!(result.is_err(), unwind);
        assert!(!observed.load(Ordering::SeqCst));
        assert!(weak.upgrade().is_some());
        for fd in descriptors {
            assert_eq!(
                capture_native_stat(fd).st_mode & libc::S_IFMT,
                libc::S_IFIFO
            );
        }
        drop(owner);
        assert!(observed.load(Ordering::SeqCst));
        assert!(weak.upgrade().is_none());
        for fd in descriptors {
            assert_eq!(unsafe { libc::fcntl(fd, libc::F_GETFD) }, -1);
            assert_eq!(
                std::io::Error::last_os_error().raw_os_error(),
                Some(libc::EBADF)
            );
        }
    }
    eprintln!("{COMPLETE}");
}

#[derive(Debug, Default)]
struct CaptureSetupMustNotInitialize;

#[reverie::global_tool]
impl reverie::GlobalTool for CaptureSetupMustNotInitialize {
    type Request = ();
    type Response = ();
    type Config = ();
    async fn init_global_state(_: &()) -> Self {
        panic!("failed capture setup initialized the Tool");
    }
    async fn receive_rpc(&self, _: reverie::Tid, _: ()) {
        panic!("failed capture setup dispatched an RPC");
    }
}

#[derive(Debug, Default)]
struct CaptureSetupTool;

#[reverie::tool]
impl reverie::Tool for CaptureSetupTool {
    type GlobalState = CaptureSetupMustNotInitialize;
    type ThreadState = ();
}

#[test]
fn capture_setup_emfile_preserves_image_and_closes_partial_streams() {
    const TEST: &str =
        "executor::tests::capture_setup_emfile_preserves_image_and_closes_partial_streams";
    const COMPLETE: &str = "capture setup EMFILE control completed";
    let mut backend = match crate::KvmBackend::new(16 * 1024 * 1024) {
        Ok(backend) => backend,
        Err(crate::Error::Kvm(error))
            if matches!(error.errno(), libc::ENOENT | libc::EACCES | libc::EPERM) =>
        {
            assert!(
                std::env::var_os("REVERIE_REQUIRE_KVM").is_none(),
                "capture setup control requires KVM: {error}"
            );
            eprintln!("capture setup control not executed: KVM unavailable: {error}");
            return;
        }
        Err(error) => panic!("capture setup failed: {error}"),
    };
    if !capture_test_child(TEST, COMPLETE) {
        return;
    }
    let root = TestDir::new();
    backend.static_elf = Some(test_state(&root.0));
    let original_pid = backend.static_elf.as_ref().unwrap().pid;
    let mut original: libc::rlimit = unsafe { std::mem::zeroed() };
    assert_eq!(
        unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut original) },
        0
    );
    let highest = std::fs::read_dir("/proc/self/fd")
        .unwrap()
        .map(|entry| {
            entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .parse::<u64>()
                .unwrap()
        })
        .max()
        .unwrap();
    let reduced = libc::rlimit {
        rlim_cur: original.rlim_cur.min(highest + 32),
        rlim_max: original.rlim_max,
    };
    for free_slots in [0, 2] {
        let mut fillers = Vec::new();
        assert_eq!(unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &reduced) }, 0);
        let exhausted = loop {
            match std::fs::File::open("/dev/null") {
                Ok(file) => fillers.push(file),
                Err(error) => break error.raw_os_error(),
            }
        };
        for _ in 0..free_slots {
            drop(fillers.pop().expect("descriptor budget has enough fillers"));
        }
        // Two free slots let the first real pipe succeed; its keeper then
        // leaves only one free slot, so the SECOND pipe2 fails atomically.
        let before_prepared = capture_identity::pipes_prepared();
        let error = futures::executor::block_on(
            backend.run_static_elf_with_tool_completion::<CaptureSetupTool>((), true),
        )
        .err()
        .expect("capture setup must fail before Tool initialization");
        let mut recovered = Vec::new();
        let after_error = loop {
            match std::fs::File::open("/dev/null") {
                Ok(file) => recovered.push(file),
                Err(error) => break error.raw_os_error(),
            }
        };
        assert_eq!(
            unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &original) },
            0
        );
        assert_eq!(exhausted, Some(libc::EMFILE));
        assert_eq!(after_error, Some(libc::EMFILE));
        assert_eq!(recovered.len(), free_slots);
        assert_eq!(
            capture_identity::pipes_prepared() - before_prepared,
            usize::from(free_slots == 2)
        );
        assert!(
            matches!(error, crate::Error::HostIo(ref io) if io.raw_os_error() == Some(libc::EMFILE))
        );
        assert_eq!(backend.static_elf.as_ref().unwrap().pid, original_pid);
        assert!(backend.tool_failure.is_none());
        drop(recovered);
        drop(fillers);
        let owner = backend.prepare_captured_output(true).unwrap().unwrap();
        for fd in owner.identities.descriptors() {
            assert_eq!(
                capture_native_stat(fd).st_mode & libc::S_IFMT,
                libc::S_IFIFO
            );
        }
    }
    // Only closed standard-number slots remain available. pipe2 succeeds in
    // 0/1, but relocating its first retained endpoint above 2 must fail.
    let saved = [0, 1].map(|fd| {
        let copy = unsafe { libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, 3) };
        assert!(copy >= 3);
        unsafe { std::os::fd::OwnedFd::from_raw_fd(copy) }
    });
    assert_eq!(unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &reduced) }, 0);
    let mut fillers = Vec::new();
    let exhausted = loop {
        match std::fs::File::open("/dev/null") {
            Ok(file) => fillers.push(file),
            Err(error) => break error.raw_os_error(),
        }
    };
    for fd in [0, 1] {
        assert_eq!(unsafe { libc::close(fd) }, 0);
    }
    let before_relocation = capture_identity::relocation_failures();
    let error = futures::executor::block_on(
        backend.run_static_elf_with_tool_completion::<CaptureSetupTool>((), true),
    )
    .err()
    .expect("private descriptor relocation must fail");
    let closed = [0, 1].map(|fd| unsafe { libc::fcntl(fd, libc::F_GETFD) } == -1
        && std::io::Error::last_os_error().raw_os_error() == Some(libc::EBADF));
    assert_eq!(
        unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &original) },
        0
    );
    for (fd, saved) in [0, 1].into_iter().zip(&saved) {
        assert_eq!(unsafe { libc::dup2(saved.as_raw_fd(), fd) }, fd);
    }
    assert_eq!(exhausted, Some(libc::EMFILE));
    assert_eq!(
        capture_identity::relocation_failures() - before_relocation,
        1
    );
    assert_eq!(closed, [true; 2]);
    assert!(
        matches!(error, crate::Error::HostIo(ref io) if io.raw_os_error() == Some(libc::EMFILE))
    );
    assert_eq!(backend.static_elf.as_ref().unwrap().pid, original_pid);
    assert!(backend.tool_failure.is_none());
    drop(fillers);
    drop(saved);
    let owner = backend.prepare_captured_output(true).unwrap().unwrap();
    for fd in owner.identities.descriptors() {
        assert!(fd >= 3);
    }
    eprintln!("{COMPLETE}");
}
