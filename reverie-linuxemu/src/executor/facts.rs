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
//! its own descriptor. Two consequences are this backend's, not Linux's:
//!
//! - A guest's `F_SETLK` or `F_SETLKW` record lock is held as an open file
//!   description lock (`F_OFD_SETLK`, see `fcntl` in `file.rs`), so it is
//!   reported with class `OpenFileDescription`, where Linux would report
//!   `Posix`. This is a named gap of the provider, not a parse error.
//! - A guest's `flock(2)` is refused (`UnsupportedOperation::Flock`), so a
//!   flock lock is reported only for a description the guest received
//!   already locked, such as an inherited standard input.
//!
//! A descriptor the executor keeps itself, whose host carrier is not the
//! guest's object, is not provided: a synthetic `/proc` file or `fdinfo`
//! description (a memfd carrier), a virtual signalfd (an eventfd carrier),
//! and captured standard output or error (the capture's own pipe, or none).
//!
//! `signal_sets`: the executor delivers the guest's signals itself, so the
//! five sets are its own records (gVisor:
//! pkg/sentry/fsimpl/proc/task_files.go, whose `status` prints them from the
//! Sentry's task and thread group): see `ElfExecutor::signal_sets`.

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
        let pinned = {
            // The table the task's syscalls install before they run.
            let table = self
                .file_table
                .lock()
                .expect("KVM file-table lock poisoned");
            let host = descriptor_source(&table, fd, self.output.is_some())?;
            // Pin the open file description before the table is released, so
            // that its number cannot be reused during the read; the table is
            // never held across procfs I/O.
            // SAFETY: the table owns `host`, or it is the supervisor's open
            // standard descriptor; F_DUPFD_CLOEXEC returns a new owned one.
            let pinned = unsafe { libc::fcntl(host, libc::F_DUPFD_CLOEXEC, 0) };
            if pinned < 0 {
                return Err(reverie::FactError::Errno(reverie::Errno::last()));
            }
            // SAFETY: a successful F_DUPFD_CLOEXEC transfers ownership.
            unsafe { std::fs::File::from_raw_fd(pinned) }
        };
        match read_own_fdinfo(&pinned) {
            Ok(Some(text)) => reverie::DescriptorStatus::from_fdinfo(&text),
            Ok(None) => Err(reverie::FactError::NotProvided),
            Err(error) => Err(reverie::FactError::Errno(reverie::Errno::new(
                error.raw_os_error().unwrap_or(libc::EIO),
            ))),
        }
    }
}

/// The host descriptor behind guest descriptor `fd` in `table`, as the
/// task's own syscalls resolve it, for a description the host kernel keeps
/// as the guest's.
fn descriptor_source(
    table: &FileTableState,
    fd: libc::c_int,
    capture_output: bool,
) -> std::result::Result<RawFd, reverie::FactError> {
    let open_standard = (0..=2).contains(&fd)
        && !table.files.contains_key(&fd)
        && !table.closed_standard_fds.contains(&fd)
        && (fd != libc::STDIN_FILENO || table.stdin.is_some());
    if table.proc_files.contains_key(&fd)
        || table.fdinfo_files.contains_key(&fd)
        || table.signalfd_fds.contains(&fd)
        || (capture_output
            && (table.stdout_alias_fds.contains(&fd)
                || table.stderr_alias_fds.contains(&fd)
                || (open_standard && fd != libc::STDIN_FILENO)))
    {
        return Err(reverie::FactError::NotProvided);
    }
    let host = match table.files.get(&fd) {
        Some(file) => Some(file.as_raw_fd()),
        None if !open_standard => None,
        None if fd == libc::STDIN_FILENO => table.stdin.as_ref().map(AsRawFd::as_raw_fd),
        // Uncaptured standard output or error is the supervisor's own.
        None => Some(fd),
    };
    host.ok_or(reverie::FactError::Errno(reverie::Errno::EBADF))
}

/// The supervisor's own `fdinfo` for `file`, or `None` when it is longer
/// than [`FDINFO_LIMIT`]. `file` stays open for the whole read, so its number
/// names it throughout.
fn read_own_fdinfo(file: &std::fs::File) -> std::io::Result<Option<Vec<u8>>> {
    let source = std::fs::File::open(format!("/proc/self/fdinfo/{}", file.as_raw_fd()))?;
    let mut text = Vec::new();
    std::io::Read::read_to_end(
        &mut std::io::Read::take(source, FDINFO_LIMIT + 1),
        &mut text,
    )?;
    Ok((text.len() as u64 <= FDINFO_LIMIT).then_some(text))
}

impl ElfExecutor {
    /// `reverie::Guest::signal_sets` for thread `tid` of this task's
    /// process, from the signal state the executor delivers from, in the
    /// shape of Linux's `task_sig` (fs/proc/array.c):
    ///
    /// - `SigPnd`: the thread's own queue (`ThreadSignalState::pending`);
    /// - `ShdPnd`: the process's shared queue
    ///   (`ProcessSignalState::shared_pending`);
    /// - `SigBlk`: the thread's mask (`ThreadSignalState::blocked`), which
    ///   holds a temporary mask while one is installed, as Linux's
    ///   `task->blocked` does, and not the mask saved to restore after it
    ///   (`saved_sigmask`);
    /// - `SigIgn` and `SigCgt`: the process's actions, as Linux's
    ///   `collect_sigign_sigcatch` builds them: `SIG_IGN` is ignored, and any
    ///   handler but `SIG_DFL` and `SIG_IGN` is caught.
    ///
    /// A queue's set holds only its current entries: an entry queued before
    /// an action that discards the signal (`pending_generations`) is no
    /// longer pending, as `rt_sigpending` also counts it.
    ///
    /// Another thread's state is the one its own syscalls and deliveries use:
    /// its lifecycle registration lends the same shared state, never a copy
    /// (`TaskLifecycleTable::signal_target`), and the locks are taken in the
    /// order every signal path takes them (lifecycle, then the process's
    /// signals, then the thread's), so the answer is the state between two
    /// of its changes, never part of one.
    ///
    /// `ESRCH` when `tid` names no live task of this process in the run's
    /// lifecycle table. Not provided for a registered task whose signal state
    /// is already gone, which a thread-directed send refuses as unreachable
    /// too.
    ///
    /// The executor refuses real-time signals (`SignalRefusal`), so no
    /// real-time signal is ever pending here.
    pub fn signal_sets(
        &self,
        tid: libc::pid_t,
    ) -> std::result::Result<reverie::SignalSets, reverie::FactError> {
        let lifecycle = self
            .state
            .task_lifecycle
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if !lifecycle.get(tid).is_some_and(|task| {
            task.tgid == self.state.pid && task.process_generation == self.process_generation
        }) {
            return Err(reverie::FactError::Errno(reverie::Errno::ESRCH));
        }
        let thread = if tid == self.state.tid {
            self.state.thread_signals.clone()
        } else {
            lifecycle
                .signal_target(tid)
                .ok_or(reverie::FactError::NotProvided)?
        };
        let process = self
            .state
            .process_signals
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
    /// thread whose signal state is gone is not provided.
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
            // Registered, but with no signal state of its own.
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
        assert_eq!(executor.signal_sets(tid + 2), Err(FactError::NotProvided));
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

    /// A number no task of the process has is ESRCH: zero and below, a
    /// number no task has, and a task of another process.
    #[test]
    fn threads_outside_the_process_are_esrch() {
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
}
