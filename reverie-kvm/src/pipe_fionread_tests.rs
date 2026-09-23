// Included in executor::tests so these controls exercise the actual decoder,
// executor dispatch and complete guest-memory image.
fn pipe_fionread_call(executor: &mut ElfExecutor, memory: &GuestMemory, args: [u64; 6]) -> i64 {
    let raw = SyscallRequest::new(libc::SYS_ioctl as u64, args);
    let decoded = raw.into_syscall().unwrap();
    let transported = SyscallRequest::from_syscall(decoded);
    assert_eq!(transported, raw);
    executor.execute(&transported, memory)
}

fn pipe_fionread_host_pipe() -> [std::fs::File; 2] {
    let mut fds = [-1; 2];
    // SAFETY: fds has space for both newly owned descriptors.
    assert_eq!(unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) }, 0);
    // SAFETY: successful pipe2 transferred ownership of both descriptors.
    unsafe {
        [
            std::fs::File::from_raw_fd(fds[0]),
            std::fs::File::from_raw_fd(fds[1]),
        ]
    }
}

#[test]
fn pipe_fionread_complete_memory_and_request_width() {
    let root = TestDir::new();
    let mut state = test_state(&root.0);
    let [reader, writer] = pipe_fionread_host_pipe();
    assert_eq!(
        unsafe { libc::write(writer.as_raw_fd(), b"abcdefg".as_ptr().cast(), 7) },
        7
    );
    state.files.insert(9, reader);
    state.files.insert(10, writer);
    let mut executor = ElfExecutor::new(state, false);
    let memory = GuestMemory::new(0, 16384).unwrap();
    let sentinel = vec![0xa5; 16384];
    for (fd, command) in [
        (9, libc::FIONREAD),
        (10, libc::FIONREAD),
        (9 + (1 << 32), libc::FIONREAD),
        (10, libc::FIONREAD | (1 << 32)),
    ] {
        memory.write_raw(0, &sentinel).unwrap();
        assert_eq!(
            pipe_fionread_call(&mut executor, &memory, [fd, command, 123, 0, 0, 0]),
            0
        );
        let mut expected = sentinel.clone();
        expected[123..127].copy_from_slice(&7_i32.to_ne_bytes());
        let mut actual = vec![0; sentinel.len()];
        memory.read_raw(0, &mut actual).unwrap();
        assert_eq!(actual, expected);
    }
    memory
        .map_user_permissions(0, PAGE_SIZE, true, true)
        .unwrap();
    memory
        .map_user_permissions(PAGE_SIZE, PAGE_SIZE, true, false)
        .unwrap();
    memory.enable_user_access();
    for address in [
        PAGE_SIZE - 1,
        PAGE_SIZE - 2,
        PAGE_SIZE - 3,
        PAGE_SIZE,
        2 * PAGE_SIZE,
        u64::MAX - 1,
    ] {
        memory.write_raw(0, &sentinel).unwrap();
        assert_eq!(
            pipe_fionread_call(
                &mut executor,
                &memory,
                [9, libc::FIONREAD, address, 0, 0, 0]
            ),
            negative_errno(libc::EFAULT)
        );
        assert_eq!(
            pipe_fionread_call(
                &mut executor,
                &memory,
                [99, libc::FIONREAD, address, 0, 0, 0]
            ),
            negative_errno(libc::EBADF)
        );
        let mut actual = vec![0; sentinel.len()];
        memory.read_raw(0, &mut actual).unwrap();
        assert_eq!(actual, sentinel);
    }
    let mut payload = [0; 7];
    assert_eq!(
        unsafe {
            libc::read(
                executor.state.files[&9].as_raw_fd(),
                payload.as_mut_ptr().cast(),
                7,
            )
        },
        7
    );
    assert_eq!(&payload, b"abcdefg");
}

#[test]
fn pipe_fionread_refuses_capture_carriers_without_physical_stdio() {
    const TEST: &str =
        "executor::tests::pipe_fionread_refuses_capture_carriers_without_physical_stdio";
    let Ok(mode) = std::env::var("REVERIE_PIPE_CAPTURE_CHILD") else {
        for mode in ["objects", "missing"] {
            let output = std::process::Command::new("timeout")
                .args(["--kill-after=2s", "10s"])
                .arg(std::env::current_exe().unwrap())
                .args(["--exact", TEST, "--nocapture"])
                .env("REVERIE_PIPE_CAPTURE_CHILD", mode)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "mode={mode} status={:?} stdout={} stderr={}",
                output.status,
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        }
        return;
    };
    // Host stdio manipulation is confined to an exact-test subprocess. Keep
    // saved descriptors alive and restore them before libtest reports results.
    struct Restore(Vec<(i32, std::fs::File)>);
    impl Drop for Restore {
        fn drop(&mut self) {
            for (fd, saved) in &self.0 {
                assert_eq!(unsafe { libc::dup2(saved.as_raw_fd(), *fd) }, *fd);
            }
        }
    }
    let mut restore = Restore(Vec::new());
    let mut pipes = Vec::new();
    for fd in [1, 2] {
        let saved = unsafe { libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, 3) };
        assert!(saved >= 0);
        restore
            .0
            .push((fd, unsafe { std::fs::File::from_raw_fd(saved) }));
        let pair = pipe_fionread_host_pipe();
        assert_eq!(
            unsafe { libc::write(pair[1].as_raw_fd(), b"supervisor!".as_ptr().cast(), 11) },
            11
        );
        assert_eq!(unsafe { libc::dup2(pair[1].as_raw_fd(), fd) }, fd);
        pipes.push(pair);
    }
    let root = TestDir::new();
    let mut state = test_state(&root.0);
    let [reader, writer] = pipe_fionread_host_pipe();
    state.files.insert(10, reader);
    state.files.insert(11, writer);
    let mut count = -1;
    assert_eq!(unsafe { libc::ioctl(1, libc::FIONREAD, &mut count) }, 0);
    assert_eq!(count, 11, "native backing has unrelated supervisor bytes");
    let output = CapturedOutput::try_new().unwrap();
    let capture = output.metadata();
    let carrier_flags = [OutputAlias::Stdout, OutputAlias::Stderr].map(|alias| {
        let flags = unsafe { libc::fcntl(capture.descriptor_carrier(alias), libc::F_GETFL) };
        assert!(flags >= 0);
        flags
    });
    let carrier_capacities = [OutputAlias::Stdout, OutputAlias::Stderr].map(|alias| {
        let capacity =
            unsafe { libc::fcntl(capture.descriptor_carrier(alias), libc::F_GETPIPE_SZ) };
        assert!(capacity > 0);
        capacity
    });
    let duplicate = unsafe {
        libc::fcntl(
            capture.descriptor_carrier(OutputAlias::Stdout),
            libc::F_DUPFD_CLOEXEC,
            3,
        )
    };
    assert!(duplicate >= 0);
    state
        .files
        .insert(9, unsafe { std::fs::File::from_raw_fd(duplicate) });
    assert!(
        output_alias(&state, 9).is_none(),
        "models received metadata for a private capture carrier"
    );
    let mut executor = ElfExecutor::with_output(state, Some(output));
    let next_object_inode = executor
        .state
        .file_identity_table
        .lock()
        .unwrap()
        .next_inode;
    let mut memory = GuestMemory::new(0, 16384).unwrap();
    let sentinel = vec![0xa5; 16384];
    memory.write(64, &1_i32.to_ne_bytes()).unwrap();
    assert_eq!(
        executor.execute(
            &SyscallRequest::new(
                libc::SYS_ioctl as u64,
                [9, (1_u64 << 32) | libc::FIONBIO, 64, 0, 0, 0],
            ),
            &memory,
        ),
        negative_errno(libc::ENOTTY),
        "an unlabelled capture carrier must not accept status mutation"
    );
    for (command, argument) in [
        (libc::F_SETFL, libc::O_NONBLOCK as u64),
        (libc::F_GETPIPE_SZ, 0),
        (libc::F_SETPIPE_SZ, 4096),
    ] {
        assert_eq!(
            executor.execute(
                &SyscallRequest::new(
                    libc::SYS_fcntl as u64,
                    [
                        (1_u64 << 32) | 9,
                        (1_u64 << 32) | command as u64,
                        argument,
                        0,
                        0,
                        0,
                    ],
                ),
                &memory,
            ),
            negative_errno(libc::ENOSYS),
            "an unlabelled capture carrier accepted fcntl command {command}"
        );
    }
    for (index, alias) in [OutputAlias::Stdout, OutputAlias::Stderr]
        .into_iter()
        .enumerate()
    {
        assert_eq!(
            unsafe { libc::fcntl(capture.descriptor_carrier(alias), libc::F_GETFL) },
            carrier_flags[index],
            "status operation changed private capture carrier {index}"
        );
        assert_eq!(
            unsafe { libc::fcntl(capture.descriptor_carrier(alias), libc::F_GETPIPE_SZ) },
            carrier_capacities[index],
            "capacity operation changed private capture carrier {index}"
        );
    }
    assert_eq!(
        executor
            .state
            .file_identity_table
            .lock()
            .unwrap()
            .next_inode,
        next_object_inode,
        "refused carrier operations allocated guest object identity"
    );
    if mode == "missing" {
        // Probe this guard without another file-table installation: installation
        // may legitimately allocate a cloned File into the just-closed host fd.
        memory.write_raw(0, &sentinel).unwrap();
        assert_eq!(unsafe { libc::close(2) }, 0);
        let result =
            pipe_fionread(
                &memory,
                &executor.state,
                10,
                executor.state.files[&10].as_raw_fd(),
                123,
                executor.output.as_ref().map(CapturedOutput::metadata),
            );
        assert_eq!(result, 0, "unrelated pipe survives closed physical stderr");
        let mut actual = vec![0; sentinel.len()];
        let mut expected = sentinel;
        expected[123..127].copy_from_slice(&0_i32.to_ne_bytes());
        memory.read_raw(0, &mut actual).unwrap();
        assert_eq!(actual, expected);
        drop(restore);
        return;
    }
    for fd in [1, 2, 9] {
        memory.write_raw(0, &sentinel).unwrap();
        assert_eq!(
            pipe_fionread_call(&mut executor, &memory, [fd, libc::FIONREAD, 123, 0, 0, 0]),
            negative_errno(libc::ENOTTY),
            "capture identity was not refused for guest fd {fd}"
        );
        let mut actual = vec![0; sentinel.len()];
        memory.read_raw(0, &mut actual).unwrap();
        assert_eq!(actual, sentinel);
    }
    for fd in [1, 2] {
        assert_eq!(
            executor.execute(
                &SyscallRequest::new(libc::SYS_close as u64, [fd, 0, 0, 0, 0, 0]),
                &memory
            ),
            0
        );
    }
    memory.write_raw(0, &sentinel).unwrap();
    assert_eq!(
        pipe_fionread_call(&mut executor, &memory, [9, libc::FIONREAD, 123, 0, 0, 0]),
        negative_errno(libc::ENOTTY)
    );
    let mut actual = vec![0; sentinel.len()];
    memory.read_raw(0, &mut actual).unwrap();
    assert_eq!(
        actual, sentinel,
        "guest closure does not authorize the backing object"
    );
    let result = pipe_fionread_call(&mut executor, &memory, [10, libc::FIONREAD, 123, 0, 0, 0]);
    let mut expected = sentinel;
    if mode == "objects" {
        assert_eq!(result, 0, "unrelated pipe remains supported under capture");
        expected[123..127].copy_from_slice(&0_i32.to_ne_bytes());
    } else {
        assert_eq!(result, negative_errno(libc::ENOTTY));
    }
    memory.read_raw(0, &mut actual).unwrap();
    assert_eq!(actual, expected);
    drop(executor);
    drop(pipes);
    drop(restore);
}
