/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

struct ProcfsRegressionDir(PathBuf);

impl ProcfsRegressionDir {
    fn new() -> Self {
        let id = NEXT_TEST_DIR.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("kvm-procfs-{}-{id}", std::process::id()));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
}

impl Drop for ProcfsRegressionDir {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).unwrap();
    }
}

struct ProcfsRegressionFixture {
    executor: ElfExecutor,
    memory: GuestMemory,
    root: ProcfsRegressionDir,
}

fn procfs_regression_read(executor: &mut ElfExecutor, memory: &GuestMemory, fd: i64) -> Vec<u8> {
    let count = executor.execute(
        &SyscallRequest::new(libc::SYS_read as u64, [fd as u64, 0x1000, 8192, 0, 0, 0]),
        memory,
    );
    assert!(count >= 0, "read fd {fd}: {count}");
    let mut bytes = vec![0; count as usize];
    memory.read(0x1000, &mut bytes).unwrap();
    bytes
}

impl ProcfsRegressionFixture {
    fn new() -> Self {
        Self::with_capture(false)
    }

    fn with_capture(capture: bool) -> Self {
        let root = ProcfsRegressionDir::new();
        Self {
            executor: ElfExecutor::new(test_state(&root.0), capture),
            memory: GuestMemory::new(0, 0x5000).unwrap(),
            root,
        }
    }

    fn call(&mut self, number: libc::c_long, args: [u64; 6]) -> i64 {
        self.executor
            .execute(&SyscallRequest::new(number as u64, args), &self.memory)
    }

    fn path(&mut self, path: &str) {
        write_c_string(&mut self.memory, 0x100, path);
    }

    fn open(&mut self, path: &str, flags: i32) -> i64 {
        self.path(path);
        self.call(
            libc::SYS_openat,
            [libc::AT_FDCWD as u64, 0x100, flags as u64, 0, 0, 0],
        )
    }

    fn read(&mut self, fd: i64) -> Vec<u8> {
        let count = self.call(libc::SYS_read, [fd as u64, 0x1000, 8192, 0, 0, 0]);
        assert!(count >= 0, "read fd {fd}: {count}");
        let mut bytes = vec![0; count as usize];
        self.memory.read(0x1000, &mut bytes).unwrap();
        bytes
    }

    fn readlinkat(&mut self, dirfd: i64, path: &str) -> Result<Vec<u8>, i64> {
        self.path(path);
        self.memory.write(0x1000, &[0xa5; 128]).unwrap();
        let count = self.call(
            libc::SYS_readlinkat,
            [dirfd as u64, 0x100, 0x1000, 128, 0, 0],
        );
        if count < 0 {
            return Err(count);
        }
        let mut bytes = vec![0; count as usize];
        self.memory.read(0x1000, &mut bytes).unwrap();
        let mut next = [0];
        self.memory.read(0x1000 + count as u64, &mut next).unwrap();
        assert_eq!(next, [0xa5], "readlinkat must not append a NUL");
        Ok(bytes)
    }

    fn close(&mut self, fd: i64) {
        assert_eq!(self.call(libc::SYS_close, [fd as u64, 0, 0, 0, 0, 0]), 0);
    }

    fn right_message(&mut self, fd: Option<i32>) {
        let control = rights_control(&[fd.unwrap_or(0)]);
        self.memory.write(0x700, b"x").unwrap();
        self.memory.write(0x600, &control).unwrap();
        let iov = libc::iovec {
            iov_base: 0x700 as *mut libc::c_void,
            iov_len: 1,
        };
        assert_eq!(write_struct(&mut self.memory, 0x300, &iov), 0);
        // SAFETY: libc::msghdr is plain data and permits zero initialization.
        let mut message: libc::msghdr = unsafe { std::mem::zeroed() };
        message.msg_iov = 0x300 as *mut libc::iovec;
        message.msg_iovlen = 1;
        message.msg_control = 0x600 as *mut libc::c_void;
        message.msg_controllen = control.len();
        assert_eq!(write_struct(&mut self.memory, 0x400, &message), 0);
    }
}

#[test]
fn procfs_regression_fd_parent_alias_and_directory_anchor() {
    let mut f = ProcfsRegressionFixture::new();
    let proc_root = f.open("/proc", libc::O_RDONLY | libc::O_DIRECTORY);
    assert!(proc_root >= 0);
    assert_eq!(
        f.readlinkat(proc_root, ""),
        Err(negative_errno(libc::ENOENT)),
        "an empty readlink path requires a symlink descriptor"
    );
    f.close(proc_root);
    let target = f.root.0.join("target");
    std::fs::write(&target, b"target").unwrap();
    let fd = f.open("target", libc::O_RDONLY);
    assert_eq!(fd, 3);
    let expected = target.as_os_str().as_bytes();
    for path in [
        format!("/proc/self/fd/../fd/{fd}"),
        format!("/proc/{}/fd/./../fd/{fd}", f.executor.state.pid),
        format!("/proc//self//fd/../fd/{fd}"),
    ] {
        assert_eq!(
            f.readlinkat(libc::AT_FDCWD as i64, &path).unwrap(),
            expected
        );
    }
    let anchor = f.open("/proc/self/fd", libc::O_PATH | libc::O_DIRECTORY);
    assert!(anchor >= 0, "open guest proc-fd anchor: {anchor}");
    assert_eq!(f.readlinkat(anchor, ""), Err(negative_errno(libc::ENOENT)));
    assert_eq!(
        f.call(
            libc::SYS_fcntl,
            [anchor as u64, libc::F_GETFL as u64, 0, 0, 0, 0]
        ),
        (libc::O_PATH | libc::O_DIRECTORY) as i64
    );
    assert_eq!(
        f.call(libc::SYS_fstat, [anchor as u64, 0x4000, 0, 0, 0, 0]),
        0
    );
    let descriptor: libc::stat = read_struct(&f.memory, 0x4000);
    assert_eq!(descriptor.st_mode & libc::S_IFMT, libc::S_IFDIR);
    f.path("/proc/self/fd/../fd");
    assert_eq!(
        f.call(libc::SYS_newfstatat, [u64::MAX, 0x100, 0x4100, 0, 0, 0]),
        0,
        "absolute paths ignore an invalid dirfd"
    );
    let absolute: libc::stat = read_struct(&f.memory, 0x4100);
    assert_eq!(
        (absolute.st_mode, absolute.st_ino),
        (descriptor.st_mode, descriptor.st_ino)
    );
    f.path("../fd");
    assert_eq!(
        f.call(
            libc::SYS_statx,
            [
                anchor as u64,
                0x100,
                0,
                libc::STATX_BASIC_STATS as u64,
                0x4200,
                0
            ]
        ),
        0
    );
    let relative: libc::statx = read_struct(&f.memory, 0x4200);
    assert_eq!(
        (relative.stx_mode as u32, relative.stx_ino),
        (descriptor.st_mode, descriptor.st_ino)
    );
    assert_eq!(f.readlinkat(anchor, &fd.to_string()).unwrap(), expected);
    let alias = f.call(libc::SYS_dup, [anchor as u64, 0, 0, 0, 0, 0]);
    assert!(alias >= 0);
    f.close(anchor);
    assert_eq!(
        f.readlinkat(alias, &format!("../fd/{fd}")).unwrap(),
        expected
    );
}

#[test]
fn procfs_regression_parent_components_do_not_skip_lookup_or_fd_targets() {
    let mut f = ProcfsRegressionFixture::new();
    std::fs::write(f.root.0.join("regular"), b"regular").unwrap();
    std::fs::create_dir(f.root.0.join("directory")).unwrap();
    std::os::unix::fs::symlink("regular", f.root.0.join("link")).unwrap();
    let regular = f.open("regular", libc::O_RDONLY);
    let directory = f.open("directory", libc::O_RDONLY | libc::O_DIRECTORY);
    assert!(regular >= 0 && directory >= 0);
    for (path, error) in [
        (format!("/proc/self/status/../fd/{regular}"), libc::ENOTDIR),
        (format!("/proc/self/missing/../fd/{regular}"), libc::ENOENT),
        (format!("/proc/self/fd/{regular}/.."), libc::ENOTDIR),
        (format!("/proc/self/fd/99/../{regular}"), libc::ENOENT),
        (format!("/proc/self/fd/{regular}/"), libc::ENOTDIR),
        ("/proc/self/fdinfo/fd/3".to_owned(), libc::ENOENT),
        ("/proc/self/fdinfo/task/1/status".to_owned(), libc::ENOENT),
    ] {
        assert_eq!(
            f.readlinkat(libc::AT_FDCWD as i64, &path),
            Err(negative_errno(error)),
            "{path}"
        );
    }
    assert_eq!(
        f.readlinkat(
            libc::AT_FDCWD as i64,
            &format!("/proc/self/fd/{directory}/../link")
        )
        .unwrap(),
        b"regular",
        "fd/.. follows the held directory target before resolving the suffix"
    );
    f.path(&format!("/proc/self/fd/{directory}/../regular"));
    assert_eq!(
        f.call(
            libc::SYS_newfstatat,
            [libc::AT_FDCWD as u64, 0x100, 0x4000, 0, 0, 0]
        ),
        0
    );
    let via_directory: libc::stat = read_struct(&f.memory, 0x4000);
    f.path("regular");
    assert_eq!(
        f.call(
            libc::SYS_newfstatat,
            [libc::AT_FDCWD as u64, 0x100, 0x4100, 0, 0, 0]
        ),
        0
    );
    let ordinary: libc::stat = read_struct(&f.memory, 0x4100);
    assert_eq!(
        (
            via_directory.st_dev,
            via_directory.st_ino,
            via_directory.st_mode
        ),
        (ordinary.st_dev, ordinary.st_ino, ordinary.st_mode)
    );
    assert_eq!(
        f.readlinkat(
            libc::AT_FDCWD as i64,
            &format!("/proc/self/fd/{directory}/")
        ),
        Err(negative_errno(libc::EINVAL)),
        "a trailing slash follows the symlink to a directory"
    );
}

#[test]
fn procfs_regression_task_symlinks_and_nofollow() {
    let mut f = ProcfsRegressionFixture::new();
    assert_eq!(
        f.readlinkat(libc::AT_FDCWD as i64, "/proc/self").unwrap(),
        b"1"
    );
    assert_eq!(
        f.readlinkat(libc::AT_FDCWD as i64, "/proc/thread-self")
            .unwrap(),
        b"1/task/1"
    );
    assert_eq!(
        f.readlinkat(libc::AT_FDCWD as i64, "/proc/1"),
        Err(negative_errno(libc::EINVAL))
    );
    assert_eq!(
        f.open("/proc/self", libc::O_RDONLY | libc::O_NOFOLLOW),
        negative_errno(libc::ELOOP)
    );
    for flags in [
        libc::O_CREAT | libc::O_EXCL,
        libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW,
    ] {
        assert_eq!(f.open("/proc/self", flags), negative_errno(libc::EEXIST));
    }
    assert_eq!(
        f.open("/proc/1", libc::O_RDONLY | libc::O_TRUNC),
        negative_errno(libc::EISDIR)
    );
    let link = f.open("/proc/thread-self", libc::O_PATH | libc::O_NOFOLLOW);
    assert!(link >= 0);
    assert_eq!(f.readlinkat(link, "").unwrap(), b"1/task/1");
    assert_eq!(
        f.call(libc::SYS_fstat, [link as u64, 0x4000, 0, 0, 0, 0]),
        0
    );
    let stat: libc::stat = read_struct(&f.memory, 0x4000);
    assert_eq!(stat.st_mode & libc::S_IFMT, libc::S_IFLNK);
    assert_eq!(stat.st_size, 8);
    let followed = f.open(
        "/proc/thread-self/",
        libc::O_PATH | libc::O_NOFOLLOW | libc::O_DIRECTORY,
    );
    assert!(
        followed >= 0,
        "trailing slash follows the task symlink: {followed}"
    );
}

#[test]
fn procfs_regression_fd_anchor_retains_original_owner_after_fork() {
    let mut f = ProcfsRegressionFixture::new();
    let target = f.root.0.join("parent-target");
    std::fs::write(&target, b"parent").unwrap();
    std::fs::write(f.root.0.join("child-target"), b"child").unwrap();
    let target_fd = f.open("parent-target", libc::O_RDONLY);
    let anchor = f.open("/proc/self/fd", libc::O_PATH | libc::O_DIRECTORY);
    assert!(anchor >= 0);
    let process_anchor = f.open("/proc/self", libc::O_PATH | libc::O_DIRECTORY);
    assert!(process_anchor >= 0);
    let owner_stat = f.open("/proc/self/stat", libc::O_RDONLY);
    assert!(owner_stat >= 0);
    let expected_stat = f.read(owner_stat);
    f.close(owner_stat);
    let mut child = f.executor.fork_child(2, false, false).unwrap();
    assert_eq!(
        child.execute(
            &SyscallRequest::new(libc::SYS_close as u64, [target_fd as u64, 0, 0, 0, 0, 0]),
            &f.memory
        ),
        0
    );
    f.path("child-target");
    assert_eq!(
        child.execute(
            &SyscallRequest::new(
                libc::SYS_openat as u64,
                [libc::AT_FDCWD as u64, 0x100, libc::O_RDONLY as u64, 0, 0, 0]
            ),
            &f.memory
        ),
        target_fd
    );
    f.path("child-view");
    assert_eq!(
        child.execute(
            &SyscallRequest::new(
                libc::SYS_prctl as u64,
                [libc::PR_SET_NAME as u64, 0x100, 0, 0, 0, 0]
            ),
            &f.memory,
        ),
        0
    );
    f.path("stat");
    let foreign_stat = child.execute(
        &SyscallRequest::new(
            libc::SYS_openat as u64,
            [process_anchor as u64, 0x100, libc::O_RDONLY as u64, 0, 0, 0],
        ),
        &f.memory,
    );
    assert!(foreign_stat >= 0, "new stat open retains original owner");
    assert_eq!(
        child.execute(
            &SyscallRequest::new(
                libc::SYS_read as u64,
                [foreign_stat as u64, 0x1000, 8192, 0, 0, 0]
            ),
            &f.memory,
        ),
        expected_stat.len() as i64
    );
    let mut foreign_bytes = vec![0; expected_stat.len()];
    f.memory.read(0x1000, &mut foreign_bytes).unwrap();
    assert_eq!(foreign_bytes, expected_stat);
    assert_eq!(
        child.execute(
            &SyscallRequest::new(libc::SYS_close as u64, [foreign_stat as u64, 0, 0, 0, 0, 0]),
            &f.memory,
        ),
        0
    );
    f.path(&target_fd.to_string());
    let count = child.execute(
        &SyscallRequest::new(
            libc::SYS_readlinkat as u64,
            [anchor as u64, 0x100, 0x1000, 128, 0, 0],
        ),
        &f.memory,
    );
    assert_eq!(count, target.as_os_str().as_bytes().len() as i64);
    let mut bytes = vec![0; count as usize];
    f.memory.read(0x1000, &mut bytes).unwrap();
    assert_eq!(bytes, target.as_os_str().as_bytes());
    drop(f.executor);
    assert_eq!(
        child.execute(
            &SyscallRequest::new(
                libc::SYS_readlinkat as u64,
                [anchor as u64, 0x100, 0x1000, 128, 0, 0]
            ),
            &f.memory
        ),
        negative_errno(libc::ENOENT)
    );
    let reused = child.thread_child(1).unwrap();
    assert_eq!(
        child.execute(
            &SyscallRequest::new(
                libc::SYS_readlinkat as u64,
                [anchor as u64, 0x100, 0x1000, 128, 0, 0]
            ),
            &f.memory
        ),
        negative_errno(libc::ENOENT),
        "a new task with the old numeric tid never rebinds the anchor"
    );
    drop(reused);
}

#[test]
fn procfs_regression_fd_anchor_transfer_preserves_owner_and_metadata() {
    let mut f = ProcfsRegressionFixture::new();
    let target = f.root.0.join("parent-target");
    std::fs::write(&target, b"parent").unwrap();
    std::fs::write(f.root.0.join("child-target"), b"child").unwrap();
    let fd = f.open("parent-target", libc::O_RDONLY);
    let anchor = f.open("/proc/self/fd", libc::O_PATH | libc::O_DIRECTORY);
    assert!(fd >= 0 && anchor >= 0);
    assert_eq!(
        f.call(
            libc::SYS_socketpair,
            [
                libc::AF_UNIX as u64,
                libc::SOCK_DGRAM as u64,
                0,
                0x800,
                0,
                0
            ]
        ),
        0
    );
    let sockets: [i32; 2] = read_struct(&f.memory, 0x800);
    let mut child = f.executor.fork_child(2, false, false).unwrap();
    for old in [anchor, fd] {
        assert_eq!(
            child.execute(
                &SyscallRequest::new(libc::SYS_close as u64, [old as u64, 0, 0, 0, 0, 0]),
                &f.memory
            ),
            0
        );
    }
    f.path("child-target");
    assert_eq!(
        child.execute(
            &SyscallRequest::new(
                libc::SYS_openat as u64,
                [libc::AT_FDCWD as u64, 0x100, libc::O_RDONLY as u64, 0, 0, 0]
            ),
            &f.memory
        ),
        fd
    );
    f.right_message(Some(anchor as i32));
    assert_eq!(
        f.call(libc::SYS_sendmsg, [sockets[0] as u64, 0x400, 0, 0, 0, 0]),
        1
    );
    f.close(anchor);
    f.right_message(None);
    assert_eq!(
        child.execute(
            &SyscallRequest::new(
                libc::SYS_recvmsg as u64,
                [sockets[1] as u64, 0x400, 0, 0, 0, 0]
            ),
            &f.memory
        ),
        1
    );
    let message: libc::msghdr = read_struct(&f.memory, 0x400);
    let mut control = vec![0; message.msg_controllen];
    f.memory.read(0x600, &mut control).unwrap();
    let received = control_rights(&control);
    assert_eq!(received.len(), 1);
    let anchor = received[0];
    assert_eq!(
        child.execute(
            &SyscallRequest::new(libc::SYS_fstat as u64, [anchor as u64, 0x4000, 0, 0, 0, 0]),
            &f.memory
        ),
        0
    );
    let stat: libc::stat = read_struct(&f.memory, 0x4000);
    assert_eq!(stat.st_mode & libc::S_IFMT, libc::S_IFDIR);
    f.path(&fd.to_string());
    let count = child.execute(
        &SyscallRequest::new(
            libc::SYS_readlinkat as u64,
            [anchor as u64, 0x100, 0x1000, 128, 0, 0],
        ),
        &f.memory,
    );
    assert_eq!(count, target.as_os_str().as_bytes().len() as i64);
    let mut bytes = vec![0; count as usize];
    f.memory.read(0x1000, &mut bytes).unwrap();
    assert_eq!(bytes, target.as_os_str().as_bytes());
}

#[test]
fn procfs_regression_fd_anchor_preserves_owner_capture_aliases() {
    let mut f = ProcfsRegressionFixture::with_capture(true);
    let alias = f.call(libc::SYS_dup, [1, 0, 0, 0, 0, 0]);
    assert!(alias >= 0);
    let anchor = f.open("/proc/self/fd", libc::O_PATH | libc::O_DIRECTORY);
    assert!(anchor >= 0);
    let mut child = f.executor.fork_child(2, false, false).unwrap();
    for fd in [1, 2, alias] {
        let expected = f
            .readlinkat(libc::AT_FDCWD as i64, &format!("/proc/self/fd/{fd}"))
            .unwrap();
        assert!(expected.starts_with(b"pipe:["));
        f.path(&fd.to_string());
        let count = child.execute(
            &SyscallRequest::new(
                libc::SYS_readlinkat as u64,
                [anchor as u64, 0x100, 0x1000, 128, 0, 0],
            ),
            &f.memory,
        );
        assert_eq!(count, expected.len() as i64);
        let mut actual = vec![0; count as usize];
        f.memory.read(0x1000, &mut actual).unwrap();
        assert_eq!(actual, expected, "the anchor retains its owner's capture");
    }
}

#[test]
fn procfs_regression_exchanged_anchors_open_without_two_table_locking() {
    let mut f = ProcfsRegressionFixture::new();
    for (directory, content) in [
        ("parent-dir", b"parent".as_slice()),
        ("child-dir", b"child"),
    ] {
        std::fs::create_dir(f.root.0.join(directory)).unwrap();
        std::fs::write(f.root.0.join(directory).join("file"), content).unwrap();
    }
    let target = f.open("parent-dir", libc::O_PATH | libc::O_DIRECTORY);
    let parent_anchor = f.open("/proc/self/fd", libc::O_PATH | libc::O_DIRECTORY);
    assert!(target >= 0 && parent_anchor >= 0);
    assert_eq!(
        f.call(
            libc::SYS_socketpair,
            [
                libc::AF_UNIX as u64,
                libc::SOCK_DGRAM as u64,
                0,
                0x800,
                0,
                0
            ]
        ),
        0
    );
    let sockets: [i32; 2] = read_struct(&f.memory, 0x800);
    let mut child = f.executor.fork_child(2, false, false).unwrap();
    assert_eq!(
        child.execute(
            &SyscallRequest::new(libc::SYS_close as u64, [target as u64, 0, 0, 0, 0, 0]),
            &f.memory,
        ),
        0
    );
    f.path("child-dir");
    assert_eq!(
        child.execute(
            &SyscallRequest::new(
                libc::SYS_openat as u64,
                [
                    libc::AT_FDCWD as u64,
                    0x100,
                    (libc::O_PATH | libc::O_DIRECTORY) as u64,
                    0,
                    0,
                    0
                ]
            ),
            &f.memory,
        ),
        target
    );
    f.path("/proc/self/fd");
    let child_anchor = child.execute(
        &SyscallRequest::new(
            libc::SYS_openat as u64,
            [
                libc::AT_FDCWD as u64,
                0x100,
                (libc::O_PATH | libc::O_DIRECTORY) as u64,
                0,
                0,
                0,
            ],
        ),
        &f.memory,
    );
    assert!(child_anchor >= 0);
    f.right_message(Some(child_anchor as i32));
    assert_eq!(
        child.execute(
            &SyscallRequest::new(
                libc::SYS_sendmsg as u64,
                [sockets[1] as u64, 0x400, 0, 0, 0, 0]
            ),
            &f.memory,
        ),
        1
    );
    f.right_message(None);
    assert_eq!(
        f.call(libc::SYS_recvmsg, [sockets[0] as u64, 0x400, 0, 0, 0, 0]),
        1
    );
    let message: libc::msghdr = read_struct(&f.memory, 0x400);
    let mut control = vec![0; message.msg_controllen];
    f.memory.read(0x600, &mut control).unwrap();
    let rights = control_rights(&control);
    assert_eq!(rights.len(), 1);
    let child_anchor = rights[0];
    f.path(&format!("{target}/file"));
    let ProcfsRegressionFixture {
        executor: mut parent,
        memory,
        root,
    } = f;
    let start = Arc::new(std::sync::Barrier::new(2));
    let (done, results) = std::sync::mpsc::channel();
    let parent_memory = memory.clone();
    let child_memory = memory.clone();
    let parent_start = start.clone();
    let parent_done = done.clone();
    let parent_thread = std::thread::spawn(move || {
        parent_start.wait();
        let fd = parent.execute(
            &SyscallRequest::new(
                libc::SYS_openat as u64,
                [child_anchor as u64, 0x100, libc::O_RDONLY as u64, 0, 0, 0],
            ),
            &parent_memory,
        );
        parent_done.send((parent, fd, b"child".as_slice())).unwrap();
    });
    let child_thread = std::thread::spawn(move || {
        start.wait();
        let fd = child.execute(
            &SyscallRequest::new(
                libc::SYS_openat as u64,
                [parent_anchor as u64, 0x100, libc::O_RDONLY as u64, 0, 0, 0],
            ),
            &child_memory,
        );
        done.send((child, fd, b"parent".as_slice())).unwrap();
    });
    // Bound a lock-order regression without letting a deadlocked worker stall
    // the test process. Keep both owners alive while checking either result.
    let mut completed = (0..2)
        .map(|_| {
            results
                .recv_timeout(std::time::Duration::from_secs(5))
                .expect("exchanged proc anchors made no progress")
        })
        .collect::<Vec<_>>();
    parent_thread.join().unwrap();
    child_thread.join().unwrap();
    for (executor, fd, expected) in &mut completed {
        assert!(*fd >= 0, "open through original foreign owner: {fd}");
        assert_eq!(procfs_regression_read(executor, &memory, *fd), *expected);
    }
    drop(completed);
    drop(root);
}

#[test]
fn procfs_regression_prepared_open_pins_target_and_preserves_fresh_caller_state() {
    let mut f = ProcfsRegressionFixture::new();
    for (directory, content) in [("old-dir", b"old".as_slice()), ("new-dir", b"new")] {
        std::fs::create_dir(f.root.0.join(directory)).unwrap();
        std::fs::write(f.root.0.join(directory).join("file"), content).unwrap();
    }
    std::fs::write(f.root.0.join("concurrent"), b"concurrent").unwrap();
    let target = f.open("old-dir", libc::O_PATH | libc::O_DIRECTORY);
    let anchor = f.open("/proc/self/fd", libc::O_PATH | libc::O_DIRECTORY);
    assert!(target >= 0 && anchor >= 0);
    let mut child = f.executor.fork_child(2, false, false).unwrap();
    let mut sibling = child.thread_child(3).unwrap();
    f.path(&format!("{target}/file"));
    let prepared = prepare_open_file(
        &f.memory,
        &child.state,
        anchor as i32,
        0x100,
        libc::O_RDONLY as u64,
        0,
        false,
    )
    .unwrap();
    f.close(target);
    assert_eq!(f.open("new-dir", libc::O_PATH | libc::O_DIRECTORY), target);
    f.path("concurrent");
    let concurrent = sibling.execute(
        &SyscallRequest::new(
            libc::SYS_openat as u64,
            [libc::AT_FDCWD as u64, 0x100, libc::O_RDONLY as u64, 0, 0, 0],
        ),
        &f.memory,
    );
    assert!(concurrent >= 0);
    // The imported bytes and original owned target must survive both guest
    // pathname mutation and foreign fd close/reuse during unlocked lookup.
    f.path("missing-after-import");
    let table = child.file_table.clone();
    let _retirement = child.state.file_retirement.hold();
    let opened = {
        let mut table = table.lock().unwrap();
        table.install(&mut child.state).unwrap();
        let fd = open_prepared_file(&mut child.state, prepared, false);
        table.update_from_elf(&child.state).unwrap();
        fd
    };
    assert!(opened >= 0 && opened != concurrent);
    assert_eq!(
        procfs_regression_read(&mut child, &f.memory, opened),
        b"old"
    );
    assert_eq!(
        procfs_regression_read(&mut child, &f.memory, concurrent),
        b"concurrent"
    );
    assert_eq!(
        sibling.execute(
            &SyscallRequest::new(
                libc::SYS_lseek as u64,
                [concurrent as u64, 0, libc::SEEK_SET as u64, 0, 0, 0]
            ),
            &f.memory,
        ),
        0
    );
    assert_eq!(
        procfs_regression_read(&mut sibling, &f.memory, concurrent),
        b"concurrent"
    );
    f.path("/proc/2/task/3/status");
    let process = prepare_open_file(
        &f.memory,
        &child.state,
        libc::AT_FDCWD,
        0x100,
        libc::O_RDONLY as u64,
        0,
        false,
    )
    .unwrap();
    f.path(&format!("/proc/2/task/3/fdinfo/{opened}"));
    let info = prepare_open_file(
        &f.memory,
        &child.state,
        libc::AT_FDCWD,
        0x100,
        libc::O_RDONLY as u64,
        0,
        false,
    )
    .unwrap();
    drop(sibling);
    let _reused = child.thread_child(3).unwrap();
    for (prepared, expected) in [(process, libc::ESRCH), (info, libc::ENOENT)] {
        let fd = {
            let mut table = table.lock().unwrap();
            table.install(&mut child.state).unwrap();
            let fd = open_prepared_file(&mut child.state, prepared, false);
            table.update_from_elf(&child.state).unwrap();
            fd
        };
        assert!(fd >= 0);
        assert_eq!(
            child.execute(
                &SyscallRequest::new(libc::SYS_read as u64, [fd as u64, 0x1000, 8192, 0, 0, 0]),
                &f.memory,
            ),
            negative_errno(expected),
            "staged descriptions never reselect a reused numeric tid"
        );
    }
}

#[test]
fn procfs_regression_thread_self_binds_nonleader_and_live_name() {
    let mut f = ProcfsRegressionFixture::new();
    let process = f.open("/proc/self/status", libc::O_RDONLY);
    assert!(process >= 0);
    let mut worker = f.executor.thread_child(3).unwrap();
    f.path("worker-before");
    assert_eq!(
        worker.execute(
            &SyscallRequest::new(
                libc::SYS_prctl as u64,
                [libc::PR_SET_NAME as u64, 0x100, 0, 0, 0, 0]
            ),
            &f.memory
        ),
        0
    );
    f.path("/proc/thread-self/status");
    let thread = worker.execute(
        &SyscallRequest::new(
            libc::SYS_openat as u64,
            [libc::AT_FDCWD as u64, 0x100, libc::O_RDONLY as u64, 0, 0, 0],
        ),
        &f.memory,
    );
    assert!(thread >= 0, "open nonleader thread-self: {thread}");
    f.path("/proc/thread-self");
    let task_directory = worker.execute(
        &SyscallRequest::new(
            libc::SYS_openat as u64,
            [
                libc::AT_FDCWD as u64,
                0x100,
                (libc::O_PATH | libc::O_DIRECTORY) as u64,
                0,
                0,
                0,
            ],
        ),
        &f.memory,
    );
    assert!(
        task_directory >= 0,
        "open nonleader task directory: {task_directory}"
    );
    let explicit = f.open("/proc/1/task/3/status", libc::O_RDONLY);
    assert!(
        explicit >= 0,
        "open explicitly selected nonleader: {explicit}"
    );
    f.path("status");
    let relative = f.call(
        libc::SYS_openat,
        [task_directory as u64, 0x100, libc::O_RDONLY as u64, 0, 0, 0],
    );
    assert!(
        relative >= 0,
        "foreign open relative to worker task anchor: {relative}"
    );
    f.path("worker-after");
    assert_eq!(
        worker.execute(
            &SyscallRequest::new(
                libc::SYS_prctl as u64,
                [libc::PR_SET_NAME as u64, 0x100, 0, 0, 0, 0]
            ),
            &f.memory
        ),
        0
    );
    let status = String::from_utf8(f.read(thread)).unwrap();
    assert!(status.contains("Tgid:\t1\n"), "{status}");
    assert!(status.contains("Pid:\t3\n"), "{status}");
    assert!(status.starts_with("Name:\tworker-after\n"), "{status}");
    assert_eq!(String::from_utf8(f.read(explicit)).unwrap(), status);
    assert_eq!(String::from_utf8(f.read(relative)).unwrap(), status);
    assert_eq!(f.call(libc::SYS_umask, [0o077, 0, 0, 0, 0, 0]), 0o022);
    f.path("status");
    let after_umask = f.call(
        libc::SYS_openat,
        [task_directory as u64, 0x100, libc::O_RDONLY as u64, 0, 0, 0],
    );
    assert!(after_umask >= 0);
    let after_umask_status = String::from_utf8(f.read(after_umask)).unwrap();
    assert!(after_umask_status.contains("Umask:\t0077\n"));
    f.close(after_umask);
    let leader = String::from_utf8(f.read(process)).unwrap();
    assert!(leader.contains("Pid:\t1\n"), "{leader}");
    assert!(leader.starts_with("Name:\ttest\n"), "{leader}");
    let duplicate = f.call(libc::SYS_dup, [thread as u64, 0, 0, 0, 0, 0]);
    assert!(duplicate >= 0);
    assert_eq!(
        f.call(
            libc::SYS_lseek,
            [duplicate as u64, 0, libc::SEEK_SET as u64, 0, 0, 0]
        ),
        0
    );
    assert_eq!(String::from_utf8(f.read(duplicate)).unwrap(), status);
    drop(worker);
    let reused = f.executor.thread_child(3).unwrap();
    assert_eq!(
        f.call(
            libc::SYS_lseek,
            [duplicate as u64, 0, libc::SEEK_SET as u64, 0, 0, 0]
        ),
        0
    );
    assert_eq!(
        f.call(libc::SYS_read, [duplicate as u64, 0x1000, 8192, 0, 0, 0]),
        negative_errno(libc::ESRCH),
        "an opened status description cannot rebind to a reused tid"
    );
    drop(reused);
}
