// Included in executor::tests; these use the real descriptor and executor paths.
fn capture_native_stat(fd: RawFd) -> libc::stat {
    let mut stat = std::mem::MaybeUninit::<libc::stat>::zeroed();
    assert_eq!(unsafe { libc::fstat(fd, stat.as_mut_ptr()) }, 0);
    unsafe { stat.assume_init() }
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
    let memory = GuestMemory::new(0, 0x4000).unwrap();
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
        table: Arc::downgrade(&executor.file_table),
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

    assert_eq!(
        unsafe { libc::fcntl(saved_stdout, libc::F_SETFL, original_flags) },
        0
    );
    assert_eq!(unsafe { libc::dup2(saved_stdout, 1) }, 1);
    assert_eq!(unsafe { libc::close(saved_stdout) }, 0);
    eprintln!("{COMPLETE}");
}

#[test]
fn captured_output_alias_identity_survives_thread_fork_exec_and_replacement() {
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
    let replacement = test_exec_replacement(&root.0, &child.state);
    child.replace_after_exec(replacement);
    assert!(Arc::ptr_eq(
        &child.state.capture_status_flags[&alias],
        &stdout_status
    ));
    assert_eq!(capture_executor_stat(&mut child, &memory, alias), stdout);
    assert_eq!(capture_executor_stat(&mut child, &memory, 1), stderr);
    assert_eq!(
        parent.execute(
            &SyscallRequest::new(libc::SYS_close as u64, [alias as u64, 0, 0, 0, 0, 0]),
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
    for fd in owner.identities.descriptors() {
        assert!(fd >= 3);
        assert_eq!(
            capture_native_stat(fd).st_mode & libc::S_IFMT,
            libc::S_IFIFO
        );
        assert_eq!(unsafe { libc::fcntl(fd, libc::F_GETFD) }, libc::FD_CLOEXEC);
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
    // 0/1, its unused end closes, but F_DUPFD_CLOEXEC(min=3) must fail.
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
