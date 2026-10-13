/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

#[derive(Clone, Debug, PartialEq, Eq)]
struct ProcListed {
    inode: u64,
    next: i64,
    kind: u8,
    name: String,
}

/// The `linux_dirent64` records of a synthetic procfs directory.
fn proc_listed(bytes: &[u8]) -> Vec<ProcListed> {
    let mut entries = Vec::new();
    let mut at = 0;
    while at < bytes.len() {
        let length = usize::from(u16::from_ne_bytes(
            bytes[at + 16..at + 18].try_into().unwrap(),
        ));
        assert!(length >= 24 && length.is_multiple_of(8) && at + length <= bytes.len());
        let name = &bytes[at + 19..at + length];
        let name = &name[..name.iter().position(|byte| *byte == 0).unwrap()];
        assert_eq!((19 + name.len() + 1 + 7) & !7, length);
        let inode = u64::from_ne_bytes(bytes[at..at + 8].try_into().unwrap());
        assert_ne!(inode, 0);
        entries.push(ProcListed {
            inode,
            next: i64::from_ne_bytes(bytes[at + 8..at + 16].try_into().unwrap()),
            kind: bytes[at + 18],
            name: String::from_utf8(name.to_vec()).unwrap(),
        });
        at += length;
    }
    entries
}

const PROC_LISTING: u64 = 0x2000;
const PROC_LISTING_CAPACITY: u64 = 0x2000;

impl ProcfsRegressionFixture {
    fn with_memory() -> Self {
        let mut f = Self::new();
        f.memory = GuestMemory::new(0, 0x4000 + PROC_LISTING_CAPACITY as usize).unwrap();
        f
    }

    fn getdents(&mut self, fd: i64, capacity: u64) -> Result<Vec<ProcListed>, i64> {
        getdents_as(&mut self.executor, &mut self.memory, fd, capacity)
    }

    fn lseek(&mut self, fd: i64, offset: i64, whence: i32) -> i64 {
        self.call(
            libc::SYS_lseek,
            [fd as u64, offset as u64, whence as u64, 0, 0, 0],
        )
    }
}

fn getdents_as(
    executor: &mut ElfExecutor,
    memory: &mut GuestMemory,
    fd: i64,
    capacity: u64,
) -> Result<Vec<ProcListed>, i64> {
    let sentinel = vec![0xa7; PROC_LISTING_CAPACITY as usize];
    memory.write(PROC_LISTING, &sentinel).unwrap();
    let count = executor.execute(
        &SyscallRequest::new(
            libc::SYS_getdents64 as u64,
            [fd as u64, PROC_LISTING, capacity, 0, 0, 0],
        ),
        &*memory,
    );
    let mut bytes = sentinel.clone();
    memory.read(PROC_LISTING, &mut bytes).unwrap();
    let copied = usize::try_from(count).unwrap_or(0);
    assert_eq!(&bytes[copied..], &sentinel[copied..], "bytes past the result");
    if count < 0 {
        return Err(count);
    }
    Ok(proc_listed(&bytes[..copied]))
}

/// Every entry from the current position to the end, `capacity` bytes at a time.
fn drain_as(
    executor: &mut ElfExecutor,
    memory: &mut GuestMemory,
    fd: i64,
    capacity: u64,
) -> Vec<ProcListed> {
    let mut entries = Vec::new();
    loop {
        let batch = getdents_as(executor, memory, fd, capacity).unwrap();
        if batch.is_empty() {
            return entries;
        }
        entries.extend(batch);
    }
}

fn names(entries: &[ProcListed]) -> Vec<&str> {
    entries.iter().map(|entry| entry.name.as_str()).collect()
}

fn open_as(executor: &mut ElfExecutor, memory: &mut GuestMemory, path: &str, flags: i32) -> i64 {
    write_c_string(memory, 0x100, path);
    executor.execute(
        &SyscallRequest::new(
            libc::SYS_openat as u64,
            [libc::AT_FDCWD as u64, 0x100, flags as u64, 0, 0, 0],
        ),
        memory,
    )
}

fn read_as(executor: &mut ElfExecutor, memory: &GuestMemory, fd: i64) -> Vec<u8> {
    procfs_regression_read(executor, memory, fd)
}

#[test]
fn procfs_enumeration_root_lists_in_position_order() {
    let mut f = ProcfsRegressionFixture::with_memory();
    let root = f.open("/proc", libc::O_RDONLY | libc::O_DIRECTORY);
    assert!(root >= 0);
    let entries = drain_as(&mut f.executor, &mut f.memory, root, PROC_LISTING_CAPACITY);
    // The root's own names: each synthetic file directly beneath it and the
    // first component of each deeper one outside self/, in name order.
    let expected_static: std::collections::BTreeSet<&str> = SYNTHETIC_REGULAR_PROC_PATHS
        .iter()
        .map(|path| std::str::from_utf8(path.strip_prefix(b"/proc/").unwrap()).unwrap())
        .filter(|path| !path.starts_with("self/"))
        .map(|path| path.split('/').next().unwrap())
        .collect();
    let mut expected = vec![".", ".."];
    expected.extend(expected_static);
    expected.extend(["self", "thread-self", "1"]);
    assert_eq!(names(&entries), expected);
    for name in ["cpuinfo", "uptime", "sys", "self", "1"] {
        assert!(names(&entries).contains(&name), "{name}");
    }
    let kind = |name: &str| entries.iter().find(|entry| entry.name == name).unwrap().kind;
    assert_eq!(kind("."), libc::DT_DIR);
    assert_eq!(kind("sys"), libc::DT_DIR);
    assert_eq!(kind("cpuinfo"), libc::DT_REG);
    assert_eq!(kind("mounts"), libc::DT_LNK);
    assert_eq!(kind("self"), libc::DT_LNK);
    assert_eq!(kind("1"), libc::DT_DIR);
    // Each d_off is the position of the next entry, increasing to the end.
    assert!(entries.windows(2).all(|pair| pair[0].next < pair[1].next));
    assert_eq!(entries[0].next, 1);
    // The listed inode is the one stat reports.
    f.path("/proc/1");
    assert_eq!(
        f.call(
            libc::SYS_newfstatat,
            [libc::AT_FDCWD as u64, 0x100, 0x4000, 0, 0, 0]
        ),
        0
    );
    let stat: libc::stat = read_struct(&f.memory, 0x4000);
    assert_eq!(stat.st_ino, entries.last().unwrap().inode);
    assert_eq!(stat.st_mode & libc::S_IFMT, libc::S_IFDIR);
    // At the end, getdents64 returns 0 and keeps returning it.
    assert_eq!(f.getdents(root, PROC_LISTING_CAPACITY), Ok(Vec::new()));
    assert_eq!(f.getdents(root, 1), Ok(Vec::new()));
}

#[test]
fn procfs_enumeration_paginates_rewinds_and_resumes() {
    let mut f = ProcfsRegressionFixture::with_memory();
    let root = f.open("/proc", libc::O_RDONLY | libc::O_DIRECTORY);
    let whole = drain_as(&mut f.executor, &mut f.memory, root, PROC_LISTING_CAPACITY);
    assert_eq!(f.lseek(root, 0, libc::SEEK_SET), 0);
    // A buffer too small for the next entry is EINVAL and moves nothing.
    assert_eq!(f.getdents(root, 23), Err(negative_errno(libc::EINVAL)));
    assert_eq!(f.lseek(root, 0, libc::SEEK_CUR), 0);
    // One entry per call (32 bytes holds one record of a short name, never
    // two) returns the same sequence, ending exactly where the large read did.
    let mut single = Vec::new();
    loop {
        match f.getdents(root, 32) {
            Ok(batch) if batch.is_empty() => break,
            Ok(batch) => {
                assert_eq!(batch.len(), 1);
                assert_eq!(
                    f.lseek(root, 0, libc::SEEK_CUR),
                    batch[0].next,
                    "the position follows the returned d_off"
                );
                single.extend(batch);
            }
            // A name longer than 12 bytes needs a 40-byte record.
            Err(error) => {
                assert_eq!(error, negative_errno(libc::EINVAL));
                single.extend(f.getdents(root, 40).unwrap());
            }
        }
    }
    assert_eq!(single, whole);
    // lseek to a returned d_off resumes after that entry, through an alias.
    let alias = f.call(libc::SYS_dup, [root as u64, 0, 0, 0, 0, 0]);
    assert_eq!(f.lseek(alias, whole[3].next, libc::SEEK_SET), whole[3].next);
    assert_eq!(
        drain_as(&mut f.executor, &mut f.memory, root, PROC_LISTING_CAPACITY),
        whole[4..]
    );
    // A subdirectory descriptor keeps its own position the same way.
    let fd_directory = f.open("/proc/self/fd", libc::O_RDONLY | libc::O_DIRECTORY);
    assert!(fd_directory >= 0);
    let first = f.getdents(fd_directory, 24).unwrap();
    assert_eq!(names(&first), ["."]);
    let rest = drain_as(&mut f.executor, &mut f.memory, fd_directory, PROC_LISTING_CAPACITY);
    assert_eq!(rest[0].name, "..");
    assert_eq!(f.lseek(fd_directory, 0, libc::SEEK_SET), 0);
    let again = drain_as(&mut f.executor, &mut f.memory, fd_directory, PROC_LISTING_CAPACITY);
    assert_eq!(again[0], first[0]);
    assert_eq!(again[1..], rest[..]);
    // An O_PATH descriptor of a synthetic directory is not readable.
    let path_only = f.open("/proc/self/fd", libc::O_PATH | libc::O_DIRECTORY);
    assert_eq!(
        f.getdents(path_only, PROC_LISTING_CAPACITY),
        Err(negative_errno(libc::EBADF))
    );
    // A file is not a directory.
    let stat = f.open("/proc/self/stat", libc::O_RDONLY);
    assert_eq!(
        f.getdents(stat, PROC_LISTING_CAPACITY),
        Err(negative_errno(libc::ENOTDIR))
    );
}

#[test]
fn procfs_enumeration_lists_descriptors_tasks_and_process_files() {
    let mut f = ProcfsRegressionFixture::with_memory();
    std::fs::write(f.root.0.join("held"), b"held").unwrap();
    let held = f.open("held", libc::O_RDONLY);
    assert_eq!(held, 3);
    let fds = f.open("/proc/self/fd", libc::O_RDONLY | libc::O_DIRECTORY);
    assert_eq!(fds, 4);
    let entries = drain_as(&mut f.executor, &mut f.memory, fds, PROC_LISTING_CAPACITY);
    assert_eq!(names(&entries), [".", "..", "0", "1", "2", "3", "4"]);
    assert!(entries[2..].iter().all(|entry| entry.kind == libc::DT_LNK));

    let worker = f.executor.thread_child(3).unwrap();
    let tasks = f.open("/proc/self/task", libc::O_RDONLY | libc::O_DIRECTORY);
    let entries = drain_as(&mut f.executor, &mut f.memory, tasks, PROC_LISTING_CAPACITY);
    assert_eq!(names(&entries), [".", "..", "1", "3"]);
    // A thread is not a process: the root lists only its group leader.
    let root = f.open("/proc", libc::O_RDONLY | libc::O_DIRECTORY);
    let listed = drain_as(&mut f.executor, &mut f.memory, root, PROC_LISTING_CAPACITY);
    assert_eq!(names(&listed).last(), Some(&"1"));
    assert!(!names(&listed).contains(&"3"));
    drop(worker);
    assert_eq!(f.lseek(tasks, 0, libc::SEEK_SET), 0);
    let entries = drain_as(&mut f.executor, &mut f.memory, tasks, PROC_LISTING_CAPACITY);
    assert_eq!(names(&entries), [".", "..", "1"]);

    let process = f.open("/proc/self", libc::O_RDONLY | libc::O_DIRECTORY);
    let entries = drain_as(&mut f.executor, &mut f.memory, process, PROC_LISTING_CAPACITY);
    let listed = names(&entries);
    for name in ["task", "fd", "fdinfo", "status", "cmdline", "stat", "maps"] {
        assert!(listed.contains(&name), "{name} in {listed:?}");
    }
    // Only names that resolve are listed, in Linux's order.
    assert!(!listed.contains(&"environ"));
    let position = |name| listed.iter().position(|listed| *listed == name).unwrap();
    assert!(position("task") < position("fd"));
    assert!(position("status") < position("cmdline"));
    assert!(position("cmdline") < position("stat"));
}

#[test]
fn procfs_enumeration_shows_a_forked_child_until_it_exits() {
    let mut f = ProcfsRegressionFixture::with_memory();
    let mut child = f.executor.fork_child(2, false, false).unwrap();
    f.path("child-name");
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
    assert_eq!(
        child.execute(
            &SyscallRequest::new(libc::SYS_umask as u64, [0o027, 0, 0, 0, 0, 0]),
            &f.memory,
        ),
        0o022
    );
    std::fs::write(f.root.0.join("child-held"), b"child").unwrap();
    let held = open_as(&mut child, &mut f.memory, "child-held", libc::O_RDONLY);
    assert_eq!(held, 3);

    let root = f.open("/proc", libc::O_RDONLY | libc::O_DIRECTORY);
    let listed = drain_as(&mut f.executor, &mut f.memory, root, PROC_LISTING_CAPACITY);
    assert_eq!(names(&listed)[names(&listed).len() - 2..], ["1", "2"]);

    // The parent reads the child's files from the child's own cells.
    let stat = f.open("/proc/2/stat", libc::O_RDONLY);
    assert!(stat >= 0, "open peer stat: {stat}");
    let stat = String::from_utf8(f.read(stat)).unwrap();
    assert!(stat.starts_with("2 (child-name) R 1 "), "{stat}");
    let status = f.open("/proc/2/status", libc::O_RDONLY);
    assert!(status >= 0, "open peer status: {status}");
    let status = String::from_utf8(f.read(status)).unwrap();
    for line in ["Name:\tchild-name\n", "Umask:\t0027\n", "Pid:\t2\n", "PPid:\t1\n"] {
        assert!(status.contains(line), "{line:?} in {status}");
    }
    let cmdline = f.open("/proc/2/cmdline", libc::O_RDONLY);
    assert!(cmdline >= 0, "open peer cmdline: {cmdline}");
    assert_eq!(f.read(cmdline), b"test\0");
    // Its own view of itself agrees.
    let own = open_as(&mut child, &mut f.memory, "/proc/self/stat", libc::O_RDONLY);
    assert_eq!(read_as(&mut child, &f.memory, own), stat.as_bytes());

    // Its descriptor table is listed, and a held file's link and stat are
    // those of the file.
    let fds = f.open("/proc/2/fd", libc::O_RDONLY | libc::O_DIRECTORY);
    assert!(fds >= 0);
    let entries = drain_as(&mut f.executor, &mut f.memory, fds, PROC_LISTING_CAPACITY);
    assert_eq!(names(&entries), [".", "..", "0", "1", "2", "3", "4"]);
    let target = f.root.0.join("child-held");
    assert_eq!(
        f.readlinkat(libc::AT_FDCWD as i64, "/proc/2/fd/3").unwrap(),
        target.as_os_str().as_bytes()
    );
    f.path("/proc/2/fd/3");
    assert_eq!(
        f.call(
            libc::SYS_newfstatat,
            [libc::AT_FDCWD as u64, 0x100, 0x4000, 0, 0, 0]
        ),
        0
    );
    let stat: libc::stat = read_struct(&f.memory, 0x4000);
    let expected = std::fs::metadata(&target).unwrap();
    assert_eq!((stat.st_dev, stat.st_ino), (expected.dev(), expected.ino()));

    drop(child);
    assert_eq!(f.lseek(root, 0, libc::SEEK_SET), 0);
    let listed = drain_as(&mut f.executor, &mut f.memory, root, PROC_LISTING_CAPACITY);
    assert_eq!(names(&listed).last(), Some(&"1"));
    assert!(f.open("/proc/2/stat", libc::O_RDONLY) < 0);
    // A descriptor of the exited child's directory reads as Linux's does.
    assert_eq!(
        f.getdents(fds, PROC_LISTING_CAPACITY),
        Err(negative_errno(libc::ENOENT))
    );
}

#[test]
fn procfs_enumeration_shows_the_namespace_init_of_a_non_init_root() {
    let root = ProcfsRegressionDir::new();
    let mut state = test_state(&root.0);
    // Renumber the only task as the conventional root guest, PID 3, whose
    // parent is the namespace init outside the traced tree.
    state.task_lifecycle = Arc::new(Mutex::new(crate::elf::TaskLifecycleTable::with_root(
        3, 3, 3, true,
    )));
    (state.pid, state.tid, state.pgid, state.ppid) = (3, 3, 3, NAMESPACE_INIT_PID);
    let mut executor = ElfExecutor::new(state, false);
    let mut memory = GuestMemory::new(0, 0x4000 + PROC_LISTING_CAPACITY as usize).unwrap();
    let proc_root = open_as(
        &mut executor,
        &mut memory,
        "/proc",
        libc::O_RDONLY | libc::O_DIRECTORY,
    );
    let listed = drain_as(&mut executor, &mut memory, proc_root, PROC_LISTING_CAPACITY);
    assert_eq!(names(&listed)[names(&listed).len() - 2..], ["1", "3"]);
    let stat = open_as(&mut executor, &mut memory, "/proc/1/stat", libc::O_RDONLY);
    assert!(stat >= 0, "open init stat: {stat}");
    assert!(read_as(&mut executor, &memory, stat).starts_with(b"1 (hermit) R 0 "));
    let directory = open_as(
        &mut executor,
        &mut memory,
        "/proc/1",
        libc::O_RDONLY | libc::O_DIRECTORY,
    );
    let entries = drain_as(&mut executor, &mut memory, directory, PROC_LISTING_CAPACITY);
    assert_eq!(names(&entries), [".", "..", "status", "cmdline", "stat"]);
    // Nothing beneath it reaches a task or the host.
    for path in ["/proc/1/fd", "/proc/1/fd/0", "/proc/1/exe", "/proc/1/maps"] {
        assert_eq!(
            open_as(&mut executor, &mut memory, path, libc::O_RDONLY),
            negative_errno(libc::ENOENT),
            "{path}"
        );
    }
}

#[test]
fn procfs_enumeration_synthetic_files_report_procfs_statfs() {
    let mut f = ProcfsRegressionFixture::new();
    for path in ["/proc/mounts", "/proc/self/stat", "/proc/self/fd"] {
        let fd = f.open(path, libc::O_RDONLY);
        assert!(fd >= 0, "{path}");
        assert_eq!(
            f.call(libc::SYS_fstatfs, [fd as u64, 0x1000, 0, 0, 0, 0]),
            0
        );
        let statfs: libc::statfs = read_struct(&f.memory, 0x1000);
        assert_eq!(statfs.f_type as libc::c_long, PROC_SUPER_MAGIC, "{path}");
        assert_eq!(statfs.f_blocks, 0, "{path}");
    }
}

/// [`checked_getdents64`] for a synthetic procfs directory, whose entries
/// include symlinks: the result, its records, and a check that nothing
/// outside the returned records was written.
fn proc_getdents64(
    memory: &mut GuestMemory,
    state: &mut LoadedStaticElf,
    fd: u64,
    address: u64,
    length: u64,
) -> (i64, Vec<ProcListed>) {
    let sentinel = vec![0xa7; (memory.guest_end() - memory.guest_base()) as usize];
    memory.write_raw(memory.guest_base(), &sentinel).unwrap();
    let result = syscall_result(
        memory,
        state,
        libc::SYS_getdents64,
        [fd, address, length, u64::MAX, 0x5a5a_a5a5_dead_beef, 1 << 63],
    );
    let mut actual = vec![0; sentinel.len()];
    memory.read_raw(memory.guest_base(), &mut actual).unwrap();
    if result <= 0 {
        assert_eq!(actual, sentinel, "fd={fd:#x}, address={address:#x}, count={length}");
        return (result, Vec::new());
    }
    let start = usize::try_from(address - memory.guest_base()).unwrap();
    let end = start + result as usize;
    assert_eq!(&actual[..start], &sentinel[..start], "prefix sentinel");
    assert_eq!(&actual[end..], &sentinel[end..], "suffix sentinel");
    (result, proc_listed(&actual[start..end]))
}
