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
//!
//! `signal_sets`: the executor delivers the guest's signals itself, so the
//! five sets are its own records, for a live thread of any of the run's
//! processes (gVisor: pkg/sentry/fsimpl/proc/task_files.go, whose `status`
//! prints them for any task from the Sentry's task and thread group): see
//! `ElfExecutor::signal_sets`.

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

impl ElfExecutor {
    /// `reverie::Guest::signal_sets` for thread `tid`, a live task of this
    /// run in any process, from the signal state the executor delivers from,
    /// in the shape of Linux's `task_sig` (fs/proc/array.c):
    ///
    /// - `SigPnd`: the thread's own queue (`ThreadSignalState::pending`);
    /// - `ShdPnd`: its process's shared queue
    ///   (`ProcessSignalState::shared_pending`);
    /// - `SigBlk`: the thread's mask (`ThreadSignalState::blocked`), which
    ///   holds a temporary mask while one is installed, as Linux's
    ///   `task->blocked` does, and not the mask saved to restore after it
    ///   (`saved_sigmask`). The executor installs one only for a delivery at
    ///   the return boundary; it keeps no waiting state, since it answers
    ///   `rt_sigtimedwait` and a masked `ppoll` or `pselect6` at once and
    ///   refuses an `rt_sigsuspend` that would wait, so a thread that waits
    ///   in one of them shows its own mask, where Linux shows the call's
    ///   mask, or for `rt_sigtimedwait` the mask without the signals it waits
    ///   for;
    /// - `SigIgn` and `SigCgt`: its process's actions, as Linux's
    ///   `collect_sigign_sigcatch` builds them: `SIG_IGN` is ignored, and any
    ///   handler but `SIG_DFL` and `SIG_IGN` is caught.
    ///
    /// A queue's set holds only its current entries: an entry queued before
    /// an action that discards the signal (`pending_generations`) is no
    /// longer pending, as `rt_sigpending` also counts it.
    ///
    /// Every state read is the one the target's own syscalls and deliveries
    /// use, never a copy, reached under the run's lifecycle lock: the thread's
    /// through its lifecycle registration (`TaskLifecycleTable::signal_target`)
    /// and, for another process's thread, the process's through the record
    /// the run keeps of each process's state by its exact identity, thread
    /// group and process generation (`ParentDeathState::signals`, which
    /// process creation and exec write under the same lock). The locks are
    /// then taken in the order every signal path takes them (lifecycle, the
    /// process's signals, the thread's), and a change to both states holds
    /// both locks throughout, as a captured trap's does
    /// (`prepare_captured_trap`), so the answer is the state between two of
    /// its changes, never part of one.
    ///
    /// `ESRCH` when `tid` names no task that is live in the run's lifecycle
    /// table with both states reachable: no such number, an exited task, or a
    /// child between the steps of its fork, which registers the child's
    /// thread before its process's state, each under its own hold of the
    /// lifecycle lock. An exec likewise replaces the thread's state before
    /// its process's, but keeps the replaced image's process state alive
    /// until the record names the new one (`replace_after_exec`): in between,
    /// that state, with the thread's copy of its own, is the process as the
    /// exec found it, so a live thread is never `ESRCH` across its exec.
    ///
    /// The executor refuses real-time signals (`SignalRefusal`), so no
    /// real-time signal is ever pending here.
    pub fn signal_sets(
        &self,
        tid: libc::pid_t,
    ) -> std::result::Result<reverie::SignalSets, reverie::FactError> {
        let no_thread = reverie::FactError::Errno(reverie::Errno::ESRCH);
        let lifecycle = self
            .state
            .task_lifecycle
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let task = lifecycle.get(tid).ok_or(no_thread)?;
        let own_process =
            task.tgid == self.state.pid && task.process_generation == self.process_generation;
        let thread = if tid == self.state.tid && own_process {
            self.state.thread_signals.clone()
        } else {
            lifecycle.signal_target(tid).ok_or(no_thread)?
        };
        let process = if own_process {
            self.state.process_signals.clone()
        } else {
            lifecycle
                .parent_death
                .signals
                .get(&(task.tgid, task.process_generation))
                .and_then(std::sync::Weak::upgrade)
                .ok_or(no_thread)?
        };
        let process = process
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let thread = thread.lock();
        let generations = &process.pending_generations;
        let mut ignored = KernelSigset::default();
        let mut caught = KernelSigset::default();
        for (&signal, action) in &process.dispositions {
            if action.is_ignored() {
                ignored.insert(signal);
            } else if action.handler != libc::SIG_DFL as u64 {
                caught.insert(signal);
            }
        }
        Ok(reverie::SignalSets::new(
            set_bits(thread.pending.pending_mask(generations)),
            set_bits(process.shared_pending.pending_mask(generations)),
            set_bits(thread.blocked),
            set_bits(ignored),
            set_bits(caught),
        ))
    }
}

/// `set` as the fact holds it: signal `n` at bit `n - 1`, the bit
/// `KernelSigset` keeps in byte `(n - 1) / 8`.
fn set_bits(set: KernelSigset) -> u64 {
    u64::from_le_bytes(set.to_bytes())
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

#[cfg(test)]
mod signal_sets_tests {
    use std::sync::atomic::AtomicU64;
    use std::sync::atomic::Ordering;

    use reverie::FactError;
    use reverie::SignalSets;

    use super::*;

    const PAGE: usize = 4096;
    /// Guest addresses of the tests' operands.
    const MASK: u64 = 0x100;
    const ACTION: u64 = 0x200;
    const OLD_MASK: u64 = 0x300;

    struct Directory(std::path::PathBuf);

    impl Directory {
        fn new() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let path = std::env::temp_dir().join(format!(
                "reverie-linuxemu-signal-sets-{}-{}",
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

    fn bit(signal: libc::c_int) -> u64 {
        1 << (signal - 1)
    }

    fn call(
        executor: &mut ElfExecutor,
        memory: &GuestMemory,
        number: libc::c_long,
        args: [u64; 6],
    ) -> i64 {
        executor.execute(&SyscallRequest::new(number as u64, args), memory)
    }

    /// `rt_sigprocmask(how, mask)`.
    fn mask(executor: &mut ElfExecutor, memory: &mut GuestMemory, how: libc::c_int, mask: u64) {
        memory.write(MASK, &mask.to_le_bytes()).unwrap();
        let how = how as u64;
        let result = call(
            executor,
            memory,
            libc::SYS_rt_sigprocmask,
            [how, MASK, OLD_MASK, 8, 0, 0],
        );
        assert_eq!(result, 0);
    }

    /// `rt_sigaction(signal, handler)`, with an empty mask and no flags.
    fn act(
        executor: &mut ElfExecutor,
        memory: &mut GuestMemory,
        signal: libc::c_int,
        handler: u64,
    ) -> i64 {
        let mut action = [0u8; 32];
        action[..8].copy_from_slice(&handler.to_le_bytes());
        memory.write(ACTION, &action).unwrap();
        call(
            executor,
            memory,
            libc::SYS_rt_sigaction,
            [signal as u64, ACTION, 0, 8, 0, 0],
        )
    }

    fn own(executor: &ElfExecutor) -> std::result::Result<SignalSets, FactError> {
        executor.signal_sets(executor.state.tid)
    }

    /// A signal the thread blocks and sends to itself is pending for it
    /// alone; one it sends to its process is pending for the process. The
    /// actions show as Linux shows them: `SIG_IGN` ignored, a handler caught,
    /// and an explicit `SIG_DFL` neither.
    #[test]
    fn the_sets_are_the_executors_queues_mask_and_actions() {
        let directory = Directory::new();
        let mut memory = GuestMemory::new(0, PAGE).unwrap();
        let mut executor = ElfExecutor::new(test_state(&directory.0), false);
        assert_eq!(own(&executor), Ok(SignalSets::new(0, 0, 0, 0, 0)));
        let (pid, tid) = (executor.state.pid as u64, executor.state.tid as u64);
        mask(
            &mut executor,
            &mut memory,
            libc::SIG_BLOCK,
            bit(libc::SIGUSR1) | bit(libc::SIGUSR2),
        );
        let usr1 = libc::SIGUSR1 as u64;
        let usr2 = libc::SIGUSR2 as u64;
        assert_eq!(
            call(
                &mut executor,
                &memory,
                libc::SYS_tgkill,
                [pid, tid, usr1, 0, 0, 0]
            ),
            0
        );
        assert_eq!(
            call(
                &mut executor,
                &memory,
                libc::SYS_kill,
                [pid, usr2, 0, 0, 0, 0]
            ),
            0
        );
        assert_eq!(act(&mut executor, &mut memory, libc::SIGHUP, 1), 0);
        assert_eq!(act(&mut executor, &mut memory, libc::SIGTERM, 0x1000), 0);
        assert_eq!(act(&mut executor, &mut memory, libc::SIGINT, 0), 0);
        // Linux refuses an action for SIGKILL and SIGSTOP.
        assert_eq!(
            act(&mut executor, &mut memory, libc::SIGSTOP, 1),
            -(libc::EINVAL as i64)
        );
        assert_eq!(
            own(&executor),
            Ok(SignalSets::new(
                bit(libc::SIGUSR1),
                bit(libc::SIGUSR2),
                bit(libc::SIGUSR1) | bit(libc::SIGUSR2),
                bit(libc::SIGHUP),
                bit(libc::SIGTERM),
            ))
        );
    }

    /// A full mask leaves SIGKILL and SIGSTOP out, as Linux's does. A
    /// temporary mask is the mask while it is installed, and the mask saved
    /// to restore after it is not reported.
    #[test]
    fn the_mask_is_the_one_in_force() {
        let directory = Directory::new();
        let mut memory = GuestMemory::new(0, PAGE).unwrap();
        let mut executor = ElfExecutor::new(test_state(&directory.0), false);
        mask(&mut executor, &mut memory, libc::SIG_SETMASK, u64::MAX);
        let full = !(bit(libc::SIGKILL) | bit(libc::SIGSTOP));
        assert_eq!(own(&executor).unwrap().blocked, full);
        let mut temporary = KernelSigset::default();
        temporary.insert(libc::SIGQUIT);
        install_temporary_mask_for_delivery(&executor.state, temporary);
        assert_eq!(
            executor.state.thread_signals.lock().saved_sigmask,
            Some(KernelSigset::from_bytes(full.to_le_bytes()))
        );
        assert_eq!(own(&executor).unwrap().blocked, bit(libc::SIGQUIT));
        executor.restore_saved_signal_mask();
        assert_eq!(own(&executor).unwrap().blocked, full);
    }

    /// An action that discards a signal ends its pending instances, which
    /// are then neither pending nor counted, as in Linux. A real-time signal
    /// is refused, so it is never pending here.
    #[test]
    fn discarded_and_refused_signals_are_not_pending() {
        let directory = Directory::new();
        let mut memory = GuestMemory::new(0, PAGE).unwrap();
        let mut executor = ElfExecutor::new(test_state(&directory.0), false);
        let (pid, tid) = (executor.state.pid as u64, executor.state.tid as u64);
        let realtime = libc::SIGRTMIN();
        mask(
            &mut executor,
            &mut memory,
            libc::SIG_BLOCK,
            bit(libc::SIGUSR1) | bit(realtime),
        );
        let usr1 = libc::SIGUSR1 as u64;
        assert_eq!(
            call(
                &mut executor,
                &memory,
                libc::SYS_tgkill,
                [pid, tid, usr1, 0, 0, 0]
            ),
            0
        );
        assert_eq!(own(&executor).unwrap().pending, bit(libc::SIGUSR1));
        assert_eq!(act(&mut executor, &mut memory, libc::SIGUSR1, 1), 0);
        let sets = own(&executor).unwrap();
        assert_eq!((sets.pending, sets.ignored), (0, bit(libc::SIGUSR1)));
        assert_eq!(
            call(
                &mut executor,
                &memory,
                libc::SYS_tgkill,
                [pid, tid, realtime as u64, 0, 0, 0]
            ),
            -(libc::ENOSYS as i64)
        );
        assert_eq!(own(&executor).unwrap().pending, 0);
    }

    /// Another live thread of the process answers with its own mask and
    /// queue, beside the process's shared queue and actions; a registered
    /// thread with no signal state is ESRCH.
    #[test]
    fn a_sibling_answers_with_its_own_state() {
        let directory = Directory::new();
        let mut memory = GuestMemory::new(0, PAGE).unwrap();
        let mut executor = ElfExecutor::new(test_state(&directory.0), false);
        let (pid, tid) = (executor.state.pid, executor.state.tid);
        assert_eq!(act(&mut executor, &mut memory, libc::SIGHUP, 1), 0);
        let sibling = SharedThreadSignalState::default();
        sibling.lock().blocked.insert(libc::SIGWINCH);
        let generation = {
            let mut lifecycle = executor.state.task_lifecycle.lock().unwrap();
            let generation = lifecycle.register_with_signals(tid + 1, pid, pid, true, &sibling);
            // Registered, but with no signal state of its own: a thread
            // between steps of its creation.
            lifecycle.register(tid + 2, pid, pid, true);
            generation
        };
        assert_eq!(
            executor.signal_sets(tid + 1),
            Ok(SignalSets::new(
                0,
                0,
                bit(libc::SIGWINCH),
                bit(libc::SIGHUP),
                0
            ))
        );
        assert_eq!(
            executor.signal_sets(tid + 2),
            Err(FactError::Errno(reverie::Errno::ESRCH))
        );
        // A sibling is reachable while it is registered, not after it exits.
        executor
            .state
            .task_lifecycle
            .lock()
            .unwrap()
            .remove(tid + 1, generation);
        assert_eq!(
            executor.signal_sets(tid + 1),
            Err(FactError::Errno(reverie::Errno::ESRCH))
        );
        drop(sibling);
    }

    /// A signal pending in a sibling's own queue stops being pending when the
    /// process sets an action that discards it: the entry stays in that
    /// queue, an instance of a generation the action ended, and is not
    /// reported, as Linux, which removes it, does not report it.
    #[test]
    fn a_discarded_signal_in_a_siblings_queue_is_not_pending() {
        let directory = Directory::new();
        let mut memory = GuestMemory::new(0, PAGE).unwrap();
        let mut executor = ElfExecutor::new(test_state(&directory.0), false);
        let (pid, tid) = (executor.state.pid, executor.state.tid);
        let sibling = SharedThreadSignalState::default();
        sibling.lock().blocked.insert(libc::SIGUSR1);
        executor
            .state
            .task_lifecycle
            .lock()
            .unwrap()
            .register_with_signals(tid + 1, pid, pid, true, &sibling);
        let usr1 = libc::SIGUSR1 as u64;
        assert_eq!(
            call(
                &mut executor,
                &memory,
                libc::SYS_tgkill,
                [pid as u64, (tid + 1) as u64, usr1, 0, 0, 0]
            ),
            0
        );
        assert_eq!(
            executor.signal_sets(tid + 1).unwrap().pending,
            bit(libc::SIGUSR1)
        );
        assert_eq!(act(&mut executor, &mut memory, libc::SIGUSR1, 1), 0);
        assert!(sibling.lock().pending.contains(libc::SIGUSR1));
        let sets = executor.signal_sets(tid + 1).unwrap();
        assert_eq!((sets.pending, sets.ignored), (0, bit(libc::SIGUSR1)));
    }

    /// A process's state, as another process holds it: its actions, its
    /// shared queue, and the given thread's queue and mask.
    fn other_process(
        tgid: i32,
    ) -> (
        Arc<std::sync::Mutex<ProcessSignalState>>,
        SharedThreadSignalState,
    ) {
        let mut process = ProcessSignalState::default();
        process.dispositions.insert(
            libc::SIGINT,
            KernelSigaction {
                handler: libc::SIG_IGN as u64,
                ..KernelSigaction::default()
            },
        );
        process.dispositions.insert(
            libc::SIGUSR1,
            KernelSigaction {
                handler: 0x1000,
                ..KernelSigaction::default()
            },
        );
        process
            .shared_pending
            .enqueue(event_for_process(libc::SIGTERM, tgid).unwrap(), 0)
            .unwrap();
        let thread = SharedThreadSignalState::default();
        {
            let mut thread = thread.lock();
            thread.blocked.insert(libc::SIGUSR2);
            thread.blocked.insert(libc::SIGTERM);
            thread
                .pending
                .enqueue(event_for_thread(libc::SIGUSR2, tgid, tgid).unwrap(), 0)
                .unwrap();
        }
        (Arc::new(std::sync::Mutex::new(process)), thread)
    }

    /// A thread of another process answers from that process's own state,
    /// which the run keeps by its exact identity: its own queue and mask, the
    /// process's shared queue and actions. Another of its threads shares the
    /// process's part. Once the process's state is gone, as after its exit,
    /// ESRCH.
    #[test]
    fn another_process_answers_from_its_own_state() {
        let directory = Directory::new();
        let executor = ElfExecutor::new(test_state(&directory.0), false);
        let other = executor.state.pid + 10;
        let (process, leader) = other_process(other);
        let sibling = SharedThreadSignalState::default();
        sibling.lock().blocked.insert(libc::SIGTERM);
        let (leader_generation, sibling_generation) = {
            let mut lifecycle = executor.state.task_lifecycle.lock().unwrap();
            let leader_generation =
                lifecycle.register_with_signals(other, other, other, true, &leader);
            let sibling_generation =
                lifecycle.register_with_signals(other + 1, other, other, true, &sibling);
            let process_generation = lifecycle.get(other).unwrap().process_generation;
            assert_eq!(
                lifecycle.get(other + 1).unwrap().process_generation,
                process_generation
            );
            lifecycle
                .parent_death
                .signals
                .insert((other, process_generation), Arc::downgrade(&process));
            (leader_generation, sibling_generation)
        };
        let actions = (bit(libc::SIGINT), bit(libc::SIGUSR1));
        let expect = |pending, blocked, (ignored, caught): (u64, u64)| {
            SignalSets::new(pending, bit(libc::SIGTERM), blocked, ignored, caught)
        };
        assert_eq!(
            executor.signal_sets(other),
            Ok(expect(
                bit(libc::SIGUSR2),
                bit(libc::SIGUSR2) | bit(libc::SIGTERM),
                actions
            ))
        );
        assert_eq!(
            executor.signal_sets(other + 1),
            Ok(expect(0, bit(libc::SIGTERM), actions))
        );
        drop(process);
        for tid in [other, other + 1] {
            assert_eq!(
                executor.signal_sets(tid),
                Err(FactError::Errno(reverie::Errno::ESRCH)),
                "tid {tid}"
            );
        }
        let mut lifecycle = executor.state.task_lifecycle.lock().unwrap();
        lifecycle.remove(other, leader_generation);
        lifecycle.remove(other + 1, sibling_generation);
    }

    /// Across another process's exec, which replaces the thread's state and
    /// then, under a second hold of the lifecycle lock, the process's, a
    /// reader sees one image or the other, never a mix: between the two the
    /// thread's new state is a copy of its old one beside the replaced
    /// image's process state, the process as the exec found it; after both,
    /// the new image, whose handlers are reset. The two steps by hand; see
    /// `a_reader_never_finds_an_execing_thread_gone` for the real exec.
    #[test]
    fn an_exec_in_another_process_shows_one_image_or_the_other() {
        let directory = Directory::new();
        let executor = ElfExecutor::new(test_state(&directory.0), false);
        let other = executor.state.pid + 20;
        let (process, thread) = other_process(other);
        let process_generation = {
            let mut lifecycle = executor.state.task_lifecycle.lock().unwrap();
            lifecycle.register_with_signals(other, other, other, true, &thread);
            let process_generation = lifecycle.get(other).unwrap().process_generation;
            lifecycle
                .parent_death
                .signals
                .insert((other, process_generation), Arc::downgrade(&process));
            process_generation
        };
        let before = executor.signal_sets(other).unwrap();
        assert_eq!(before.caught, bit(libc::SIGUSR1));
        // The exec's first step: the thread's state replaced by its copy.
        let replaced_thread = thread.after_exec();
        executor
            .state
            .task_lifecycle
            .lock()
            .unwrap()
            .reset_after_exec_with_signals(other, other, other, &replaced_thread);
        assert_eq!(executor.signal_sets(other), Ok(before));
        // The second: the process's state replaced, its handlers reset.
        let replaced_process =
            Arc::new(std::sync::Mutex::new(process.lock().unwrap().after_exec()));
        executor
            .state
            .task_lifecycle
            .lock()
            .unwrap()
            .parent_death
            .signals
            .insert(
                (other, process_generation),
                Arc::downgrade(&replaced_process),
            );
        drop(process);
        let after = executor.signal_sets(other).unwrap();
        assert_eq!(
            after,
            SignalSets::new(
                before.pending,
                before.shared_pending,
                before.blocked,
                bit(libc::SIGINT),
                0
            )
        );
        drop(thread);
    }

    /// A number with no live task, or with a task whose process's state the
    /// run does not hold (a child between the steps of its fork), is ESRCH:
    /// zero and below, a number no task has, and such a task.
    #[test]
    fn numbers_without_a_live_task_or_its_state_are_esrch() {
        let directory = Directory::new();
        let executor = ElfExecutor::new(test_state(&directory.0), false);
        let pid = executor.state.pid;
        let other = SharedThreadSignalState::default();
        executor
            .state
            .task_lifecycle
            .lock()
            .unwrap()
            .register_with_signals(pid + 10, pid + 10, pid + 10, true, &other);
        for tid in [0, -1, i32::MIN, pid + 5, pid + 10, i32::MAX] {
            assert_eq!(
                executor.signal_sets(tid),
                Err(FactError::Errno(reverie::Errno::ESRCH)),
                "tid {tid}"
            );
        }
    }

    /// A thread-local slot for a test hook.
    type HookSlot = std::thread::LocalKey<std::cell::RefCell<Option<Box<dyn FnMut()>>>>;

    /// Installs a test hook in a thread-local slot of this thread, and takes
    /// it out again when dropped.
    struct Hook(&'static HookSlot);

    impl Hook {
        fn install(slot: &'static HookSlot, hook: impl FnMut() + 'static) -> Self {
            slot.with(|cell| {
                assert!(cell.borrow().is_none(), "nested hook");
                *cell.borrow_mut() = Some(Box::new(hook));
            });
            Self(slot)
        }
    }

    impl Drop for Hook {
        fn drop(&mut self) {
            self.0.with(|cell| cell.borrow_mut().take());
        }
    }

    /// A real exec of another process, with a reader in its window (the
    /// thread's state replaced, the process's not yet rebound), as a
    /// concurrent reader can be. The exec drops its own hold of the replaced
    /// image's process state there, as it always has; the reader still gets
    /// the process as the exec found it, never ESRCH for the live thread.
    /// After the exec, the new image: the handler reset, the mask kept.
    #[test]
    fn a_reader_never_finds_an_execing_thread_gone() {
        let directory = Directory::new();
        let mut memory = GuestMemory::new(0, PAGE).unwrap();
        let parent = std::rc::Rc::new(ElfExecutor::new(test_state(&directory.0), false));
        let mut child = parent
            .fork_child(parent.state.pid + 30, false, false)
            .unwrap();
        let child_tid = child.state.tid;
        mask(&mut child, &mut memory, libc::SIG_BLOCK, bit(libc::SIGUSR2));
        assert_eq!(act(&mut child, &mut memory, libc::SIGUSR1, 0x1000), 0);
        let before = SignalSets::new(0, 0, bit(libc::SIGUSR2), 0, bit(libc::SIGUSR1));
        assert_eq!(parent.signal_sets(child_tid), Ok(before));
        let during = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let hook = Hook::install(&EXEC_WINDOW_TEST_HOOK, {
            let (parent, during) = (parent.clone(), during.clone());
            move || during.borrow_mut().push(parent.signal_sets(child_tid))
        });
        child.replace_after_exec(test_state(&directory.0));
        drop(hook);
        assert_eq!(*during.borrow(), [Ok(before)]);
        assert_eq!(
            parent.signal_sets(child_tid),
            Ok(SignalSets::new(0, 0, bit(libc::SIGUSR2), 0, 0))
        );
    }

    /// A trap's preparation unblocks its signal and resets its action as one
    /// change, as Linux makes both under one siglock: a reader on another
    /// thread, asked to read between the two, can only read once both are
    /// made. Before, the signal is blocked and caught; after, neither.
    #[test]
    fn a_trap_unblocks_and_resets_its_signal_in_one_change() {
        let directory = Directory::new();
        let mut memory = GuestMemory::new(0, PAGE).unwrap();
        let mut executor = ElfExecutor::new(test_state(&directory.0), false);
        let tid = executor.state.tid;
        let segv = bit(libc::SIGSEGV);
        mask(&mut executor, &mut memory, libc::SIG_BLOCK, segv);
        assert_eq!(act(&mut executor, &mut memory, libc::SIGSEGV, 0x1000), 0);
        let reader = executor.thread_child(tid + 1).unwrap();
        assert_eq!(
            reader.signal_sets(tid),
            Ok(SignalSets::new(0, 0, segv, 0, segv))
        );
        let (ask, asked) = std::sync::mpsc::channel::<()>();
        let (answer, answered) = std::sync::mpsc::channel();
        let reading = std::thread::spawn(move || {
            asked.recv().unwrap();
            answer.send(reader.signal_sets(tid)).unwrap();
        });
        let answered = std::rc::Rc::new(answered);
        let during = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let hook = Hook::install(&CAPTURED_TRAP_TEST_HOOK, {
            let (answered, during) = (answered.clone(), during.clone());
            move || {
                ask.send(()).unwrap();
                // A reader that gets in here reads half the change.
                if let Ok(sets) = answered.recv_timeout(std::time::Duration::from_millis(200)) {
                    during.borrow_mut().push(sets);
                }
            }
        });
        executor.prepare_captured_trap(libc::SIGSEGV);
        drop(hook);
        assert_eq!(*during.borrow(), []);
        assert_eq!(answered.recv(), Ok(Ok(SignalSets::new(0, 0, 0, 0, 0))));
        reading.join().unwrap();
    }
}
