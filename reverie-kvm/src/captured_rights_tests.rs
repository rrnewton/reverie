// These controls use real executor dispatch except the explicitly synthetic
// staging-failure/budget cases, whose ownership inputs are constructed here.
#[test]
fn captured_rights_late_control_fault_rolls_back_both_tables_after_consuming() {
    let root = TestDir::new();
    let mut e = ElfExecutor::new(test_state(&root.0), true);
    let mut m = GuestMemory::new(0, 0x4000).unwrap();
    let pair = capture_rights_pair(&mut e, &mut m);
    assert_eq!(capture_rights_send(&mut e, &mut m, pair[0], &[1, 2]), 1);
    let before = e.state.files.keys().copied().collect::<Vec<_>>();
    let descriptions = e
        .state
        .capture_descriptions
        .keys()
        .copied()
        .collect::<Vec<_>>();
    let iov = libc::iovec {
        iov_base: 0xa00_usize as *mut _,
        iov_len: 1,
    };
    assert_eq!(write_struct(&mut m, 0x700, &iov), 0);
    let mut message: libc::msghdr = unsafe { std::mem::zeroed() };
    message.msg_iov = 0x700_usize as *mut _;
    message.msg_iovlen = 1;
    message.msg_control = 0x1000_usize as *mut _;
    message.msg_controllen = 128;
    assert_eq!(write_struct(&mut m, 0x600, &message), 0);
    m.write_raw(0x1000, &[0xa5; 128]).unwrap();
    m.map_user_permissions(0, 0x4000, true, true).unwrap();
    m.map_user_permissions(0x1000, 0x1000, true, false).unwrap();
    m.enable_user_access();
    assert_eq!(
        capture_rights_call(
            &mut e,
            &m,
            libc::SYS_recvmsg,
            [pair[1] as u64, 0x600, 0, 0, 0, 0]
        ),
        negative_errno(libc::EFAULT)
    );
    assert_eq!(capture_rights_count(&e), 0);
    assert_eq!(read_guest_bytes::<1>(&m, 0xa00).unwrap(), *b"R");
    assert_eq!(read_guest_bytes::<128>(&m, 0x1000).unwrap(), [0xa5; 128]);
    assert_eq!(e.state.files.keys().copied().collect::<Vec<_>>(), before);
    assert_eq!(
        e.state
            .capture_descriptions
            .keys()
            .copied()
            .collect::<Vec<_>>(),
        descriptions
    );
    let shared = e.file_table.lock().unwrap();
    assert_eq!(shared.files.keys().copied().collect::<Vec<_>>(), before);
    assert_eq!(
        shared
            .capture_descriptions
            .keys()
            .copied()
            .collect::<Vec<_>>(),
        descriptions
    );
    drop(shared);
    assert_eq!(
        capture_rights_call(
            &mut e,
            &m,
            libc::SYS_recvmsg,
            [pair[1] as u64, 0x600, libc::MSG_DONTWAIT as u64, 0, 0, 0]
        ),
        negative_errno(libc::EAGAIN)
    );
}

#[test]
fn captured_rights_zero_length_datagram_keeps_queued_authority() {
    let root = TestDir::new();
    let mut e = ElfExecutor::new(test_state(&root.0), true);
    let mut m = GuestMemory::new(0, 0x4000).unwrap();
    let pair = capture_rights_pair(&mut e, &mut m);
    let control = rights_control(&[1]);
    m.write(0x500, &control).unwrap();
    let mut message: libc::msghdr = unsafe { std::mem::zeroed() };
    message.msg_control = 0x500_usize as *mut _;
    message.msg_controllen = control.len();
    assert_eq!(write_struct(&mut m, 0x300, &message), 0);
    assert_eq!(
        capture_rights_call(
            &mut e,
            &m,
            libc::SYS_sendmsg,
            [pair[0] as u64, 0x300, 0, 0, 0, 0]
        ),
        0
    );
    assert_eq!(capture_rights_count(&e), 1);
    message.msg_control = 0x800_usize as *mut _;
    message.msg_controllen = 128;
    assert_eq!(write_struct(&mut m, 0x300, &message), 0);
    assert_eq!(
        capture_rights_call(
            &mut e,
            &m,
            libc::SYS_recvmsg,
            [pair[1] as u64, 0x300, 0, 0, 0, 0]
        ),
        0
    );
    assert_eq!(capture_rights_count(&e), 0);
    let returned: libc::msghdr = read_struct(&m, 0x300);
    let mut control = vec![0; returned.msg_controllen];
    m.read(0x800, &mut control).unwrap();
    let fd = control_rights(&control)[0];
    m.write(0xb00, b"zero").unwrap();
    assert_eq!(
        capture_rights_call(&mut e, &m, libc::SYS_write, [fd as u64, 0xb00, 4, 0, 0, 0]),
        4
    );
    assert_eq!(e.take_output(), (b"zero".to_vec(), Vec::new()));
}

#[test]
fn captured_rights_wrong_owner_and_path_only_donations_stay_refused() {
    let root = TestDir::new();
    let mut source = ElfExecutor::new(test_state(&root.0), true);
    let mut receiver = ElfExecutor::new(test_state(&root.0), true);
    register_capture_transfer(&source.state, 1).unwrap();
    let token = source.state.capture_descriptions[&1]
        .token
        .try_clone()
        .unwrap();
    receiver.state.file_identity_table = source.state.file_identity_table.clone();
    assert_eq!(
        install_received_rights(
            &mut receiver.state,
            &mut [0; 4],
            vec![PendingReceivedRight {
                control_offset: 0,
                file: token
            }],
            false,
            false
        ),
        Err(negative_errno(libc::ENOSYS))
    );
    assert_eq!(capture_rights_count(&source), 0);
    let mut m = GuestMemory::new(0, 0x4000).unwrap();
    let pair = capture_rights_pair(&mut source, &mut m);
    write_c_string(&mut m, 0xc00, "/proc/self/fd/1");
    let fd = capture_rights_call(
        &mut source,
        &m,
        libc::SYS_open,
        [0xc00, libc::O_PATH as u64, 0, 0, 0, 0],
    );
    assert!(fd >= 0);
    assert_eq!(
        capture_rights_send(&mut source, &mut m, pair[0], &[fd as i32]),
        negative_errno(libc::ENOSYS)
    );
    assert_eq!(capture_rights_count(&source), 0);
}

fn capture_rights_call(e: &mut ElfExecutor, m: &GuestMemory, n: libc::c_long, a: [u64; 6]) -> i64 {
    e.execute(&SyscallRequest::new(n as u64, a), m)
}

fn capture_rights_pair(e: &mut ElfExecutor, m: &mut GuestMemory) -> [i32; 2] {
    assert_eq!(
        capture_rights_call(
            e,
            m,
            libc::SYS_socketpair,
            [
                libc::AF_UNIX as u64,
                libc::SOCK_DGRAM as u64,
                0,
                0x100,
                0,
                0
            ]
        ),
        0
    );
    read_struct(m, 0x100)
}

fn capture_rights_send(e: &mut ElfExecutor, m: &mut GuestMemory, socket: i32, fds: &[i32]) -> i64 {
    m.write(0x200, b"R").unwrap();
    let iov = libc::iovec {
        iov_base: 0x200_usize as *mut _,
        iov_len: 1,
    };
    assert_eq!(write_struct(m, 0x400, &iov), 0);
    let control = rights_control(fds);
    m.write(0x500, &control).unwrap();
    let mut message: libc::msghdr = unsafe { std::mem::zeroed() };
    message.msg_iov = 0x400_usize as *mut _;
    message.msg_iovlen = 1;
    message.msg_control = 0x500_usize as *mut _;
    message.msg_controllen = control.len();
    assert_eq!(write_struct(m, 0x300, &message), 0);
    capture_rights_call(e, m, libc::SYS_sendmsg, [socket as u64, 0x300, 0, 0, 0, 0])
}

fn capture_rights_receive(
    e: &mut ElfExecutor,
    m: &mut GuestMemory,
    socket: i32,
    flags: i32,
) -> Vec<i32> {
    let iov = libc::iovec {
        iov_base: 0xa00_usize as *mut _,
        iov_len: 1,
    };
    assert_eq!(write_struct(m, 0x700, &iov), 0);
    let mut message: libc::msghdr = unsafe { std::mem::zeroed() };
    message.msg_iov = 0x700_usize as *mut _;
    message.msg_iovlen = 1;
    message.msg_control = 0x800_usize as *mut _;
    message.msg_controllen = 128;
    assert_eq!(write_struct(m, 0x600, &message), 0);
    assert_eq!(
        capture_rights_call(
            e,
            m,
            libc::SYS_recvmsg,
            [socket as u64, 0x600, flags as u64, 0, 0, 0]
        ),
        1
    );
    let message: libc::msghdr = read_struct(m, 0x600);
    let mut control = vec![0; message.msg_controllen];
    m.read(0x800, &mut control).unwrap();
    control_rights(&control)
}

fn capture_rights_count(e: &ElfExecutor) -> usize {
    virtual_transfers_in_flight(&e.state.file_identity_table.lock().unwrap())
}

fn capture_rights_flags(e: &mut ElfExecutor, m: &GuestMemory, fd: i32) -> i32 {
    capture_rights_call(
        e,
        m,
        libc::SYS_fcntl,
        [fd as u64, libc::F_GETFL as u64, 0, 0, 0, 0],
    ) as i32
}

#[test]
fn captured_rights_restore_stream_after_sender_close_and_fd_reuse() {
    let root = TestDir::new();
    let owner = CapturedOutput::try_new().unwrap();
    let mut e = ElfExecutor::with_test_output(test_state(&root.0), Some(owner.clone()));
    let mut m = GuestMemory::new(0, 0x4000).unwrap();
    let pair = capture_rights_pair(&mut e, &mut m);
    let identities = [
        capture_executor_stat(&mut e, &m, 1),
        capture_executor_stat(&mut e, &m, 2),
    ];
    assert_eq!(
        capture_rights_call(
            &mut e,
            &m,
            libc::SYS_fcntl,
            [1, libc::F_SETFL as u64, libc::O_NONBLOCK as u64, 0, 0, 0]
        ),
        0
    );
    let aliases =
        [1, 2].map(|fd| capture_rights_call(&mut e, &m, libc::SYS_dup, [fd, 0, 0, 0, 0, 0]) as i32);
    assert_eq!(capture_rights_send(&mut e, &mut m, pair[0], &aliases), 1);
    assert_eq!(capture_rights_count(&e), 2);
    for fd in [aliases[0], aliases[1], 1, 2] {
        assert_eq!(
            capture_rights_call(&mut e, &m, libc::SYS_close, [fd as u64, 0, 0, 0, 0, 0]),
            0
        );
    }
    write_c_string(&mut m, 0xc00, "capture-reused");
    let replacement = capture_rights_call(
        &mut e,
        &m,
        libc::SYS_open,
        [0xc00, (libc::O_CREAT | libc::O_RDWR) as u64, 0o600, 0, 0, 0],
    );
    assert_eq!(replacement, 1);
    let received = capture_rights_receive(&mut e, &mut m, pair[1], libc::MSG_CMSG_CLOEXEC);
    assert_eq!(received.len(), 2);
    assert_eq!(capture_rights_count(&e), 0);
    for (index, &fd) in received.iter().enumerate() {
        assert_eq!(capture_executor_stat(&mut e, &m, fd), identities[index]);
        assert_eq!(
            capture_rights_call(
                &mut e,
                &m,
                libc::SYS_fcntl,
                [fd as u64, libc::F_GETFD as u64, 0, 0, 0, 0]
            ),
            libc::FD_CLOEXEC as i64
        );
        assert_eq!(
            unsafe { libc::fcntl(e.state.files[&fd].as_raw_fd(), libc::F_GETFL) } & libc::O_ACCMODE,
            libc::O_WRONLY
        );
        m.write(0xb00, &[0xa5; 32]).unwrap();
        assert_eq!(
            capture_rights_call(
                &mut e,
                &m,
                libc::SYS_ioctl,
                [fd as u64, libc::FIONREAD, 0xb08, 0, 0, 0]
            ),
            negative_errno(libc::ENOTTY)
        );
        assert_eq!(read_guest_bytes::<32>(&m, 0xb00).unwrap(), [0xa5; 32]);
        assert_eq!(
            capture_rights_call(&mut e, &m, libc::SYS_read, [fd as u64, 0xb00, 1, 0, 0, 0]),
            negative_errno(libc::EBADF)
        );
        m.write(0xb00, if index == 0 { b"out" } else { b"err" })
            .unwrap();
        assert_eq!(
            capture_rights_call(&mut e, &m, libc::SYS_write, [fd as u64, 0xb00, 3, 0, 0, 0]),
            3
        );
    }
    assert_ne!(
        capture_rights_flags(&mut e, &m, received[0]) & libc::O_NONBLOCK,
        0
    );
    assert_eq!(e.take_output(), (b"out".to_vec(), b"err".to_vec()));
    assert_eq!(std::fs::read(root.0.join("capture-reused")).unwrap(), b"");
}

#[test]
fn captured_rights_peek_fork_thread_exec_share_description_and_status() {
    let root = TestDir::new();
    let mut e = ElfExecutor::new(test_state(&root.0), true);
    let mut captured = e.output.as_ref().expect("capture root").clone();
    let mut m = GuestMemory::new(0, 0x4000).unwrap();
    let pair = capture_rights_pair(&mut e, &mut m);
    write_c_string(&mut m, 0xc00, "/proc/self/fd/1");
    let reopened = capture_rights_call(
        &mut e,
        &m,
        libc::SYS_open,
        [
            0xc00,
            (libc::O_WRONLY | libc::O_NONBLOCK) as u64,
            0,
            0,
            0,
            0,
        ],
    ) as i32;
    assert!(reopened >= 3);
    assert_eq!(capture_rights_flags(&mut e, &m, 1), libc::O_WRONLY);
    let weak = Arc::downgrade(&e.state.capture_descriptions[&reopened]);
    let mut receiver = e.fork_child(3, false, false).unwrap();
    assert_eq!(
        capture_rights_call(
            &mut receiver,
            &m,
            libc::SYS_close,
            [reopened as u64, 0, 0, 0, 0, 0]
        ),
        0
    );
    assert_eq!(capture_rights_send(&mut e, &mut m, pair[0], &[reopened]), 1);
    assert_eq!(
        capture_rights_call(
            &mut e,
            &m,
            libc::SYS_close,
            [reopened as u64, 0, 0, 0, 0, 0]
        ),
        0
    );
    drop(e);
    assert!(
        weak.upgrade().is_some(),
        "queued token retains independent reopened description"
    );
    for _ in 0..2 {
        let peek = capture_rights_receive(&mut receiver, &mut m, pair[1], libc::MSG_PEEK)[0];
        assert_eq!(capture_rights_count(&receiver), 1);
        assert_eq!(
            capture_rights_call(
                &mut receiver,
                &m,
                libc::SYS_fcntl,
                [
                    peek as u64,
                    libc::F_SETFL as u64,
                    libc::O_APPEND as u64,
                    0,
                    0,
                    0
                ]
            ),
            0
        );
        assert_eq!(
            capture_rights_call(
                &mut receiver,
                &m,
                libc::SYS_close,
                [peek as u64, 0, 0, 0, 0, 0]
            ),
            0
        );
    }
    let fd = capture_rights_receive(&mut receiver, &mut m, pair[1], 0)[0];
    assert_eq!(capture_rights_count(&receiver), 0);
    assert_ne!(
        capture_rights_flags(&mut receiver, &m, fd) & libc::O_APPEND,
        0
    );
    assert_eq!(
        capture_rights_flags(&mut receiver, &m, fd) & libc::O_NONBLOCK,
        0
    );
    let mut sibling = receiver.thread_child(4).unwrap();
    assert_eq!(
        capture_rights_call(
            &mut sibling,
            &m,
            libc::SYS_fcntl,
            [
                fd as u64,
                libc::F_SETFL as u64,
                libc::O_NONBLOCK as u64,
                0,
                0,
                0
            ]
        ),
        0
    );
    assert_ne!(
        capture_rights_flags(&mut receiver, &m, fd) & libc::O_NONBLOCK,
        0
    );
    drop(sibling);
    let replacement = test_exec_replacement(&root.0, &receiver.state);
    receiver.replace_after_exec(replacement);
    assert!(Arc::ptr_eq(
        &receiver.state.capture_descriptions[&fd],
        &weak.upgrade().unwrap()
    ));
    m.write(0xb00, b"after").unwrap();
    assert_eq!(
        capture_rights_call(
            &mut receiver,
            &m,
            libc::SYS_write,
            [fd as u64, 0xb00, 5, 0, 0, 0]
        ),
        5
    );
    assert!(Arc::ptr_eq(
        &captured.inner,
        &receiver.state.capture_descriptions[&fd].sink,
    ));
    assert!(!receiver.owns_output);
    assert_eq!(receiver.take_output(), (Vec::new(), Vec::new()));
    assert_eq!(captured.take(), (b"after".to_vec(), Vec::new()));
    assert_eq!(
        capture_rights_call(
            &mut receiver,
            &m,
            libc::SYS_close,
            [fd as u64, 0, 0, 0, 0, 0]
        ),
        0
    );
    assert!(
        weak.upgrade().is_none(),
        "final independent description retired"
    );
}

#[test]
fn captured_rights_failed_send_and_mixed_budget_rollback() {
    let root = TestDir::new();
    let mut e = ElfExecutor::new(test_state(&root.0), true);
    let mut m = GuestMemory::new(0, 0x4000).unwrap();
    let pair = capture_rights_pair(&mut e, &mut m);
    assert_eq!(
        capture_rights_send(&mut e, &mut m, pair[0], &[1, 99999]),
        negative_errno(libc::EBADF)
    );
    assert_eq!(capture_rights_count(&e), 0);
    assert_eq!(
        control_rights(&read_guest_bytes::<24>(&m, 0x500).unwrap()),
        [1, 99999]
    );
    let mut ordinary = rights_control(&[1]);
    let missing = test_state(&root.0);
    assert_eq!(
        translate_outgoing_control(&mut ordinary, &missing, true),
        Err(negative_errno(libc::ENOSYS))
    );

    write_c_string(&mut m, 0xc00, "/proc/self/status");
    let proc_fd = capture_rights_call(
        &mut e,
        &m,
        libc::SYS_open,
        [0xc00, libc::O_RDONLY as u64, 0, 0, 0, 0],
    ) as i32;
    assert!(proc_fd >= 3);
    let mut reservations = Vec::new();
    for _ in 0..PROC_TRANSFER_LIMIT - 1 {
        let (key, _) = register_capture_transfer(&e.state, 1).unwrap();
        reservations.push(OutgoingTransfer::Capture(key));
    }
    let proc_key =
        register_proc_transfer(&e.state, proc_fd, host_fd(&e.state, proc_fd).unwrap()).unwrap();
    reservations.push(OutgoingTransfer::Proc(proc_key));
    assert_eq!(capture_rights_count(&e), PROC_TRANSFER_LIMIT);
    assert_eq!(
        capture_rights_send(&mut e, &mut m, pair[0], &[1]),
        negative_errno(libc::ETOOMANYREFS)
    );
    assert_eq!(
        register_proc_transfer(&e.state, proc_fd, host_fd(&e.state, proc_fd).unwrap()),
        Err(negative_errno(libc::ETOOMANYREFS))
    );
    release_outgoing_transfers(&e.state, &reservations);
    assert_eq!(capture_rights_count(&e), 0);
    assert_eq!(
        capture_rights_call(&mut e, &m, libc::SYS_close, [pair[1] as u64, 0, 0, 0, 0, 0]),
        0
    );
    assert!(capture_rights_send(&mut e, &mut m, pair[0], &[1, 2]) < 0);
    assert_eq!(capture_rights_count(&e), 0);
}

#[test]
fn captured_rights_failed_staging_and_unknown_tokens_do_not_publish_metadata() {
    let root = TestDir::new();
    let mut e = ElfExecutor::new(test_state(&root.0), true);
    for _ in 0..3 {
        register_capture_transfer(&e.state, 1).unwrap();
    }
    let files_before = e.state.files.keys().copied().collect::<Vec<_>>();
    let descriptions_before = e
        .state
        .capture_descriptions
        .keys()
        .copied()
        .collect::<Vec<_>>();
    for peek in [true, false] {
        let rights = [0, 8, 4]
            .map(|control_offset| PendingReceivedRight {
                control_offset,
                file: e.state.capture_descriptions[&1].token.try_clone().unwrap(),
            })
            .into();
        assert_eq!(
            install_received_rights(&mut e.state, &mut [0; 8], rights, false, peek),
            Err(negative_errno(libc::EINVAL))
        );
        assert_eq!(capture_rights_count(&e), if peek { 3 } else { 0 });
        assert_eq!(
            e.state.files.keys().copied().collect::<Vec<_>>(),
            files_before
        );
        assert_eq!(
            e.state
                .capture_descriptions
                .keys()
                .copied()
                .collect::<Vec<_>>(),
            descriptions_before
        );
    }
    let token = capture_token().unwrap();
    assert!(capture_token_candidate(&token).unwrap());
    assert_eq!(
        install_received_rights(
            &mut e.state,
            &mut [0; 4],
            vec![PendingReceivedRight {
                control_offset: 0,
                file: token
            }],
            false,
            false
        ),
        Err(negative_errno(libc::EBADMSG))
    );
    let description = e.state.capture_descriptions[&1].clone();
    let unlabelled = insert_file_with_flags(
        &mut e.state,
        description.io.try_clone().unwrap(),
        false,
        None,
    ) as i32;
    let mut control = rights_control(&[unlabelled]);
    assert_eq!(
        translate_outgoing_control(&mut control, &e.state, true),
        Err(negative_errno(libc::ENOSYS))
    );
}

#[test]
fn captured_rights_never_mutate_shared_physical_stdout_stderr() {
    const TEST: &str =
        "executor::tests::captured_rights_never_mutate_shared_physical_stdout_stderr";
    const DONE: &str = "captured rights physical isolation checked";
    if !capture_test_child(TEST, DONE) {
        return;
    }
    struct Restore([std::fs::File; 2]);
    impl Drop for Restore {
        fn drop(&mut self) {
            for (fd, file) in [1, 2].into_iter().zip(&self.0) {
                assert_eq!(unsafe { libc::dup2(file.as_raw_fd(), fd) }, fd);
            }
        }
    }
    let restore = Restore([1, 2].map(|fd| {
        let raw = unsafe { libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, 3) };
        assert!(raw >= 3);
        unsafe { std::fs::File::from_raw_fd(raw) }
    }));
    let root = TestDir::new();
    let physical = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(root.0.join("physical"))
        .unwrap();
    physical.write_at(b"supervisor", 0).unwrap();
    for fd in [1, 2] {
        assert_eq!(unsafe { libc::dup2(physical.as_raw_fd(), fd) }, fd);
    }
    let before = unsafe { libc::fcntl(physical.as_raw_fd(), libc::F_GETFL) };
    let metadata_before = capture_native_stat(physical.as_raw_fd());
    let mut e = ElfExecutor::new(test_state(&root.0), true);
    let mut m = GuestMemory::new(0, 0x4000).unwrap();
    let pair = capture_rights_pair(&mut e, &mut m);
    // Deliberately remove provenance from an actual physical duplicate. Its
    // matching inode is refusal evidence, never authority to restore capture.
    let unlabelled =
        insert_file_with_flags(&mut e.state, physical.try_clone().unwrap(), false, None) as i32;
    let mut control = rights_control(&[unlabelled]);
    assert_eq!(
        translate_outgoing_control(&mut control, &e.state, true),
        Err(negative_errno(libc::ENOSYS))
    );
    assert_eq!(close(&mut e.state, unlabelled as u64), 0);
    assert_eq!(capture_rights_send(&mut e, &mut m, pair[0], &[1, 2]), 1);
    let received = capture_rights_receive(&mut e, &mut m, pair[1], 0);
    for (index, fd) in received.into_iter().enumerate() {
        assert_ne!(
            host_file_key(e.state.files[&fd].as_raw_fd()).unwrap(),
            host_file_key(physical.as_raw_fd()).unwrap()
        );
        assert_eq!(
            capture_rights_call(
                &mut e,
                &m,
                libc::SYS_fcntl,
                [
                    fd as u64,
                    libc::F_SETFL as u64,
                    libc::O_APPEND as u64,
                    0,
                    0,
                    0
                ]
            ),
            0
        );
        assert_ne!(
            capture_rights_flags(&mut e, &m, (index + 1) as i32) & libc::O_APPEND,
            0
        );
        m.write(0xb00, if index == 0 { b"O" } else { b"E" })
            .unwrap();
        assert_eq!(
            capture_rights_call(&mut e, &m, libc::SYS_write, [fd as u64, 0xb00, 1, 0, 0, 0]),
            1
        );
    }
    assert_eq!(e.take_output(), (b"O".to_vec(), b"E".to_vec()));
    assert_eq!(
        unsafe { libc::fcntl(physical.as_raw_fd(), libc::F_GETFL) },
        before
    );
    assert_eq!(
        std::fs::read(root.0.join("physical")).unwrap(),
        b"supervisor"
    );
    assert_eq!(
        unsafe { libc::lseek(physical.as_raw_fd(), 0, libc::SEEK_CUR) },
        0
    );
    let metadata_after = capture_native_stat(physical.as_raw_fd());
    assert_eq!(
        (
            metadata_after.st_dev,
            metadata_after.st_ino,
            metadata_after.st_mode,
            metadata_after.st_size
        ),
        (
            metadata_before.st_dev,
            metadata_before.st_ino,
            metadata_before.st_mode,
            metadata_before.st_size
        )
    );
    drop(e);
    drop(restore);
    eprintln!("{DONE}");
}

#[test]
fn captured_rights_receive_clone_failure_retires_sender_final_owners_unlocked() {
    for successful_clones in [0, 1] {
        let root = TestDir::new();
        let mut sender = ElfExecutor::new(test_state(&root.0), true);
        let mut m = GuestMemory::new(0, 0x4000).unwrap();
        let pair = capture_rights_pair(&mut sender, &mut m);
        write_c_string(&mut m, 0xc00, "/proc/self/fd/1");
        let fd = capture_rights_call(
            &mut sender,
            &m,
            libc::SYS_open,
            [0xc00, libc::O_WRONLY as u64, 0, 0, 0, 0],
        ) as i32;
        assert!(fd >= 3);
        let unique = [
            sender.state.capture_descriptions[&fd].io.as_raw_fd(),
            sender.state.capture_descriptions[&fd].token.as_raw_fd(),
        ];
        let weak = Arc::downgrade(&sender.state.capture_descriptions[&fd]);
        let mut receiver = sender.fork_child(3, false, false).unwrap();
        assert_eq!(
            capture_rights_call(
                &mut receiver,
                &m,
                libc::SYS_close,
                [fd as u64, 0, 0, 0, 0, 0]
            ),
            0
        );
        assert_eq!(capture_rights_send(&mut sender, &mut m, pair[0], &[fd]), 1);
        drop(sender);
        assert!(weak.upgrade().is_some());
        let observed = Arc::new(Mutex::new(std::collections::BTreeSet::new()));
        let seen = observed.clone();
        let table = Arc::downgrade(&receiver.file_table);
        let transaction = receiver.state.signal_transaction.clone();
        receiver
            .state
            .file_retirement
            .set_probe(Some(Arc::new(move |descriptors| {
                let table = table.upgrade().unwrap();
                let _table = table
                    .try_lock()
                    .expect("failed receive close held file table");
                let _transaction = transaction
                    .try_lock()
                    .expect("failed receive close held transaction");
                for fd in unique {
                    if descriptors.contains(&fd) {
                        assert!(unsafe { libc::fcntl(fd, libc::F_GETFD) } >= 0);
                        seen.lock().unwrap().insert(fd);
                    }
                }
            })));
        let iov = libc::iovec {
            iov_base: 0xa00_usize as *mut _,
            iov_len: 1,
        };
        assert_eq!(write_struct(&mut m, 0x700, &iov), 0);
        let mut message: libc::msghdr = unsafe { std::mem::zeroed() };
        message.msg_iov = 0x700_usize as *mut _;
        message.msg_iovlen = 1;
        message.msg_control = 0x800_usize as *mut _;
        message.msg_controllen = 128;
        assert_eq!(write_struct(&mut m, 0x600, &message), 0);
        let before = receiver.state.files.keys().copied().collect::<Vec<_>>();
        receiver
            .state
            .file_retirement
            .fail_clone_after(Some(successful_clones));
        assert_eq!(
            capture_rights_call(
                &mut receiver,
                &m,
                libc::SYS_recvmsg,
                [pair[1] as u64, 0x600, 0, 0, 0, 0]
            ),
            negative_errno(libc::EMFILE)
        );
        receiver.state.file_retirement.fail_clone_after(None);
        receiver.state.file_retirement.set_probe(None);
        assert_eq!(capture_rights_count(&receiver), 0);
        assert!(weak.upgrade().is_none());
        assert_eq!(*observed.lock().unwrap(), unique.into_iter().collect());
        for fd in unique {
            assert_eq!(unsafe { libc::fcntl(fd, libc::F_GETFD) }, -1);
            assert_eq!(
                std::io::Error::last_os_error().raw_os_error(),
                Some(libc::EBADF)
            );
        }
        assert_eq!(
            receiver.state.files.keys().copied().collect::<Vec<_>>(),
            before
        );
        assert_eq!(
            receiver
                .file_table
                .lock()
                .unwrap()
                .files
                .keys()
                .copied()
                .collect::<Vec<_>>(),
            before
        );
        assert_eq!(
            capture_rights_call(
                &mut receiver,
                &m,
                libc::SYS_recvmsg,
                [pair[1] as u64, 0x600, libc::MSG_DONTWAIT as u64, 0, 0, 0]
            ),
            negative_errno(libc::EAGAIN)
        );
    }
}

#[test]
fn captured_rights_authentication_failure_after_valid_right_releases_entire_message() {
    let root = TestDir::new();
    let mut e = ElfExecutor::new(test_state(&root.0), true);
    let before = e.state.files.keys().copied().collect::<Vec<_>>();
    register_capture_transfer(&e.state, 1).unwrap();
    let rights = vec![
        PendingReceivedRight {
            control_offset: 0,
            file: e.state.capture_descriptions[&1].token.try_clone().unwrap(),
        },
        PendingReceivedRight {
            control_offset: 4,
            file: capture_token().unwrap(),
        },
    ];
    assert_eq!(
        install_received_rights(&mut e.state, &mut [0; 8], rights, false, false),
        Err(negative_errno(libc::EBADMSG))
    );
    assert_eq!(capture_rights_count(&e), 0);
    assert_eq!(e.state.files.keys().copied().collect::<Vec<_>>(), before);
}

#[test]
fn captured_rights_unobserved_discard_remains_bounded_until_namespace_drop() {
    for truncate in [false, true] {
        let root = TestDir::new();
        let mut e = ElfExecutor::new(test_state(&root.0), true);
        let mut m = GuestMemory::new(0, 0x4000).unwrap();
        let pair = capture_rights_pair(&mut e, &mut m);
        write_c_string(&mut m, 0xc00, "/proc/self/fd/1");
        let fd = capture_rights_call(
            &mut e,
            &m,
            libc::SYS_open,
            [0xc00, libc::O_WRONLY as u64, 0, 0, 0, 0],
        ) as i32;
        assert!(fd >= 3);
        let weak = Arc::downgrade(&e.state.capture_descriptions[&fd]);
        assert_eq!(capture_rights_send(&mut e, &mut m, pair[0], &[fd]), 1);
        assert_eq!(
            capture_rights_call(&mut e, &m, libc::SYS_close, [fd as u64, 0, 0, 0, 0, 0]),
            0
        );
        if truncate {
            let iov = libc::iovec {
                iov_base: 0xa00_usize as *mut _,
                iov_len: 1,
            };
            assert_eq!(write_struct(&mut m, 0x700, &iov), 0);
            let mut message: libc::msghdr = unsafe { std::mem::zeroed() };
            message.msg_iov = 0x700_usize as *mut _;
            message.msg_iovlen = 1;
            assert_eq!(write_struct(&mut m, 0x600, &message), 0);
            assert_eq!(
                capture_rights_call(
                    &mut e,
                    &m,
                    libc::SYS_recvmsg,
                    [pair[1] as u64, 0x600, 0, 0, 0, 0]
                ),
                1
            );
            let returned: libc::msghdr = read_struct(&m, 0x600);
            assert_ne!(returned.msg_flags & libc::MSG_CTRUNC, 0);
        }
        for socket in pair {
            assert_eq!(
                capture_rights_call(&mut e, &m, libc::SYS_close, [socket as u64, 0, 0, 0, 0, 0]),
                0
            );
        }
        assert_eq!(
            capture_rights_count(&e),
            1,
            "host-discarded rights are conservatively charged"
        );
        assert!(weak.upgrade().is_some());
        drop(e);
        assert!(
            weak.upgrade().is_none(),
            "namespace retirement releases the conservative pin"
        );
    }
}

#[test]
fn captured_rights_write_uses_description_sink_instead_of_fallback_output() {
    let root = TestDir::new();
    let mut e = ElfExecutor::new(test_state(&root.0), true);
    let mut m = GuestMemory::new(0, 0x4000).unwrap();
    let pair = capture_rights_pair(&mut e, &mut m);
    assert_eq!(capture_rights_send(&mut e, &mut m, pair[0], &[1, 2]), 1);
    let received = capture_rights_receive(&mut e, &mut m, pair[1], 0);
    let mut unrelated = CapturedOutput::try_new().unwrap();
    m.write(0xb00, b"owned").unwrap();
    write_guest_iovecs(&mut m, 0xc00, &[(0xb00, 5)]);
    // Deliberately mismatched fallback is a helper-only adversarial input. The
    // real receiver requires the same namespace; its authenticated description
    // remains the output authority even if a caller supplies a different sink.
    assert_eq!(
        syscall_result_with_output(
            &mut m,
            &mut e.state,
            &mut unrelated,
            libc::SYS_write,
            [received[0] as u64, 0xb00, 5, 0, 0, 0]
        ),
        5
    );
    assert_eq!(
        syscall_result_with_output(
            &mut m,
            &mut e.state,
            &mut unrelated,
            libc::SYS_writev,
            [received[1] as u64, 0xc00, 1, 0, 0, 0]
        ),
        5
    );
    assert_eq!(unrelated.take(), (Vec::new(), Vec::new()));
    assert_eq!(e.take_output(), (b"owned".to_vec(), b"owned".to_vec()));
}

#[test]
fn captured_rights_mixed_message_keeps_ordinary_and_process_proc_controls() {
    let root = TestDir::new();
    std::fs::write(root.0.join("ordinary-right"), b"ordinary").unwrap();
    let mut e = ElfExecutor::new(test_state(&root.0), true);
    let mut m = GuestMemory::new(0, 0x4000).unwrap();
    let pair = capture_rights_pair(&mut e, &mut m);
    let mut opened = Vec::new();
    for name in ["/proc/self/status", "ordinary-right"] {
        write_c_string(&mut m, 0xc00, name);
        let fd = capture_rights_call(
            &mut e,
            &m,
            libc::SYS_open,
            [0xc00, libc::O_RDONLY as u64, 0, 0, 0, 0],
        ) as i32;
        assert!(fd >= 3);
        opened.push(fd);
    }
    assert_eq!(
        capture_rights_send(&mut e, &mut m, pair[0], &[1, opened[0], opened[1]]),
        1
    );
    assert_eq!(capture_rights_count(&e), 2);
    let received = capture_rights_receive(&mut e, &mut m, pair[1], 0);
    assert_eq!(received.len(), 3);
    assert_eq!(capture_rights_count(&e), 0);
    assert_eq!(
        output_alias(&e.state, received[0]),
        Some(OutputAlias::Stdout)
    );
    assert!(e.state.fdinfo_files.contains_key(&received[1]));
    assert!(e.state.proc_files.contains_key(&received[1]));
    assert!(!e.state.capture_descriptions.contains_key(&received[1]));
    assert!(!e.state.capture_descriptions.contains_key(&received[2]));
    assert_eq!(
        capture_rights_call(
            &mut e,
            &m,
            libc::SYS_read,
            [received[2] as u64, 0xb00, 8, 0, 0, 0]
        ),
        8
    );
    assert_eq!(read_guest_bytes::<8>(&m, 0xb00).unwrap(), *b"ordinary");
    assert!(
        capture_rights_call(
            &mut e,
            &m,
            libc::SYS_read,
            [received[1] as u64, 0xb00, 64, 0, 0, 0]
        ) > 0
    );
    assert_eq!(read_guest_bytes::<5>(&m, 0xb00).unwrap(), *b"Name:");
    write_c_string(&mut m, 0xc00, "/dev/urandom");
    let random = capture_rights_call(
        &mut e,
        &m,
        libc::SYS_open,
        [0xc00, libc::O_RDONLY as u64, 0, 0, 0, 0],
    ) as i32;
    assert!(random >= 3);
    assert_eq!(
        capture_rights_send(&mut e, &mut m, pair[0], &[1, random]),
        negative_errno(libc::ENOSYS)
    );
    assert_eq!(capture_rights_count(&e), 0);
    assert_eq!(
        capture_rights_call(
            &mut e,
            &m,
            libc::SYS_recvmsg,
            [pair[1] as u64, 0x600, libc::MSG_DONTWAIT as u64, 0, 0, 0]
        ),
        negative_errno(libc::EAGAIN)
    );
}

fn capture_rights_receive_variant(
    e: &mut ElfExecutor,
    m: &mut GuestMemory,
    socket: i32,
    flags: i32,
    multi: bool,
) -> Vec<i32> {
    if !multi {
        return capture_rights_receive(e, m, socket, flags);
    }
    let iov = libc::iovec {
        iov_base: 0xa00_usize as *mut _,
        iov_len: 1,
    };
    assert_eq!(write_struct(m, 0x700, &iov), 0);
    let mut message: libc::mmsghdr = unsafe { std::mem::zeroed() };
    message.msg_hdr.msg_iov = 0x700_usize as *mut _;
    message.msg_hdr.msg_iovlen = 1;
    message.msg_hdr.msg_control = 0x800_usize as *mut _;
    message.msg_hdr.msg_controllen = 128;
    assert_eq!(write_struct(m, 0x600, &message), 0);
    assert_eq!(
        capture_rights_call(
            e,
            m,
            libc::SYS_recvmmsg,
            [socket as u64, 0x600, 1, flags as u64, 0, 0]
        ),
        1
    );
    let returned: libc::mmsghdr = read_struct(m, 0x600);
    assert_eq!(returned.msg_len, 1);
    let mut control = vec![0; returned.msg_hdr.msg_controllen];
    m.read(0x800, &mut control).unwrap();
    control_rights(&control)
}

#[test]
fn captured_rights_peek_admission_pins_metadata_before_fork_consumes_and_closes() {
    for multi in [false, true] {
        let root = TestDir::new();
        let mut peeker = ElfExecutor::new(test_state(&root.0), true);
        let mut m = GuestMemory::new(0, 0x4000).unwrap();
        let pair = capture_rights_pair(&mut peeker, &mut m);
        write_c_string(&mut m, 0xc00, "/proc/self/fd/1");
        let fd = capture_rights_call(
            &mut peeker,
            &m,
            libc::SYS_open,
            [0xc00, libc::O_WRONLY as u64, 0, 0, 0, 0],
        ) as i32;
        assert!(fd >= 3);
        let weak = Arc::downgrade(&peeker.state.capture_descriptions[&fd]);
        let mut consumer = peeker.fork_child(3, false, false).unwrap();
        assert!(!Arc::ptr_eq(&peeker.file_table, &consumer.file_table));
        assert_eq!(
            capture_rights_call(
                &mut consumer,
                &m,
                libc::SYS_close,
                [fd as u64, 0, 0, 0, 0, 0]
            ),
            0
        );
        assert_eq!(capture_rights_send(&mut peeker, &mut m, pair[0], &[fd]), 1);
        assert_eq!(
            capture_rights_call(&mut peeker, &m, libc::SYS_close, [fd as u64, 0, 0, 0, 0, 0]),
            0
        );
        let gate = capture_receive_gate(&peeker.state).unwrap();
        assert!(Arc::ptr_eq(
            &gate,
            &capture_receive_gate(&consumer.state).unwrap()
        ));
        let after_host = Arc::new(AtomicBool::new(false));
        let observed = after_host.clone();
        let host_gate = gate.clone();
        CAPTURE_RECEIVE_AFTER_HOST.with_borrow_mut(|hook| {
            assert!(hook.is_none());
            *hook = Some(Box::new(move || {
                // Real native MSG_PEEK has completed, but metadata staging has
                // not. A distinct fork cannot consume through its shared gate.
                assert!(host_gate.try_lock().is_err());
                observed.store(true, Ordering::SeqCst);
            }));
        });
        let consumed = Arc::new(AtomicBool::new(false));
        let observed = consumed.clone();
        let identity_table = peeker.state.file_identity_table.clone();
        CAPTURE_RECEIVE_AFTER_RELEASE.with_borrow_mut(|hook| {
            assert!(hook.is_none());
            *hook = Some(Box::new(move || {
                // Earliest permitted consumer interleaving, before peeker's
                // first guest copyout. It consumes and closes every returned
                // alias; only peeker's staged description then pins metadata.
                assert!(gate.try_lock().is_ok());
                assert!(identity_table.try_lock().is_ok());
                let mut memory = GuestMemory::new(0, 0x4000).unwrap();
                let returned =
                    capture_rights_receive_variant(&mut consumer, &mut memory, pair[1], 0, !multi);
                assert_eq!(returned.len(), 1);
                assert_eq!(capture_rights_count(&consumer), 0);
                assert_eq!(
                    capture_rights_call(
                        &mut consumer,
                        &memory,
                        libc::SYS_fcntl,
                        [
                            returned[0] as u64,
                            libc::F_SETFL as u64,
                            libc::O_NONBLOCK as u64,
                            0,
                            0,
                            0
                        ]
                    ),
                    0
                );
                memory.write(0xb00, b"C").unwrap();
                assert_eq!(
                    capture_rights_call(
                        &mut consumer,
                        &memory,
                        libc::SYS_write,
                        [returned[0] as u64, 0xb00, 1, 0, 0, 0]
                    ),
                    1
                );
                assert_eq!(
                    capture_rights_call(
                        &mut consumer,
                        &memory,
                        libc::SYS_close,
                        [returned[0] as u64, 0, 0, 0, 0, 0]
                    ),
                    0
                );
                drop(consumer);
                observed.store(true, Ordering::SeqCst);
            }));
        });
        let returned =
            capture_rights_receive_variant(&mut peeker, &mut m, pair[1], libc::MSG_PEEK, multi);
        assert_eq!(returned.len(), 1);
        assert!(after_host.load(Ordering::SeqCst));
        assert!(consumed.load(Ordering::SeqCst));
        assert_eq!(capture_rights_count(&peeker), 0);
        assert!(Arc::ptr_eq(
            &weak.upgrade().unwrap(),
            &peeker.state.capture_descriptions[&returned[0]]
        ));
        assert_ne!(
            capture_rights_flags(&mut peeker, &m, returned[0]) & libc::O_NONBLOCK,
            0
        );
        m.write(0xb00, b"P").unwrap();
        assert_eq!(
            capture_rights_call(
                &mut peeker,
                &m,
                libc::SYS_write,
                [returned[0] as u64, 0xb00, 1, 0, 0, 0]
            ),
            1
        );
        assert_eq!(peeker.take_output(), (b"CP".to_vec(), Vec::new()));
        assert_eq!(
            capture_rights_call(
                &mut peeker,
                &m,
                libc::SYS_close,
                [returned[0] as u64, 0, 0, 0, 0, 0]
            ),
            0
        );
        assert!(weak.upgrade().is_none());
    }
}

#[test]
fn captured_reopen_token_relocation_and_seal_failure_close_outside_guards() {
    const TEST: &str =
        "executor::tests::captured_reopen_token_relocation_and_seal_failure_close_outside_guards";
    const COMPLETE: &str = "capture token staged cleanup checked";
    if !capture_test_child(TEST, COMPLETE) {
        return;
    }
    for fail_seal in [false, true] {
        let root = TestDir::new();
        let mut e = ElfExecutor::new(test_state(&root.0), true);
        let mut m = GuestMemory::new(0, 0x4000).unwrap();
        write_c_string(&mut m, 0x100, "/proc/self/fd/1");
        let saved = [0, 1, 2].map(|fd| {
            let saved = unsafe { libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, 3) };
            assert!(saved >= 3);
            unsafe { std::fs::File::from_raw_fd(saved) }
        });
        let table = Arc::downgrade(&e.file_table);
        let transaction = e.state.signal_transaction.clone();
        let unlocked = Arc::new(AtomicBool::new(true));
        let observed = unlocked.clone();
        let relocated = Arc::new(AtomicBool::new(false));
        let saw_relocation = relocated.clone();
        e.state.file_retirement.set_probe(Some(Arc::new(move |fds| {
            let table = table.upgrade().unwrap();
            if table.try_lock().is_err() || transaction.try_lock().is_err() {
                observed.store(false, Ordering::SeqCst);
            }
            if fds.contains(&1) {
                saw_relocation.store(true, Ordering::SeqCst);
            }
        })));
        for fd in [0, 1, 2] {
            assert_eq!(unsafe { libc::close(fd) }, 0);
        }
        CAPTURE_TOKEN_FAIL_SEAL.set(fail_seal);
        let result = capture_rights_call(
            &mut e,
            &m,
            libc::SYS_open,
            [0x100, libc::O_WRONLY as u64, 0, 0, 0, 0],
        );
        CAPTURE_TOKEN_FAIL_SEAL.set(false);
        let closed_alias = if result >= 0 {
            capture_rights_call(&mut e, &m, libc::SYS_close, [result as u64, 0, 0, 0, 0, 0])
        } else {
            0
        };
        let closed = [0, 1, 2].map(|fd| unsafe { libc::fcntl(fd, libc::F_GETFD) } == -1
            && std::io::Error::last_os_error().raw_os_error() == Some(libc::EBADF));
        for (fd, saved) in [0, 1, 2].into_iter().zip(&saved) {
            assert_eq!(unsafe { libc::dup2(saved.as_raw_fd(), fd) }, fd);
        }
        e.state.file_retirement.set_probe(None);
        if fail_seal {
            assert_eq!(result, negative_errno(libc::EINVAL));
        } else {
            assert!(result >= 3);
        }
        assert_eq!(closed_alias, 0);
        assert_eq!(closed, [true; 3]);
        assert!(unlocked.load(Ordering::SeqCst));
        assert!(relocated.load(Ordering::SeqCst));
    }
    eprintln!("{COMPLETE}");
}

#[test]
fn captured_reopen_token_relocation_emfile_closes_outside_guards() {
    const TEST: &str =
        "executor::tests::captured_reopen_token_relocation_emfile_closes_outside_guards";
    const COMPLETE: &str = "capture token relocation EMFILE cleanup checked";
    if !capture_test_child(TEST, COMPLETE) {
        return;
    }
    let root = TestDir::new();
    let mut e = ElfExecutor::new(test_state(&root.0), true);
    let mut m = GuestMemory::new(0, 0x4000).unwrap();
    write_c_string(&mut m, 0x100, "/proc/self/fd/1");
    let saved = [0, 1, 2].map(|fd| {
        let saved = unsafe { libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, 3) };
        assert!(saved >= 3);
        unsafe { std::fs::File::from_raw_fd(saved) }
    });
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
        rlim_cur: original.rlim_cur.min(highest + 16),
        rlim_max: original.rlim_max,
    };
    let table = Arc::downgrade(&e.file_table);
    let transaction = e.state.signal_transaction.clone();
    let unlocked = Arc::new(AtomicBool::new(true));
    let observed = unlocked.clone();
    let retired = Arc::new(Mutex::new(std::collections::BTreeSet::new()));
    let seen = retired.clone();
    e.state.file_retirement.set_probe(Some(Arc::new(move |fds| {
        let table = table.upgrade().unwrap();
        if table.try_lock().is_err() || transaction.try_lock().is_err() {
            observed.store(false, Ordering::SeqCst);
        }
        seen.lock()
            .unwrap()
            .extend(fds.iter().copied().filter(|fd| *fd < 3));
    })));
    assert_eq!(unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &reduced) }, 0);
    let mut fillers = Vec::new();
    let exhausted = loop {
        match std::fs::File::open("/dev/null") {
            Ok(file) => fillers.push(file),
            Err(error) => break error.raw_os_error(),
        }
    };
    // Only 0/1/2 are free. The writable reopen takes 0 and memfd_create takes
    // 1; token relocation with minimum 3 must fail even though fd 2 is free.
    for fd in [0, 1, 2] {
        assert_eq!(unsafe { libc::close(fd) }, 0);
    }
    let result = capture_rights_call(
        &mut e,
        &m,
        libc::SYS_open,
        [0x100, libc::O_WRONLY as u64, 0, 0, 0, 0],
    );
    let closed = [0, 1, 2].map(|fd| unsafe { libc::fcntl(fd, libc::F_GETFD) } == -1
        && std::io::Error::last_os_error().raw_os_error() == Some(libc::EBADF));
    assert_eq!(
        unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &original) },
        0
    );
    for (fd, saved) in [0, 1, 2].into_iter().zip(&saved) {
        assert_eq!(unsafe { libc::dup2(saved.as_raw_fd(), fd) }, fd);
    }
    e.state.file_retirement.set_probe(None);
    drop(fillers);
    assert_eq!(exhausted, Some(libc::EMFILE));
    assert_eq!(result, negative_errno(libc::EMFILE));
    assert_eq!(closed, [true; 3]);
    assert!(unlocked.load(Ordering::SeqCst));
    assert_eq!(
        *retired.lock().unwrap(),
        [0, 1]
            .into_iter()
            .collect::<std::collections::BTreeSet<_>>()
    );
    assert!(e.state.files.is_empty());
    assert!(e.file_table.lock().unwrap().files.is_empty());
    eprintln!("{COMPLETE}");
}
