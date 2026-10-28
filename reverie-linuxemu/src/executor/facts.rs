/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! Facts the executor reports to a tool through `reverie::Guest`, from the
//! state it keeps for the guest (gVisor: pkg/sentry/fsimpl/proc/task_fds.go,
//! whose `fdinfo` prints `pos`, `flags` and `mnt_id` from the Sentry's own
//! file description, and not yet its locks or type-specific lines).
//!
//! `descriptor_status`: a guest descriptor that the executor backs with a
//! host descriptor of the same open file description reports what the host
//! kernel's `fdinfo` shows for that description, read by the supervisor for
//! its own descriptor while the descriptor table is locked. Nothing here
//! duplicates or closes a descriptor of the guest's file: closing any
//! descriptor of a file releases every POSIX lock the closing process holds
//! on it (`locks_remove_posix` in Linux's `fs/locks.c`), and tools run in the
//! supervisor process. Two answers are this backend's, not Linux's:
//!
//! - A guest's `F_SETLK` or `F_SETLKW` record lock is held as an open file
//!   description lock (`F_OFD_SETLK`, see `fcntl` in `file.rs`), so it is
//!   reported with class `OpenFileDescription`, where Linux would report
//!   `Posix`. This is a named gap of the provider, not a parse error. The
//!   lock also behaves as the open file description lock it is. Every
//!   process that holds a descriptor of its description, such as a forked
//!   child or a receiver of the descriptor through `SCM_RIGHTS`, shares it:
//!   the lock is reported for that descriptor, and an `F_SETLK` through it
//!   changes or releases the lock, where Linux lists a `POSIX` lock only
//!   through its owner's descriptor table and lets only its owner change it.
//!   And it outlives a close that would release a `POSIX` lock, any close of
//!   the file by the owning process, until the description's last descriptor
//!   is closed.
//! - A guest's `flock(2)` is refused (`UnsupportedOperation::Flock`), so a
//!   flock lock is reported only for a description the guest received
//!   already locked, such as an inherited standard input.
//!
//! An object the executor keeps itself, whose host carrier is not the guest's
//! object, is not provided: a virtual signalfd (an eventfd carrier), a memfd
//! of the executor's own, such as a synthetic `/proc` file or `fdinfo`
//! description, and with captured output the supervisor's standard output or
//! error. The last two are recognized by the host object itself, not by what
//! the table records for the guest descriptor, so a descriptor received
//! through `SCM_RIGHTS`, whose records the receive drops, is declined too.

use super::*;

/// The most `fdinfo` text read for one host descriptor. An epoll
/// descriptor lists one line for every descriptor it watches, so its text
/// has no fixed bound; a longer one is not provided.
const FDINFO_LIMIT: u64 = 1024 * 1024;

impl ElfExecutor {
    /// `reverie::Guest::descriptor_status` for this task's descriptor `fd`:
    /// see the module documentation. `EBADF` when this task's table does not
    /// hold `fd`; any other errno is the host kernel's answer to the
    /// supervisor's read of its own `fdinfo`.
    pub fn descriptor_status(
        &self,
        fd: i32,
    ) -> std::result::Result<reverie::DescriptorStatus, reverie::FactError> {
        // The table the task's syscalls install before they run, locked until
        // the read is done. Every path that takes a descriptor out of it, to
        // close or retire it, holds this lock: publishing a task's table
        // (`update_from_elf`) and the last task's exit. The executor never
        // closes the supervisor's standard output or error. So `host` stays
        // open, and names the same open file description, throughout. While
        // it holds the lock, this only reads procfs and stats descriptors: it
        // waits for no other lock and no guest task.
        let table = self
            .file_table
            .lock()
            .expect("KVM file-table lock poisoned");
        let host = descriptor_source(&table, fd)?;
        if executor_object(host, self.output.is_some()) {
            return Err(reverie::FactError::NotProvided);
        }
        let text = read_own_fdinfo(host);
        drop(table);
        match text {
            Ok(Some(text)) => reverie::DescriptorStatus::from_fdinfo(&text),
            Ok(None) => Err(reverie::FactError::NotProvided),
            Err(error) => Err(reverie::FactError::Errno(reverie::Errno::new(
                error.raw_os_error().unwrap_or(libc::EIO),
            ))),
        }
    }
}

/// The host descriptor behind guest descriptor `fd` in `table`, as the
/// task's own syscalls resolve it (`host_fd` in `file.rs`). A virtual
/// signalfd is not provided: its host carrier is the executor's eventfd, and
/// it never leaves this table, because fork is refused while one is open
/// and `SCM_RIGHTS` refuses to send one.
fn descriptor_source(
    table: &FileTableState,
    fd: libc::c_int,
) -> std::result::Result<RawFd, reverie::FactError> {
    let open_standard = (0..=2).contains(&fd)
        && !table.files.contains_key(&fd)
        && !table.closed_standard_fds.contains(&fd)
        && (fd != libc::STDIN_FILENO || table.stdin.is_some());
    let host = match table.files.get(&fd) {
        Some(file) => Some(file.as_raw_fd()),
        None if !open_standard => None,
        None if fd == libc::STDIN_FILENO => table.stdin.as_ref().map(AsRawFd::as_raw_fd),
        // Standard output or error is the supervisor's own.
        None => Some(fd),
    };
    let host = host.ok_or(reverie::FactError::Errno(reverie::Errno::EBADF))?;
    if table.signalfd_fds.contains(&fd) {
        return Err(reverie::FactError::NotProvided);
    }
    Ok(host)
}

/// Whether host descriptor `host` is an object the executor keeps itself,
/// judged by the object, so that the answer is the same for every
/// descriptor of it: one received through `SCM_RIGHTS`, in this process or
/// another, whose records the receive drops (`install_received_rights` in
/// `socket.rs`), a duplicate, or a reopen. These are the deny-only tests
/// that `syncfs` and shared `mmap` make (`sync_filesystem` in `syncfs.rs`
/// and `classify_shared_mmap_file` in `shared_file.rs`):
///
/// - a memfd with a name reserved for the executor's own
///   (`syncfs_private_memfd_name`), such as a synthetic `/proc` file;
/// - with captured output, the supervisor's standard output or error,
///   compared by device and inode.
///
/// An object that cannot be identified is the executor's too. A name only
/// denies: a guest's own memfd with a reserved name is not provided either,
/// nor, with captured output, a guest's own descriptor of the file the
/// supervisor's standard output or error is.
fn executor_object(host: RawFd, capture_output: bool) -> bool {
    match canonical_fd_path(host) {
        Ok(path) if !syncfs_private_memfd_name(path.as_os_str().as_bytes()) => {}
        _ => return true,
    }
    if !capture_output {
        return false;
    }
    let Ok(object) = host_file_key(host) else {
        return true;
    };
    [libc::STDOUT_FILENO, libc::STDERR_FILENO]
        .into_iter()
        .any(|standard| !matches!(host_file_key(standard), Ok(key) if key != object))
}

/// The supervisor's own `fdinfo` for its descriptor `host`, or `None` when it
/// is longer than [`FDINFO_LIMIT`]. Only the procfs file is opened and
/// closed.
fn read_own_fdinfo(host: RawFd) -> std::io::Result<Option<Vec<u8>>> {
    let source = std::fs::File::open(format!("/proc/self/fdinfo/{host}"))?;
    let mut text = Vec::new();
    std::io::Read::read_to_end(
        &mut std::io::Read::take(source, FDINFO_LIMIT + 1),
        &mut text,
    )?;
    Ok((text.len() as u64 <= FDINFO_LIMIT).then_some(text))
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicU64;
    use std::sync::atomic::Ordering;

    use reverie::FactError;
    use reverie::LockClass;

    use super::*;

    const PAGE: u64 = 4096;
    /// Guest addresses of the tests' operands.
    const VALUE: u64 = 0x100;
    const LOCK: u64 = 0x200;
    const PATH: u64 = 0x400;
    const PAIR: u64 = 0x600;
    const MESSAGE: u64 = 0x700;
    const VECTOR: u64 = 0x780;
    const BYTE: u64 = 0x7c0;
    const CONTROL: u64 = 0x800;

    struct Directory(std::path::PathBuf);

    impl Directory {
        fn new() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let path = std::env::temp_dir().join(format!(
                "reverie-linuxemu-facts-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for Directory {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.0).unwrap();
        }
    }

    fn call(
        executor: &mut ElfExecutor,
        memory: &GuestMemory,
        number: libc::c_long,
        args: [u64; 6],
    ) -> i64 {
        executor.execute(&SyscallRequest::new(number as u64, args), memory)
    }

    fn open(executor: &mut ElfExecutor, memory: &mut GuestMemory, path: &[u8], flags: i32) -> i32 {
        let mut path = path.to_vec();
        path.push(0);
        memory.write(PATH, &path).unwrap();
        let fd = call(
            executor,
            memory,
            libc::SYS_openat,
            [libc::AT_FDCWD as u64, PATH, flags as u64, 0o600, 0, 0],
        );
        assert!(fd >= 0, "open {}: {fd}", String::from_utf8_lossy(&path));
        fd as i32
    }

    /// The bytes of `value`, a plain C structure.
    fn bytes_of<T>(value: &T) -> &[u8] {
        // SAFETY: the view covers exactly the initialized value.
        unsafe {
            std::slice::from_raw_parts(std::ptr::from_ref(value).cast::<u8>(), size_of::<T>())
        }
    }

    /// A guest syscall that returns a new descriptor.
    fn new_fd(
        executor: &mut ElfExecutor,
        memory: &GuestMemory,
        number: libc::c_long,
        args: [u64; 6],
    ) -> i32 {
        let fd = call(executor, memory, number, args);
        assert!(fd >= 0, "syscall {number}: {fd}");
        fd as i32
    }

    fn close(executor: &mut ElfExecutor, memory: &GuestMemory, fd: i32) {
        assert_eq!(
            call(
                executor,
                memory,
                libc::SYS_close,
                [fd as u64, 0, 0, 0, 0, 0]
            ),
            0,
            "close {fd}"
        );
    }

    fn memfd(executor: &mut ElfExecutor, memory: &mut GuestMemory, name: &[u8]) -> i32 {
        let mut name = name.to_vec();
        name.push(0);
        memory.write(PATH, &name).unwrap();
        new_fd(
            executor,
            memory,
            libc::SYS_memfd_create,
            [PATH, 0, 0, 0, 0, 0],
        )
    }

    /// A guest datagram socket pair.
    fn socket_pair(executor: &mut ElfExecutor, memory: &GuestMemory) -> [i32; 2] {
        assert_eq!(
            call(
                executor,
                memory,
                libc::SYS_socketpair,
                [libc::AF_UNIX as u64, libc::SOCK_DGRAM as u64, 0, PAIR, 0, 0],
            ),
            0
        );
        let mut pair = [0u8; 8];
        memory.read(PAIR, &mut pair).unwrap();
        [
            i32::from_ne_bytes(pair[..4].try_into().unwrap()),
            i32::from_ne_bytes(pair[4..].try_into().unwrap()),
        ]
    }

    /// Writes a one-byte message whose control buffer, `control` bytes long,
    /// is at `CONTROL`.
    fn write_message(memory: &mut GuestMemory, control: usize) {
        memory.write(BYTE, b"x").unwrap();
        let vector = libc::iovec {
            iov_base: BYTE as usize as *mut libc::c_void,
            iov_len: 1,
        };
        memory.write(VECTOR, bytes_of(&vector)).unwrap();
        // SAFETY: libc::msghdr is plain data; an all-zero value is valid.
        let mut message: libc::msghdr = unsafe { std::mem::zeroed() };
        message.msg_iov = VECTOR as usize as *mut libc::iovec;
        message.msg_iovlen = 1;
        message.msg_control = CONTROL as usize as *mut libc::c_void;
        message.msg_controllen = control;
        memory.write(MESSAGE, bytes_of(&message)).unwrap();
    }

    /// Sends guest descriptor `fd` through guest socket `socket` in one
    /// `SCM_RIGHTS` message.
    fn send_descriptor(executor: &mut ElfExecutor, memory: &mut GuestMemory, socket: i32, fd: i32) {
        // SAFETY: CMSG_SPACE and CMSG_LEN only compute sizes.
        let (space, length) = unsafe { (libc::CMSG_SPACE(4), libc::CMSG_LEN(4)) };
        let header = libc::cmsghdr {
            cmsg_len: length as usize,
            cmsg_level: libc::SOL_SOCKET,
            cmsg_type: libc::SCM_RIGHTS,
        };
        let mut control = vec![0u8; space as usize];
        control[..size_of::<libc::cmsghdr>()].copy_from_slice(bytes_of(&header));
        control[size_of::<libc::cmsghdr>()..][..4].copy_from_slice(&fd.to_ne_bytes());
        memory.write(CONTROL, &control).unwrap();
        write_message(memory, control.len());
        assert_eq!(
            call(
                executor,
                memory,
                libc::SYS_sendmsg,
                [socket as u64, MESSAGE, 0, 0, 0, 0]
            ),
            1,
            "send {fd}"
        );
    }

    /// Receives the one descriptor of the next `SCM_RIGHTS` message queued
    /// for guest socket `socket`.
    fn receive_descriptor(
        executor: &mut ElfExecutor,
        memory: &mut GuestMemory,
        socket: i32,
    ) -> i32 {
        memory.write(CONTROL, &[0; 64]).unwrap();
        write_message(memory, 64);
        assert_eq!(
            call(
                executor,
                memory,
                libc::SYS_recvmsg,
                [socket as u64, MESSAGE, 0, 0, 0, 0]
            ),
            1
        );
        let mut control = [0u8; 64];
        memory.read(CONTROL, &mut control).unwrap();
        // SAFETY: the buffer holds a whole cmsghdr; read_unaligned copies it.
        let header: libc::cmsghdr =
            unsafe { std::ptr::read_unaligned(control.as_ptr().cast::<libc::cmsghdr>()) };
        assert_eq!(
            (header.cmsg_level, header.cmsg_type),
            (libc::SOL_SOCKET, libc::SCM_RIGHTS)
        );
        // SAFETY: CMSG_LEN only computes a size.
        assert_eq!(header.cmsg_len, unsafe { libc::CMSG_LEN(4) } as usize);
        let data = &control[size_of::<libc::cmsghdr>()..][..4];
        i32::from_ne_bytes(data.try_into().unwrap())
    }

    /// A record lock of `kind` on bytes `start` to `start + length - 1`.
    fn record_lock(kind: libc::c_int, start: i64, length: i64) -> libc::flock {
        // SAFETY: libc::flock is plain data; an all-zero value is valid.
        let mut lock: libc::flock = unsafe { std::mem::zeroed() };
        lock.l_type = kind as i16;
        lock.l_whence = libc::SEEK_SET as i16;
        lock.l_start = start;
        lock.l_len = length;
        lock
    }

    /// Whether the supervisor process, this one, still holds its write lock
    /// on bytes 0 to 9 of `path`, taken through `supervisor`: as `F_GETLK`
    /// from a forked child sees it, and as this process's own `fdinfo` of
    /// `supervisor` lists it. Neither check opens or closes a descriptor of
    /// the file in this process.
    fn supervisor_lock_held(path: &std::path::Path, supervisor: &std::fs::File) -> bool {
        use std::os::unix::ffi::OsStrExt;

        let path = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
        let mut probe = record_lock(libc::F_RDLCK, 0, 10);
        // SAFETY: getpid has no preconditions.
        let parent = unsafe { libc::getpid() };
        // SAFETY: the child calls only open, fcntl and _exit, which are
        // async-signal-safe, before it exits.
        let child = unsafe { libc::fork() };
        assert!(child >= 0, "fork: {}", std::io::Error::last_os_error());
        if child == 0 {
            // SAFETY: path is NUL-terminated and probe is a valid flock.
            let held = unsafe {
                let fd = libc::open(path.as_ptr(), libc::O_RDONLY | libc::O_CLOEXEC);
                fd >= 0
                    && libc::fcntl(fd, libc::F_GETLK, &mut probe) == 0
                    && probe.l_type == libc::F_WRLCK as i16
                    && probe.l_pid == parent
                    && (probe.l_start, probe.l_len) == (0, 10)
            };
            // SAFETY: _exit ends the child without running the parent's
            // exit handlers.
            unsafe { libc::_exit(if held { 0 } else { 1 }) };
        }
        let mut status = 0;
        // SAFETY: status is writable and child is this process's child.
        assert_eq!(unsafe { libc::waitpid(child, &mut status, 0) }, child);
        let forked = libc::WIFEXITED(status) && libc::WEXITSTATUS(status) == 0;
        let text = std::fs::read(format!("/proc/self/fdinfo/{}", supervisor.as_raw_fd())).unwrap();
        let listed = reverie::DescriptorStatus::from_fdinfo(&text)
            .unwrap()
            .locks
            .iter()
            .any(|lock| {
                lock.class == LockClass::Posix
                    && lock.write
                    && (lock.start, lock.end) == (0, Some(9))
            });
        assert_eq!(forked, listed, "F_GETLK and fdinfo disagree");
        forked
    }

    /// An eventfd reports the count its writes left, and a read consumes it.
    #[test]
    fn an_eventfd_reports_its_count() {
        let directory = Directory::new();
        let mut memory = GuestMemory::new(0, PAGE as usize).unwrap();
        let mut executor = ElfExecutor::new(test_state(&directory.0), false);
        let fd = call(
            &mut executor,
            &memory,
            libc::SYS_eventfd2,
            [3, 0, 0, 0, 0, 0],
        );
        assert!(fd >= 0, "eventfd2: {fd}");
        memory.write(VALUE, &4u64.to_ne_bytes()).unwrap();
        assert_eq!(
            call(
                &mut executor,
                &memory,
                libc::SYS_write,
                [fd as u64, VALUE, 8, 0, 0, 0]
            ),
            8
        );
        let status = executor.descriptor_status(fd as i32).unwrap();
        assert_eq!(status.eventfd_count, Some(7));
        assert_eq!(status.scm_fds, None);
        assert!(status.locks.is_empty());
        assert_eq!(
            call(
                &mut executor,
                &memory,
                libc::SYS_read,
                [fd as u64, VALUE, 8, 0, 0, 0]
            ),
            8
        );
        assert_eq!(
            executor.descriptor_status(fd as i32).unwrap().eventfd_count,
            Some(0)
        );
    }

    /// A guest record lock is the host's OFD lock, and is reported as one.
    #[test]
    fn a_record_lock_is_reported_as_the_open_file_description_lock_it_is() {
        let directory = Directory::new();
        let mut memory = GuestMemory::new(0, PAGE as usize).unwrap();
        let mut executor = ElfExecutor::new(test_state(&directory.0), false);
        let fd = open(
            &mut executor,
            &mut memory,
            b"locked",
            libc::O_RDWR | libc::O_CREAT,
        );
        // SAFETY: libc::flock is plain data; an all-zero value is valid.
        let mut lock: libc::flock = unsafe { std::mem::zeroed() };
        lock.l_type = libc::F_WRLCK as i16;
        lock.l_whence = libc::SEEK_SET as i16;
        lock.l_start = 0;
        lock.l_len = 10;
        // SAFETY: the view covers exactly the initialized flock value.
        let bytes = unsafe {
            std::slice::from_raw_parts(
                std::ptr::from_ref(&lock).cast::<u8>(),
                std::mem::size_of::<libc::flock>(),
            )
        };
        memory.write(LOCK, bytes).unwrap();
        assert_eq!(
            call(
                &mut executor,
                &memory,
                libc::SYS_fcntl,
                [fd as u64, libc::F_SETLK as u64, LOCK, 0, 0, 0]
            ),
            0
        );
        let status = executor.descriptor_status(fd).unwrap();
        let metadata = std::fs::metadata(directory.0.join("locked")).unwrap();
        assert_eq!(status.locks.len(), 1, "{status:?}");
        let held = status.locks[0];
        assert_eq!(held.class, LockClass::OpenFileDescription);
        assert!(held.write && !held.mandatory);
        assert_eq!((held.start, held.end), (0, Some(9)));
        assert_eq!(
            held.raw_inode,
            std::os::unix::fs::MetadataExt::ino(&metadata)
        );
        assert_eq!((status.eventfd_count, status.scm_fds), (None, None));
    }

    /// A descriptor the table does not hold is EBADF.
    #[test]
    fn a_closed_descriptor_is_ebadf() {
        let directory = Directory::new();
        let mut memory = GuestMemory::new(0, PAGE as usize).unwrap();
        let mut executor = ElfExecutor::new(test_state(&directory.0), false);
        let fd = open(
            &mut executor,
            &mut memory,
            b"closed",
            libc::O_RDWR | libc::O_CREAT,
        );
        assert!(executor.descriptor_status(fd).is_ok());
        assert_eq!(
            call(
                &mut executor,
                &memory,
                libc::SYS_close,
                [fd as u64, 0, 0, 0, 0, 0]
            ),
            0
        );
        for fd in [fd, -1, 4096] {
            assert_eq!(
                executor.descriptor_status(fd),
                Err(FactError::Errno(reverie::Errno::EBADF)),
                "fd {fd}"
            );
        }
    }

    /// Descriptions the executor keeps itself are not provided: captured
    /// standard output and error, a virtual signalfd and a synthetic `/proc`
    /// file. Uncaptured standard output is the supervisor's own.
    #[test]
    fn descriptions_the_executor_keeps_are_not_provided() {
        let directory = Directory::new();
        let mut memory = GuestMemory::new(0, PAGE as usize).unwrap();
        let mut captured = ElfExecutor::new(test_state(&directory.0), true);
        for fd in [1, 2] {
            assert_eq!(
                captured.descriptor_status(fd),
                Err(FactError::NotProvided),
                "fd {fd}"
            );
        }
        memory.write(VALUE, &0u64.to_ne_bytes()).unwrap();
        let signalfd = call(
            &mut captured,
            &memory,
            libc::SYS_signalfd4,
            [u64::MAX, VALUE, 8, 0, 0, 0],
        );
        assert!(signalfd >= 0, "signalfd4: {signalfd}");
        assert_eq!(
            captured.descriptor_status(signalfd as i32),
            Err(FactError::NotProvided)
        );
        let maps = open(
            &mut captured,
            &mut memory,
            b"/proc/self/maps",
            libc::O_RDONLY,
        );
        assert_eq!(
            captured.descriptor_status(maps),
            Err(FactError::NotProvided)
        );
        let uncaptured = ElfExecutor::new(test_state(&directory.0), false);
        assert!(uncaptured.descriptor_status(1).is_ok());
    }

    /// A query, answered or not, releases no lock the supervisor process
    /// holds on the guest's file. It closes no descriptor of that file, and
    /// closing any one would release every POSIX lock the closing process
    /// holds on it.
    #[test]
    fn a_query_leaves_the_supervisors_record_locks_held() {
        let directory = Directory::new();
        let mut memory = GuestMemory::new(0, PAGE as usize).unwrap();
        // Captured, so that standard output is a descriptor the fact declines.
        let mut executor = ElfExecutor::new(test_state(&directory.0), true);
        let guest = open(
            &mut executor,
            &mut memory,
            b"shared",
            libc::O_RDWR | libc::O_CREAT,
        );
        let closed = open(
            &mut executor,
            &mut memory,
            b"other",
            libc::O_RDWR | libc::O_CREAT,
        );
        close(&mut executor, &memory, closed);
        let path = directory.0.join("shared");
        let supervisor = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .unwrap();
        let lock = record_lock(libc::F_WRLCK, 0, 10);
        // SAFETY: supervisor owns a live descriptor and lock is a valid flock.
        assert_eq!(
            unsafe { libc::fcntl(supervisor.as_raw_fd(), libc::F_SETLK, &lock) },
            0
        );
        assert!(supervisor_lock_held(&path, &supervisor), "before a query");
        // The supervisor's lock is held through its own description, so the
        // guest's fdinfo does not list it.
        let status = executor.descriptor_status(guest).unwrap();
        assert!(status.locks.is_empty(), "{status:?}");
        assert!(
            supervisor_lock_held(&path, &supervisor),
            "after an answered query"
        );
        assert_eq!(executor.descriptor_status(1), Err(FactError::NotProvided));
        assert!(
            supervisor_lock_held(&path, &supervisor),
            "after a declined query"
        );
        for fd in [closed, 4000] {
            assert_eq!(
                executor.descriptor_status(fd),
                Err(FactError::Errno(reverie::Errno::EBADF))
            );
            assert!(
                supervisor_lock_held(&path, &supervisor),
                "after a query of fd {fd}, which is not open"
            );
        }
    }

    /// The executor's own memfds are not provided however the guest holds
    /// them: opened, received through `SCM_RIGHTS` after the sender closed
    /// its descriptor, or duplicated after that, when nothing in the
    /// receiver's table records what the descriptor is. A memfd the guest
    /// made is reported, from the sender and the receiver alike.
    #[test]
    fn executor_memfds_are_declined_after_a_transfer_and_guest_memfds_are_not() {
        let directory = Directory::new();
        let mut memory = GuestMemory::new(0, PAGE as usize).unwrap();
        let mut executor = ElfExecutor::new(test_state(&directory.0), false);
        let [sender, receiver] = socket_pair(&mut executor, &memory);
        for path in [&b"/proc/self/maps"[..], b"/proc/self/status"] {
            let name = String::from_utf8_lossy(path);
            let carrier = open(&mut executor, &mut memory, path, libc::O_RDONLY);
            assert_eq!(
                executor.descriptor_status(carrier),
                Err(FactError::NotProvided),
                "{name}"
            );
            send_descriptor(&mut executor, &mut memory, sender, carrier);
            close(&mut executor, &memory, carrier);
            let received = receive_descriptor(&mut executor, &mut memory, receiver);
            let duplicate = new_fd(
                &mut executor,
                &memory,
                libc::SYS_dup,
                [received as u64, 0, 0, 0, 0, 0],
            );
            for fd in [received, duplicate] {
                assert_eq!(
                    executor.descriptor_status(fd),
                    Err(FactError::NotProvided),
                    "{name}, fd {fd}"
                );
            }
        }
        // A guest memfd, with an open file description lock that travels
        // with its description.
        let guest = memfd(&mut executor, &mut memory, b"guest-object");
        memory
            .write(LOCK, bytes_of(&record_lock(libc::F_RDLCK, 0, 0)))
            .unwrap();
        assert_eq!(
            call(
                &mut executor,
                &memory,
                libc::SYS_fcntl,
                [guest as u64, libc::F_OFD_SETLK as u64, LOCK, 0, 0, 0]
            ),
            0
        );
        let inode = {
            let table = executor.file_table.lock().unwrap();
            std::os::unix::fs::MetadataExt::ino(&table.files[&guest].metadata().unwrap())
        };
        let original = executor.descriptor_status(guest).unwrap();
        assert_eq!(original.locks.len(), 1, "{original:?}");
        let held = original.locks[0];
        assert_eq!(held.class, LockClass::OpenFileDescription);
        assert!(!held.write);
        assert_eq!((held.raw_inode, held.start, held.end), (inode, 0, None));
        send_descriptor(&mut executor, &mut memory, sender, guest);
        close(&mut executor, &memory, guest);
        let received = receive_descriptor(&mut executor, &mut memory, receiver);
        assert_eq!(executor.descriptor_status(received), Ok(original));
        // A name only denies: a guest memfd named as the executor names its
        // own is not provided either.
        let named = memfd(&mut executor, &mut memory, b"reverie-kvm-proc");
        assert_eq!(
            executor.descriptor_status(named),
            Err(FactError::NotProvided)
        );
    }

    /// With captured output, the supervisor's standard output and error are
    /// not provided however the guest holds them: as its standard output and
    /// error, duplicated, or received through `SCM_RIGHTS`, when nothing in
    /// the receiver's table records them as captured output. A standard
    /// number the guest closed is `EBADF`, and one it reused for its own
    /// file reports that file.
    #[test]
    fn captured_output_is_declined_however_the_guest_holds_it() {
        let directory = Directory::new();
        let mut memory = GuestMemory::new(0, PAGE as usize).unwrap();
        let mut executor = ElfExecutor::new(test_state(&directory.0), true);
        let [sender, receiver] = socket_pair(&mut executor, &memory);
        let duplicate = new_fd(&mut executor, &memory, libc::SYS_dup, [1, 0, 0, 0, 0, 0]);
        send_descriptor(&mut executor, &mut memory, sender, 1);
        let received_output = receive_descriptor(&mut executor, &mut memory, receiver);
        let error = new_fd(&mut executor, &memory, libc::SYS_dup, [2, 0, 0, 0, 0, 0]);
        send_descriptor(&mut executor, &mut memory, sender, error);
        close(&mut executor, &memory, error);
        let received_error = receive_descriptor(&mut executor, &mut memory, receiver);
        for fd in [1, 2, duplicate, received_output, received_error] {
            assert_eq!(
                executor.descriptor_status(fd),
                Err(FactError::NotProvided),
                "fd {fd}"
            );
        }
        close(&mut executor, &memory, 1);
        assert_eq!(
            executor.descriptor_status(1),
            Err(FactError::Errno(reverie::Errno::EBADF))
        );
        let file = open(
            &mut executor,
            &mut memory,
            b"output",
            libc::O_RDWR | libc::O_CREAT,
        );
        assert_eq!(
            call(
                &mut executor,
                &memory,
                libc::SYS_dup2,
                [file as u64, 1, 0, 0, 0, 0]
            ),
            1
        );
        let status = executor.descriptor_status(1).unwrap();
        assert_eq!(
            (status.eventfd_count, status.scm_fds, status.locks.len()),
            (None, None, 0)
        );
    }
}
