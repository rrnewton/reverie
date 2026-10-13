/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

#[derive(Debug)]
struct OwnedProcDirent {
    inode: u64,
    next: i64,
    kind: u8,
    name: Vec<u8>,
}

fn owned_proc_dirents(bytes: &[u8]) -> Vec<OwnedProcDirent> {
    let mut entries = Vec::new();
    let mut offset = 0;
    while offset < bytes.len() {
        assert!(bytes.len() - offset >= 24, "truncated dirent header");
        let record = &bytes[offset..];
        let length = u16::from_le_bytes(record[16..18].try_into().unwrap()) as usize;
        assert!(length >= 24 && length % 8 == 0, "invalid dirent size {length}");
        assert!(length <= record.len(), "dirent exceeds returned byte count");
        let end = record[19..length]
            .iter()
            .position(|byte| *byte == 0)
            .expect("unterminated directory name");
        let name = record[19..19 + end].to_vec();
        assert!(!name.is_empty() && !name.contains(&b'/'));
        let inode = u64::from_le_bytes(record[..8].try_into().unwrap());
        let next = i64::from_le_bytes(record[8..16].try_into().unwrap());
        assert_ne!(inode, 0, "an emitted entry has an object identity");
        assert!(next >= 0, "negative directory resume cookie");
        let kind = record[18];
        assert!(matches!(kind, libc::DT_DIR | libc::DT_REG | libc::DT_LNK));
        entries.push(OwnedProcDirent {
            inode,
            next,
            kind,
            name,
        });
        offset += length;
    }
    entries
}

fn owned_proc_read_directory(
    executor: &mut ElfExecutor,
    memory: &GuestMemory,
    fd: i64,
    address: u64,
    count: u64,
) -> Vec<OwnedProcDirent> {
    let result = executor.execute(
        &SyscallRequest::new(libc::SYS_getdents64 as u64, [fd as u64, address, count, 0, 0, 0]),
        memory,
    );
    assert!(result >= 0, "getdents64 on an owned directory: {result}");
    let mut bytes = vec![0; result as usize];
    memory.read(address, &mut bytes).unwrap();
    owned_proc_dirents(&bytes)
}

fn owned_proc_rewind(f: &mut ProcfsRegressionFixture, fd: i64) {
    assert_eq!(
        f.call(libc::SYS_lseek, [fd as u64, 0, libc::SEEK_SET as u64, 0, 0, 0]),
        0
    );
}

fn owned_proc_fstat(f: &mut ProcfsRegressionFixture, fd: i64) -> libc::stat {
    assert_eq!(f.call(libc::SYS_fstat, [fd as u64, 0x4000, 0, 0, 0, 0]), 0);
    read_struct(&f.memory, 0x4000)
}

#[test]
fn owned_proc_registered_root_names_resolve_in_the_captured_run() {
    let mut f = ProcfsRegressionFixture::new();
    let own_pid = f.call(libc::SYS_getpid, [0; 6]);
    let mut child = f.executor.fork_child(2, false, false).unwrap();
    let child_pid = child.execute(&SyscallRequest::new(libc::SYS_getpid as u64, [0; 6]), &f.memory);
    let _worker = child.thread_child(3).unwrap();
    let root = f.open("/proc", libc::O_RDONLY | libc::O_DIRECTORY);
    assert!(root >= 0);
    let root_stat = owned_proc_fstat(&mut f, root);
    assert_eq!(root_stat.st_mode & libc::S_IFMT, libc::S_IFDIR);
    let entries = owned_proc_read_directory(&mut f.executor, &f.memory, root, 0x1000, 8192);
    assert_eq!(entries.iter().find(|entry| entry.name == b".").unwrap().inode, root_stat.st_ino);
    let mut numeric = Vec::new();
    for entry in entries.iter().filter(|entry| entry.name.iter().all(u8::is_ascii_digit)) {
        assert_eq!(entry.kind, libc::DT_DIR);
        let pid: i64 = std::str::from_utf8(&entry.name).unwrap().parse().unwrap();
        numeric.push(pid);
        f.path(&format!("/proc/{pid}"));
        assert_eq!(
            f.call(libc::SYS_newfstatat, [libc::AT_FDCWD as u64, 0x100, 0x4000, 0, 0, 0]),
            0,
            "every emitted PID resolves through this namespace"
        );
        let selected: libc::stat = read_struct(&f.memory, 0x4000);
        assert_eq!(selected.st_mode & libc::S_IFMT, libc::S_IFDIR);
        assert_eq!(selected.st_ino, entry.inode);
    }
    numeric.sort_unstable();
    let mut admitted = vec![own_pid, child_pid];
    admitted.sort_unstable();
    assert_eq!(numeric, admitted, "root lists processes, not host tasks or nonleaders");
    assert!(owned_proc_read_directory(&mut f.executor, &f.memory, root, 0x1000, 8192).is_empty());
}

#[test]
fn owned_proc_directory_dup_shares_cookie_and_reopen_starts_a_new_description() {
    let mut f = ProcfsRegressionFixture::new();
    let root = f.open("/proc", libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW);
    assert!(root >= 0);
    let status_flags = f.call(libc::SYS_fcntl, [root as u64, libc::F_GETFL as u64, 0, 0, 0, 0]);
    assert_eq!(status_flags & i64::from(libc::O_ACCMODE | libc::O_DIRECTORY | libc::O_NOFOLLOW),
        i64::from(libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW));
    assert_eq!(f.call(libc::SYS_fchdir, [root as u64, 0, 0, 0, 0, 0]), negative_errno(libc::EACCES),
        "the existing procfs cwd restriction observes the logical mount, not its carrier type");
    assert_eq!(f.call(libc::SYS_lseek, [root as u64, 0, libc::SEEK_CUR as u64, 0, 0, 0]), 0);
    let duplicate = f.call(libc::SYS_dup, [root as u64, 0, 0, 0, 0, 0]);
    assert!(duplicate >= 0);
    let first = owned_proc_read_directory(&mut f.executor, &f.memory, root, 0x1000, 24);
    assert_eq!(first.len(), 1);
    assert_eq!((&first[0].name[..], first[0].next), (&b"."[..], 1));
    let second = owned_proc_read_directory(&mut f.executor, &f.memory, duplicate, 0x1000, 24);
    assert_eq!(second.len(), 1);
    assert_eq!((&second[0].name[..], second[0].next), (&b".."[..], 2));
    assert_eq!(f.call(libc::SYS_lseek, [root as u64, 0, libc::SEEK_CUR as u64, 0, 0, 0]), 2);
    assert_eq!(
        f.call(libc::SYS_lseek, [root as u64, (-1i64) as u64, libc::SEEK_SET as u64, 0, 0, 0]),
        negative_errno(libc::EINVAL)
    );
    assert_eq!(f.call(libc::SYS_lseek, [duplicate as u64, 0, libc::SEEK_CUR as u64, 0, 0, 0]), 2);

    let reopened = f.open(&format!("/proc/self/fd/{root}"), libc::O_RDONLY | libc::O_DIRECTORY);
    assert!(reopened >= 0);
    let reopened_first = owned_proc_read_directory(&mut f.executor, &f.memory, reopened, 0x1000, 24);
    assert_eq!((&reopened_first[0].name[..], reopened_first[0].next), (&b"."[..], 1));
    let original_stat = owned_proc_fstat(&mut f, root);
    let reopened_stat = owned_proc_fstat(&mut f, reopened);
    assert_eq!((original_stat.st_dev, original_stat.st_ino), (reopened_stat.st_dev, reopened_stat.st_ino));

    f.path("");
    assert_eq!(
        f.call(libc::SYS_statx, [root as u64, 0x100, libc::AT_EMPTY_PATH as u64,
            (libc::STATX_TYPE | libc::STATX_INO | libc::STATX_MNT_ID) as u64, 0x4000, 0]),
        0
    );
    let metadata: libc::statx = read_struct(&f.memory, 0x4000);
    assert_ne!(metadata.stx_mask & libc::STATX_MNT_ID, 0);
    assert_eq!(metadata.stx_ino, original_stat.st_ino);
    let info = f.open(&format!("/proc/self/fdinfo/{root}"), libc::O_RDONLY);
    assert!(info >= 0);
    let bytes = f.read(info);
    assert_eq!(FdinfoFixture::field(&bytes, "pos:", 10), 2);
    assert_eq!(FdinfoFixture::field(&bytes, "ino:", 10), original_stat.st_ino);
    assert_eq!(FdinfoFixture::field(&bytes, "mnt_id:", 10), metadata.stx_mnt_id);
    let flags = FdinfoFixture::field(&bytes, "flags:", 8);
    assert_ne!(flags & libc::O_DIRECTORY as u64, 0);
    assert_ne!(flags & libc::O_CLOEXEC as u64, 0);
    assert_ne!(flags & libc::O_NOFOLLOW as u64, 0);

    let path_only = f.open("/proc", libc::O_PATH | libc::O_DIRECTORY);
    assert!(path_only >= 0);
    assert_eq!(f.call(libc::SYS_getdents64, [path_only as u64, u64::MAX, 24, 0, 0, 0]), negative_errno(libc::EBADF));
    assert_eq!(f.call(libc::SYS_lseek, [path_only as u64, 0, libc::SEEK_CUR as u64, 0, 0, 0]), negative_errno(libc::EBADF));
    assert_eq!(f.call(libc::SYS_fchdir, [path_only as u64, 0, 0, 0, 0, 0]), negative_errno(libc::EBADF));
}

#[test]
fn owned_proc_getdents_keeps_completed_record_and_later_copy_fault_effects() {
    let mut f = ProcfsRegressionFixture::new();
    let root = f.open("/proc", libc::O_RDONLY | libc::O_DIRECTORY);
    assert!(root >= 0);
    const PROTECTED: u64 = 0x3000;
    const BUFFER: u64 = PROTECTED - 40;
    f.memory.write(BUFFER, &[0xa5; 40]).unwrap();
    f.memory.map_user_range(0, PROTECTED, false).unwrap();
    f.memory.map_user_range(PROTECTED, PAGE_SIZE as u64, true).unwrap();
    f.memory.enable_user_access();
    // '.' fits; '..' writes its inode but faults on d_reclen. Linux finalizes
    // the prior d_off and returns the complete first record, not the later fault.
    assert_eq!(f.call(libc::SYS_getdents64, [root as u64, BUFFER, 48, 0, 0, 0]), 24);
    let mut complete = [0; 24];
    f.memory.read(BUFFER, &mut complete).unwrap();
    let first = owned_proc_dirents(&complete);
    assert_eq!((&first[0].name[..], first[0].next), (&b"."[..], 1));
    let mut partial_inode = [0; 8];
    f.memory.read(BUFFER + 24, &mut partial_inode).unwrap();
    assert_eq!(u64::from_le_bytes(partial_inode), first[0].inode);
    assert_eq!(f.call(libc::SYS_lseek, [root as u64, 0, libc::SEEK_CUR as u64, 0, 0, 0]), 1);
    let retried = owned_proc_read_directory(&mut f.executor, &f.memory, root, 0x1000, 24);
    assert_eq!(&retried[0].name[..], b"..");

    owned_proc_rewind(&mut f, root);
    assert_eq!(
        f.call(libc::SYS_getdents64, [root as u64, PROTECTED, (7u64 << 32) | 23, 0, 0, 0]),
        negative_errno(libc::EINVAL),
        "record-fit failure precedes any output access and uses the low count word"
    );
    assert_eq!(f.call(libc::SYS_lseek, [root as u64, 0, libc::SEEK_CUR as u64, 0, 0, 0]), 0);
    assert_eq!(f.call(libc::SYS_getdents64, [root as u64, PROTECTED, 24, 0, 0, 0]), negative_errno(libc::EFAULT));
    assert_eq!(f.call(libc::SYS_lseek, [root as u64, 0, libc::SEEK_CUR as u64, 0, 0, 0]), 0);
    assert_eq!(f.call(libc::SYS_lseek, [root as u64, i64::MAX as u64, libc::SEEK_SET as u64, 0, 0, 0]), i64::MAX);
    assert_eq!(f.call(libc::SYS_getdents64, [root as u64, PROTECTED, 24, 0, 0, 0]), 0, "EOF makes no output access");
}

#[test]
fn owned_proc_thread_fs_is_shared_and_fork_fs_is_an_independent_snapshot() {
    let mut f = ProcfsRegressionFixture::new();
    std::fs::create_dir(f.root.0.join("shared")).unwrap();
    std::fs::create_dir(f.root.0.join("other")).unwrap();
    let mut worker = f.executor.thread_child(3).unwrap();
    let old_status = f.open("/proc/1/task/3/status", libc::O_RDONLY);
    assert!(old_status >= 0);
    let old_bytes = f.read(old_status);
    assert_eq!(
        worker.execute(&SyscallRequest::new(libc::SYS_umask as u64, [0o077, 0, 0, 0, 0, 0]), &f.memory),
        0o022
    );
    assert_eq!(f.call(libc::SYS_umask, [0o027, 0, 0, 0, 0, 0]), 0o077, "accepted CLONE_FS shares the actual syscall operand");
    let current_status = f.open("/proc/1/task/3/status", libc::O_RDONLY);
    assert!(current_status >= 0);
    assert!(String::from_utf8(f.read(current_status)).unwrap().contains("Umask:\t0027\n"));
    owned_proc_rewind(&mut f, old_status);
    assert_eq!(f.read(old_status), old_bytes, "the existing opened-status snapshot contract remains literal");

    f.path("shared");
    assert_eq!(worker.execute(&SyscallRequest::new(libc::SYS_chdir as u64, [0x100, 0, 0, 0, 0, 0]), &f.memory), 0);
    let mut child = f.executor.fork_child(2, false, false).unwrap();
    let shared_path = f.root.0.join("shared");
    let shared_path = shared_path.as_os_str().as_bytes();
    let count = f.call(libc::SYS_getcwd, [0x1000, 8192, 0, 0, 0, 0]);
    assert_eq!(count, shared_path.len() as i64 + 1);
    let mut cwd = vec![0; count as usize];
    f.memory.read(0x1000, &mut cwd).unwrap();
    assert_eq!(&cwd[..cwd.len() - 1], shared_path);
    assert_eq!(cwd.last(), Some(&0));

    f.path("../other");
    assert_eq!(f.call(libc::SYS_chdir, [0x100, 0, 0, 0, 0, 0]), 0);
    let other_path = f.root.0.join("other");
    assert_eq!(f.readlinkat(libc::AT_FDCWD as i64, "/proc/1/task/3/cwd").unwrap(), other_path.as_os_str().as_bytes());
    assert_eq!(f.readlinkat(libc::AT_FDCWD as i64, "/proc/2/cwd").unwrap(), shared_path);
    let count = child.execute(&SyscallRequest::new(libc::SYS_getcwd as u64, [0x1000, 8192, 0, 0, 0, 0]), &f.memory);
    assert_eq!(count, shared_path.len() as i64 + 1);
    let mut child_cwd = vec![0; count as usize];
    f.memory.read(0x1000, &mut child_cwd).unwrap();
    assert_eq!(&child_cwd[..child_cwd.len() - 1], shared_path);
    assert_eq!(child.execute(&SyscallRequest::new(libc::SYS_umask as u64, [0o007, 0, 0, 0, 0, 0]), &f.memory), 0o027);
    assert_eq!(worker.execute(&SyscallRequest::new(libc::SYS_umask as u64, [0o027, 0, 0, 0, 0, 0]), &f.memory), 0o027);
    let child_status = f.open("/proc/2/status", libc::O_RDONLY);
    assert!(child_status >= 0);
    assert!(String::from_utf8(f.read(child_status)).unwrap().contains("Umask:\t0007\n"));
}

#[test]
fn owned_proc_membership_tracks_live_threads_then_waitable_zombie_then_consumption() {
    let mut f = ProcfsRegressionFixture::new();
    let root = f.open("/proc", libc::O_RDONLY | libc::O_DIRECTORY);
    assert!(root >= 0);
    let mut child = f.executor.fork_child(2, false, false).unwrap();
    let mut worker = child.thread_child(3).unwrap();
    let tasks = f.open("/proc/2/task", libc::O_RDONLY | libc::O_DIRECTORY);
    assert!(tasks >= 0);
    commit_test_exit(&mut child, 11, false);
    let entries = owned_proc_read_directory(&mut f.executor, &f.memory, tasks, 0x1000, 8192);
    let mut tids: Vec<_> = entries.iter()
        .filter(|entry| entry.name.iter().all(u8::is_ascii_digit))
        .map(|entry| entry.name.clone()).collect();
    tids.sort();
    assert_eq!(tids, [b"2".to_vec(), b"3".to_vec()], "the exited untraced leader remains attached while a worker lives");
    assert_eq!(f.call(libc::SYS_wait4, [2, 0x4000, libc::WNOHANG as u64, 0, 0, 0]), 0, "leader-only exit is not a process terminal event");

    let terminal = commit_test_exit(&mut worker, 23, false);
    assert_eq!(terminal.status, ExitStatus::Exited(23));
    let slot = ChildCompletionSlot::default();
    child.state.children.publish_fixture_owner(terminal.status, &slot).unwrap();
    owned_proc_rewind(&mut f, root);
    let entries = owned_proc_read_directory(&mut f.executor, &f.memory, root, 0x1000, 8192);
    assert!(entries.iter().any(|entry| entry.name == b"2"), "an unconsumed waitable child remains in the owned root");
    let status = f.open("/proc/2/status", libc::O_RDONLY);
    assert!(status >= 0);
    let status_bytes = String::from_utf8(f.read(status)).unwrap();
    assert!(status_bytes.contains("State:\tZ (zombie)\n"), "{status_bytes}");
    let cmdline = f.open("/proc/2/cmdline", libc::O_RDONLY);
    assert!(cmdline >= 0);
    assert!(f.read(cmdline).is_empty());

    assert_eq!(f.call(libc::SYS_waitid, [libc::P_PID as u64, 2, 0x4000,
        (libc::WEXITED | libc::WNOHANG | libc::WNOWAIT) as u64, 0, 0]), 0);
    let info: libc::siginfo_t = read_struct(&f.memory, 0x4000);
    // SAFETY: the unchanged waitid route writes the SIGCHLD variant.
    unsafe {
        assert_eq!(info.si_pid(), 2);
        assert_eq!(info.si_status(), 23);
    }
    owned_proc_rewind(&mut f, root);
    assert!(owned_proc_read_directory(&mut f.executor, &f.memory, root, 0x1000, 8192)
        .iter().any(|entry| entry.name == b"2"));
    assert_eq!(f.call(libc::SYS_wait4, [2, 0x4000, libc::WNOHANG as u64, 0, 0, 0]), 2);
    let wait_status: libc::c_int = read_struct(&f.memory, 0x4000);
    assert_eq!(wait_status, 23 << 8);
    owned_proc_rewind(&mut f, root);
    assert!(!owned_proc_read_directory(&mut f.executor, &f.memory, root, 0x1000, 8192)
        .iter().any(|entry| entry.name == b"2"));
    assert_eq!(f.open("/proc/2/status", libc::O_RDONLY), negative_errno(libc::ENOENT));
    drop(worker);
    drop(child);
    let _replacement = f.executor.fork_child(2, false, false).unwrap();
    owned_proc_rewind(&mut f, status);
    assert_eq!(f.call(libc::SYS_read, [status as u64, 0x1000, 8192, 0, 0, 0]), negative_errno(libc::ESRCH), "an old description never rebinds to a new process generation");
}

#[test]
fn owned_proc_scm_preserves_independent_root_descriptions_and_calling_task_aliases() {
    let mut f = ProcfsRegressionFixture::new();
    let first = f.open("/proc", libc::O_RDONLY | libc::O_DIRECTORY);
    let second = f.open("/proc", libc::O_RDONLY | libc::O_DIRECTORY);
    assert!(first >= 0 && second >= 0);
    let first_stat = owned_proc_fstat(&mut f, first);
    let second_stat = owned_proc_fstat(&mut f, second);
    assert_eq!(first_stat.st_ino, second_stat.st_ino, "independent opens name one logical root");
    let alias = f.call(libc::SYS_dup, [first as u64, 0, 0, 0, 0, 0]);
    assert!(alias >= 0);
    let entries = owned_proc_read_directory(&mut f.executor, &f.memory, first, 0x1000, 24);
    assert_eq!(&entries[0].name[..], b".");
    assert_eq!(f.call(libc::SYS_socketpair, [libc::AF_UNIX as u64, libc::SOCK_DGRAM as u64, 0, 0x800, 0, 0]), 0);
    let sockets: [i32; 2] = read_struct(&f.memory, 0x800);
    let mut child = f.executor.fork_child(2, false, false).unwrap();
    for inherited in [first, second, alias] {
        assert_eq!(child.execute(&SyscallRequest::new(libc::SYS_close as u64, [inherited as u64, 0, 0, 0, 0, 0]), &f.memory), 0);
    }
    // Both rights are in flight simultaneously. A common logical inode is not
    // sufficient authentication for two different native open descriptions.
    for root in [first, second] {
        f.right_message(Some(root as i32));
        assert_eq!(f.call(libc::SYS_sendmsg, [sockets[0] as u64, 0x400, 0, 0, 0, 0]), 1);
        f.close(root);
    }
    let mut received = Vec::new();
    for _ in 0..2 {
        f.right_message(None);
        assert_eq!(child.execute(&SyscallRequest::new(libc::SYS_recvmsg as u64, [sockets[1] as u64, 0x400, 0, 0, 0, 0]), &f.memory), 1);
        let message: libc::msghdr = read_struct(&f.memory, 0x400);
        let mut control = vec![0; message.msg_controllen];
        f.memory.read(0x600, &mut control).unwrap();
        let rights = control_rights(&control);
        assert_eq!(rights.len(), 1);
        received.push(rights[0]);
    }
    let first_entry = owned_proc_read_directory(&mut child, &f.memory, received[0] as i64, 0x1000, 24);
    assert_eq!((&first_entry[0].name[..], first_entry[0].next), (&b".."[..], 2));
    let second_entry = owned_proc_read_directory(&mut child, &f.memory, received[1] as i64, 0x1000, 24);
    assert_eq!((&second_entry[0].name[..], second_entry[0].next), (&b"."[..], 1));
    assert_eq!(f.call(libc::SYS_lseek, [alias as u64, 0, libc::SEEK_CUR as u64, 0, 0, 0]), 2);
    f.path("self/status");
    let status = child.execute(&SyscallRequest::new(libc::SYS_openat as u64,
        [received[0] as u64, 0x100, libc::O_RDONLY as u64, 0, 0, 0]), &f.memory);
    assert!(status >= 0);
    let status = String::from_utf8(procfs_regression_read(&mut child, &f.memory, status)).unwrap();
    assert!(status.contains("Pid:\t2\n"), "magic self is the caller inside the captured run: {status}");
    assert!(status.contains("PPid:\t1\n"), "{status}");
}

#[test]
fn owned_proc_exited_nonleader_directory_cannot_resolve_or_rebind() {
    let mut f = ProcfsRegressionFixture::new();
    let mut worker = f.executor.thread_child(3).unwrap();
    let task = f.open("/proc/1/task/3", libc::O_RDONLY | libc::O_DIRECTORY);
    let fds = f.open("/proc/1/task/3/fd", libc::O_RDONLY | libc::O_DIRECTORY);
    let status = f.open("/proc/1/task/3/status", libc::O_RDONLY);
    assert!(task >= 0 && fds >= 0 && status >= 0);
    commit_test_exit(&mut worker, 7, false);
    for directory in [task, fds] {
        assert_eq!(f.call(libc::SYS_getdents64, [directory as u64, 0x1000, 8192, 0, 0, 0]), negative_errno(libc::ENOENT));
    }
    f.path("status");
    assert_eq!(f.call(libc::SYS_openat, [task as u64, 0x100, libc::O_RDONLY as u64, 0, 0, 0]), negative_errno(libc::ENOENT));
    assert_eq!(f.call(libc::SYS_read, [status as u64, 0x1000, 8192, 0, 0, 0]), negative_errno(libc::ESRCH));
    drop(worker);
    let _new_process = f.executor.fork_child(3, false, false).unwrap();
    let selected = f.open("/proc/3/status", libc::O_RDONLY);
    assert!(selected >= 0, "an unrelated current numeric owner remains resolvable");
    assert!(String::from_utf8(f.read(selected)).unwrap().contains("Tgid:\t3\n"));
    assert_eq!(f.call(libc::SYS_getdents64, [task as u64, 0x1000, 8192, 0, 0, 0]), negative_errno(libc::ENOENT));
    assert_eq!(f.call(libc::SYS_read, [status as u64, 0x1000, 8192, 0, 0, 0]), negative_errno(libc::ESRCH));
}
