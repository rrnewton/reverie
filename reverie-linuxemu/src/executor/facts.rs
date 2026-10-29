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
//!
//! `link_path`: a guest descriptor the executor backs with a host descriptor
//! of the same open file description names what the host kernel's `d_path`
//! names for that description, read by the supervisor through its own
//! `/proc/self/fd/<host fd>` while the descriptor table is locked; the
//! process's image and the thread's working directory are the supervisor's
//! own handles of them (gVisor: pkg/sentry/fsimpl/proc/task_fds.go and
//! task_files.go, whose `fd/<fd>`, `exe` and `cwd` render the Sentry's own
//! file description, executable and working directory). An object the
//! executor keeps itself names what Linux names for the object the guest
//! sees, or is not provided: captured output is a pipe of its own, a virtual
//! signalfd `anon_inode:[signalfd]`, and the executor's own memfds, and with
//! captured output the supervisor's standard output or error however the
//! guest holds them, are not provided. See `ElfExecutor::link_path` for the
//! threads it answers for.
//!
//! `memory_map`: the executor keeps the guest's address space page by page,
//! with what each page was reserved for and whether it is accessible and
//! writable, but no mapping list, no file identity, no read or execute
//! permission of its own, and none of the history that decides where Linux
//! splits and merges mappings. It lists the one mapping it can establish
//! exactly for its own address space, the process's `[stack]` while nothing
//! has changed the stack window since the image started (in this process, or
//! in a vfork child or parent that shares it), and says that the list is not
//! complete (gVisor:
//! pkg/sentry/fsimpl/proc/task_files.go, whose `maps` the Sentry renders from
//! its own memory manager's list). See `ElfExecutor::memory_map`.

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
    let (host, _) = table_host_fd(table, fd)?;
    if table.signalfd_fds.contains(&fd) {
        return Err(reverie::FactError::NotProvided);
    }
    Ok(host)
}

/// The host descriptor behind guest descriptor `fd` in `table`, as the
/// task's own syscalls resolve it (`host_fd` in `file.rs`), and whether `fd`
/// is a standard descriptor the guest has neither replaced nor closed
/// (`is_open_standard`), whose standard output and error are the
/// supervisor's own. `EBADF` when the table does not hold `fd`.
fn table_host_fd(
    table: &FileTableState,
    fd: libc::c_int,
) -> std::result::Result<(RawFd, bool), reverie::FactError> {
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
    host.map(|host| (host, open_standard))
        .ok_or(reverie::FactError::Errno(reverie::Errno::EBADF))
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
    capture_output && supervisor_output(host)
}

/// Whether host descriptor `host` is the supervisor's standard output or
/// error, compared by device and inode, or an object that cannot be
/// identified: with captured output, the guest's writes never reach it.
fn supervisor_output(host: RawFd) -> bool {
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
    ///
    /// Under a Tool, a signal whose action ignores it, and which its target
    /// does not block, stays pending until the target's next return boundary
    /// (`ThreadSignalState::observe_ignored`): the Tool observes it there,
    /// before the final disposition discards it, as a tracer observes such a
    /// signal at a ptrace signal-delivery stop. Linux discards it when it is
    /// sent, unless the target is traced (`sig_ignored`, kernel/signal.c at
    /// v6.12, line 104), so until that boundary it is pending here and not on
    /// Linux. A child's exit `SIGCHLD` under an explicit `SIG_IGN` is the
    /// exception: its publication suppresses it, as Linux does.
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
        Ok(crate::signal::task_signal_sets(&process, &thread))
    }
}

/// Which thread of this process a link of [`ElfExecutor::link_path`] is
/// asked for.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum LinkOwner {
    /// The asking thread itself.
    Caller,
    /// Another live thread of the asking thread's process.
    Sibling,
}

impl ElfExecutor {
    /// `reverie::Guest::link_path` for link `link` of thread `tid`: the name
    /// Linux's `readlink(2)` gives for it, from the state the executor keeps.
    ///
    /// - `fd/<fd>`: the descriptor table the task's syscalls install, locked
    ///   until the read is done, as `descriptor_status` locks it; the
    ///   executor runs every thread of a process with that one table
    ///   (`clone` without `CLONE_FILES` is refused). A descriptor the
    ///   executor backs with a host descriptor of the same open file
    ///   description names what the host kernel names for that description:
    ///   the supervisor's `readlink` of its own `/proc/self/fd/<host fd>`,
    ///   which opens and closes nothing, so it releases no POSIX lock the
    ///   supervisor holds. Captured standard output or error, and a
    ///   descriptor the guest made of it, is the pipe its `fstat` describes,
    ///   `pipe:[<inode>]`; a virtual signalfd is `anon_inode:[signalfd]`, as
    ///   Linux names a signalfd. The executor's own objects are not
    ///   provided, judged by the host object as `descriptor_status` judges
    ///   them, so a descriptor received through `SCM_RIGHTS` is declined too:
    ///   any memfd with a name reserved for the executor's own, such as a
    ///   synthetic `/proc` file or `fdinfo` description, and with captured
    ///   output the supervisor's standard output or error however the guest
    ///   holds it. `EBADF` when the table does not hold `fd`.
    /// - `exe`: the supervisor's handle of the process's image
    ///   (`executable_file`), the file the executor read the image from at
    ///   the process's start or its last exec, so the name follows a rename
    ///   and ends in ` (deleted)` once the file is unlinked, as Linux's
    ///   `exe` link does. Not provided for an image the executor was given
    ///   without a file.
    /// - `cwd`: the supervisor's handle of the working directory
    ///   (`cwd_fd`), walked at the call, as `getcwd` walks it, so a removed
    ///   directory ends in ` (deleted)`, as Linux's `cwd` link does.
    ///
    /// The executor keeps a process's descriptor table, its image and each
    /// thread's working directory in the executors of that process's own
    /// threads, so it answers for the asking thread's process only: the
    /// descriptor table and the image for any live thread of it, and the
    /// working directory for the asking thread alone. Every other live
    /// thread of the run is not provided, and neither is another thread's
    /// working directory:
    ///
    /// - The executor keeps a working directory per thread, where Linux
    ///   shares one between threads created with `CLONE_FS`, so another
    ///   thread's is its own, in its own executor's state.
    /// - Another process's image is in its executors' state too, and its
    ///   descriptor table is reachable only by holding the table through the
    ///   run's process records. A hold that outlived the process's last
    ///   thread would close the process's descriptors on the asking thread
    ///   when it ends, which is why the run's active signal publication does
    ///   not hold one either (`process_signal_publication.rs`).
    ///
    /// `ESRCH` when `tid` names no task that is live in the run's lifecycle
    /// table: no such number, zero and below, or an exited task.
    pub fn link_path(
        &self,
        tid: libc::pid_t,
        link: reverie::PathLink,
    ) -> std::result::Result<reverie::LinkPath, reverie::FactError> {
        match link {
            reverie::PathLink::Descriptor(fd) => {
                // The table every thread of the process installs before its
                // syscalls run, locked until the read is done, before the
                // lifecycle lock, the order an exec takes them in.
                let table = self
                    .file_table
                    .lock()
                    .expect("KVM file-table lock poisoned");
                self.link_owner(tid)?;
                descriptor_link(
                    &table,
                    fd,
                    self.output.as_ref().map(CapturedOutput::metadata),
                )
            }
            reverie::PathLink::Executable => {
                self.link_owner(tid)?;
                let file = self
                    .state
                    .executable_file
                    .as_ref()
                    .ok_or(reverie::FactError::NotProvided)?;
                handle_link(file.as_raw_fd())
            }
            reverie::PathLink::WorkingDirectory => match self.link_owner(tid)? {
                LinkOwner::Caller => handle_link(self.state.cwd_fd.as_raw_fd()),
                LinkOwner::Sibling => Err(reverie::FactError::NotProvided),
            },
            _ => Err(reverie::FactError::NotProvided),
        }
    }

    /// Which thread of this process `tid` is, from the run's lifecycle table:
    /// `ESRCH` for a number that names no live task, and not provided for a
    /// live thread of another process.
    fn link_owner(&self, tid: libc::pid_t) -> std::result::Result<LinkOwner, reverie::FactError> {
        let lifecycle = self
            .state
            .task_lifecycle
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let task = lifecycle
            .get(tid)
            .ok_or(reverie::FactError::Errno(reverie::Errno::ESRCH))?;
        if task.tgid != self.state.pid || task.process_generation != self.process_generation {
            return Err(reverie::FactError::NotProvided);
        }
        Ok(if tid == self.state.tid {
            LinkOwner::Caller
        } else {
            LinkOwner::Sibling
        })
    }
}

impl ElfExecutor {
    /// `reverie::Guest::memory_map` for thread `tid`, a live thread of this
    /// task's process, from the address space `memory` the process's
    /// syscalls change: the mappings the executor can list exactly as Linux
    /// lists them for this address space, which are not all of them, so the
    /// list is never complete.
    ///
    /// The executor keeps the address space as pages, each with what it was
    /// reserved for and whether it is accessible and writable. That is not
    /// enough for a mapping's permissions, which Linux keeps apart (read,
    /// write and execute), nor for its boundaries, which depend on how
    /// earlier calls split and merged mappings and on flags Linux keeps on
    /// them (<https://github.com/rrnewton/reverie/issues/1211>). The one mapping
    /// it can establish is the stack, while the stack window is as the
    /// loader left it: nothing has changed since the image started
    /// (`GuestMemory::unchanged_user_stack_window`), in this process; for a
    /// fork, in its parent before the fork; and in a process that shares
    /// this address space on Linux, a vfork child (`vfork(2)`, or `clone(2)`
    /// and `clone3(2)` with `CLONE_VM|CLONE_VFORK`) and its parent, which run
    /// on two copies of the memory here that share the record, until the
    /// child executes another image. Then the list holds one mapping, the
    /// window's:
    ///
    /// - its range is the window, from its start (the executor maps the whole
    ///   window at load, where Linux maps the stack's arguments and 128 KiB
    ///   below them and grows the stack on demand, so Linux's mapping starts
    ///   higher) to the top of the user address space, where Linux's ends;
    /// - it is readable and writable, and executable where Linux makes the
    ///   stack executable (`setup_arg_pages`): for an image whose last
    ///   `PT_GNU_STACK` header has `PF_X`. Linux also does for an image
    ///   without one started with the `READ_IMPLIES_EXEC` personality, which
    ///   this executor never sets; for a process that has it the entry is
    ///   omitted, since the permission its stack started with cannot be
    ///   established;
    /// - it is private, with no file offset, device or inode;
    /// - it is `[stack]`, the mapping that holds the initial stack pointer
    ///   (`vma_is_initial_stack`); a window that does not hold it is not
    ///   listed.
    ///
    /// Once the window has changed, the list is empty: any change to the
    /// state of a page of it, a page of it reserved for another use, and any
    /// call this executor accepts without changing the pages' states that
    /// changes what Linux keeps on the mapping, that is `mprotect(2)` of part
    /// of it, `madvise(2)` advice other than `MADV_WILLNEED`,
    /// `MADV_DONTNEED`, `MADV_FREE`, `MADV_COLD` and `MADV_PAGEOUT`,
    /// `munlock(2)` of part of it and `munlockall(2)`. Where Linux applies
    /// part of a call and then fails, the part is recorded too: an
    /// `mprotect(2)` range that overlaps the window is recorded before any
    /// refusal, since Linux protects the mappings before a hole and keeps
    /// that when it reports ENOMEM, where this executor refuses the whole
    /// range (a range that Linux refuses before it reaches the stack is
    /// recorded as well, which only omits); madvise records its range before
    /// it reports a hole in the same way. A new image starts it again.
    ///
    /// A call this executor refuses where Linux applies it leaves the entry
    /// listed where Linux shows the stack changed: `mlock(2)` and `mlock2(2)`
    /// (ENOSYS), `mprotect(2)` with `PROT_GROWSDOWN` (EINVAL), and a
    /// `MAP_FIXED` mapping or an `mremap(2)` destination in the window
    /// (ENOMEM and EINVAL). The entry is exact for this executor's address
    /// space, which the refused call left as it was; the refusal, which the
    /// guest sees, is the difference from Linux.
    ///
    /// It answers for the threads of this task's process, as `link_path`
    /// does, since only they share this address space: `ESRCH` when `tid`
    /// names no task that is live in the run's lifecycle table, and not
    /// provided for a live thread of another process, whose address space
    /// this executor does not hold.
    ///
    /// Whether the window is unchanged is read under one acquisition of the
    /// address space's lock, which every change to a page's state and every
    /// recorded change also takes, and a syscall records a change before it
    /// makes it, so the answer is never part of a change; the reader never
    /// waits for another thread's syscall to finish.
    pub fn memory_map(
        &self,
        memory: &GuestMemory,
        tid: libc::pid_t,
    ) -> std::result::Result<reverie::MemoryMap, reverie::FactError> {
        // The same threads as a link of this process.
        self.link_owner(tid)?;
        let mut mappings = Vec::new();
        let start_stack = self.state.stack_pointer;
        if let Some((start, end)) = memory.unchanged_user_stack_window()
            && self.state.linux_personality & READ_IMPLIES_EXEC == 0
            && (start..=end).contains(&start_stack)
        {
            let executable = if self.state.stack_executable {
                libc::PROT_EXEC
            } else {
                0
            };
            mappings.push(reverie::MappingFact::new(
                start,
                end,
                (libc::PROT_READ | libc::PROT_WRITE | executable) as u32,
                false,
                0,
                reverie::MappingBacking::Stack,
            )?);
        }
        reverie::MemoryMap::new(mappings, false)
    }
}

/// Linux's `READ_IMPLIES_EXEC` personality flag (`include/uapi/linux/personality.h`).
const READ_IMPLIES_EXEC: u32 = 0x0040_0000;

/// The name of guest descriptor `fd` of `table`: see
/// [`ElfExecutor::link_path`]. `capture` is the run's captured output, if it
/// captures it.
fn descriptor_link(
    table: &FileTableState,
    fd: libc::c_int,
    capture: Option<CaptureMetadata>,
) -> std::result::Result<reverie::LinkPath, reverie::FactError> {
    let (host, open_standard) = table_host_fd(table, fd)?;
    if let Some(capture) = capture
        && let Some(alias) = output_alias_from_sets(
            fd,
            &table.stdout_alias_fds,
            &table.stderr_alias_fds,
            open_standard,
        )
    {
        // The pipe `fstat` describes (`synthetic_captured_output_stat`).
        let name = format!("pipe:[{}]", capture.identity(alias).inode);
        return reverie::LinkPath::from_readlink(name.as_bytes());
    }
    if table.signalfd_fds.contains(&fd) {
        return reverie::LinkPath::from_readlink(b"anon_inode:[signalfd]");
    }
    if capture.is_some() && supervisor_output(host) {
        return Err(reverie::FactError::NotProvided);
    }
    handle_link(host)
}

/// The name the host kernel's `d_path` gives the supervisor's descriptor
/// `host`, from the supervisor's `readlink` of its own `/proc/self/fd/<host>`
/// into a buffer of `PATH_MAX` bytes, one more than the longest name Linux
/// gives, so a name that fills it is refused. A memfd with a name reserved
/// for the executor's own is not provided, whichever descriptor of it this
/// is. Any error of the read is the host kernel's answer.
fn handle_link(host: RawFd) -> std::result::Result<reverie::LinkPath, reverie::FactError> {
    let mut text = [0u8; libc::PATH_MAX as usize];
    let count = read_fd_link_into(host, &mut text).map_err(|error| {
        reverie::FactError::Errno(reverie::Errno::new(
            i32::try_from(-error).unwrap_or(libc::EIO),
        ))
    })?;
    let text = &text[..count];
    if syncfs_private_memfd_name(text) {
        return Err(reverie::FactError::NotProvided);
    }
    reverie::LinkPath::from_readlink(text)
}

#[cfg(test)]
#[allow(clippy::disallowed_methods, reason = "tests may make threads freely")]
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

    /// `ElfExecutor::link_path`, with the same guest operations.
    mod link_path {
        use std::os::unix::ffi::OsStringExt;

        use reverie::LinkPath;
        use reverie::PathLink;

        use super::*;

        fn path(text: &[u8]) -> std::result::Result<LinkPath, FactError> {
            Ok(LinkPath::Path(text.to_vec()))
        }

        fn anonymous(text: &str) -> std::result::Result<LinkPath, FactError> {
            Ok(LinkPath::Anonymous(text.as_bytes().to_vec()))
        }

        /// The name `d_path` gives `name` in the test's directory.
        fn canonical(directory: &Directory, name: &str) -> Vec<u8> {
            std::fs::canonicalize(directory.0.join(name))
                .unwrap()
                .into_os_string()
                .into_vec()
        }

        /// Guest descriptor `fd`'s link, asked by the executor's own thread.
        fn descriptor(executor: &ElfExecutor, fd: i32) -> std::result::Result<LinkPath, FactError> {
            executor.link_path(executor.state.tid, PathLink::Descriptor(fd))
        }

        /// The inode of the host object behind guest descriptor `fd`.
        fn table_inode(executor: &ElfExecutor, fd: i32) -> u64 {
            let table = executor.file_table.lock().unwrap();
            std::os::unix::fs::MetadataExt::ino(&table.files[&fd].metadata().unwrap())
        }

        /// The inode the guest's `fstat` of `fd` reports.
        fn guest_inode(executor: &mut ElfExecutor, memory: &mut GuestMemory, fd: i32) -> u64 {
            assert_eq!(
                call(
                    executor,
                    memory,
                    libc::SYS_fstat,
                    [fd as u64, VALUE, 0, 0, 0, 0]
                ),
                0
            );
            // `st_ino` follows the eight bytes of `st_dev`.
            let mut inode = [0u8; 8];
            memory.read(VALUE + 8, &mut inode).unwrap();
            u64::from_ne_bytes(inode)
        }

        /// Each kind of descriptor the guest makes is named as Linux names it:
        /// a file, an unlinked file, a pipe and a socket by their host inodes,
        /// an eventfd, a virtual signalfd as a signalfd, a memfd by its name,
        /// `/dev/null`, and uncaptured standard output as the supervisor's own.
        #[test]
        fn each_kind_of_descriptor_is_named_as_linux_names_it() {
            let directory = Directory::new();
            let mut memory = GuestMemory::new(0, PAGE as usize).unwrap();
            let mut executor = ElfExecutor::new(test_state(&directory.0), false);
            let regular = open(
                &mut executor,
                &mut memory,
                b"regular",
                libc::O_RDWR | libc::O_CREAT,
            );
            assert_eq!(
                descriptor(&executor, regular),
                path(&canonical(&directory, "regular"))
            );
            let deleted = open(
                &mut executor,
                &mut memory,
                b"deleted",
                libc::O_RDWR | libc::O_CREAT,
            );
            let deleted_name = canonical(&directory, "deleted");
            std::fs::remove_file(directory.0.join("deleted")).unwrap();
            assert_eq!(
                descriptor(&executor, deleted),
                path(&[deleted_name.as_slice(), b" (deleted)"].concat())
            );
            assert_eq!(
                call(
                    &mut executor,
                    &memory,
                    libc::SYS_pipe2,
                    [PAIR, 0, 0, 0, 0, 0]
                ),
                0
            );
            let mut pair = [0u8; 8];
            memory.read(PAIR, &mut pair).unwrap();
            let reader = i32::from_ne_bytes(pair[..4].try_into().unwrap());
            let pipe = format!("pipe:[{}]", table_inode(&executor, reader));
            assert_eq!(descriptor(&executor, reader), anonymous(&pipe));
            assert_eq!(
                pipe,
                format!("pipe:[{}]", guest_inode(&mut executor, &mut memory, reader))
            );
            let [socket, _] = socket_pair(&mut executor, &memory);
            assert_eq!(
                descriptor(&executor, socket),
                anonymous(&format!("socket:[{}]", table_inode(&executor, socket)))
            );
            let eventfd = new_fd(
                &mut executor,
                &memory,
                libc::SYS_eventfd2,
                [0, 0, 0, 0, 0, 0],
            );
            assert_eq!(
                descriptor(&executor, eventfd),
                anonymous("anon_inode:[eventfd]")
            );
            memory.write(VALUE, &0u64.to_ne_bytes()).unwrap();
            let signalfd = new_fd(
                &mut executor,
                &memory,
                libc::SYS_signalfd4,
                [u64::MAX, VALUE, 8, 0, 0, 0],
            );
            assert_eq!(
                descriptor(&executor, signalfd),
                anonymous("anon_inode:[signalfd]")
            );
            let guest = memfd(&mut executor, &mut memory, b"guest-object");
            assert_eq!(
                descriptor(&executor, guest),
                path(b"/memfd:guest-object (deleted)")
            );
            let null = open(&mut executor, &mut memory, b"/dev/null", libc::O_RDONLY);
            assert_eq!(descriptor(&executor, null), path(b"/dev/null"));
            let own = std::fs::read_link("/proc/self/fd/1")
                .unwrap()
                .into_os_string()
                .into_vec();
            assert_eq!(descriptor(&executor, 1), LinkPath::from_readlink(&own));
        }

        /// With captured output, standard output and error are the pipes
        /// their `fstat` describes, and so is a descriptor the guest made of
        /// one; standard output received through `SCM_RIGHTS`, which nothing
        /// in the receiver's table records as captured output, is the
        /// supervisor's own and is not provided. A standard number the guest
        /// closed is `EBADF`, and one it reused for its own file names it.
        #[test]
        fn captured_output_is_its_own_pipe_and_the_supervisors_is_not_provided() {
            let directory = Directory::new();
            let mut memory = GuestMemory::new(0, PAGE as usize).unwrap();
            let mut executor = ElfExecutor::new(test_state(&directory.0), true);
            let metadata = executor.output.as_ref().unwrap().metadata();
            let stdout = format!("pipe:[{}]", metadata.identity(OutputAlias::Stdout).inode);
            let stderr = format!("pipe:[{}]", metadata.identity(OutputAlias::Stderr).inode);
            assert_ne!(stdout, stderr);
            assert_eq!(descriptor(&executor, 1), anonymous(&stdout));
            assert_eq!(descriptor(&executor, 2), anonymous(&stderr));
            assert_eq!(
                stdout,
                format!("pipe:[{}]", guest_inode(&mut executor, &mut memory, 1))
            );
            let duplicate = new_fd(&mut executor, &memory, libc::SYS_dup, [2, 0, 0, 0, 0, 0]);
            assert_eq!(descriptor(&executor, duplicate), anonymous(&stderr));
            let [sender, receiver] = socket_pair(&mut executor, &memory);
            send_descriptor(&mut executor, &mut memory, sender, 1);
            let received = receive_descriptor(&mut executor, &mut memory, receiver);
            assert_eq!(descriptor(&executor, received), Err(FactError::NotProvided));
            close(&mut executor, &memory, 1);
            assert_eq!(
                descriptor(&executor, 1),
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
            assert_eq!(
                descriptor(&executor, 1),
                path(&canonical(&directory, "output"))
            );
        }

        /// The executor's own objects are not provided however the guest
        /// holds them: a synthetic `/proc` file as opened, as received through
        /// `SCM_RIGHTS` after the sender closed its descriptor, and as a
        /// duplicate of the received one; and a guest memfd given a name the
        /// executor reserves, since a name only denies.
        #[test]
        fn the_executors_own_objects_are_not_provided_however_the_guest_holds_them() {
            let directory = Directory::new();
            let mut memory = GuestMemory::new(0, PAGE as usize).unwrap();
            let mut executor = ElfExecutor::new(test_state(&directory.0), false);
            let [sender, receiver] = socket_pair(&mut executor, &memory);
            for name in [&b"/proc/self/maps"[..], b"/proc/self/status"] {
                let label = String::from_utf8_lossy(name);
                let carrier = open(&mut executor, &mut memory, name, libc::O_RDONLY);
                assert_eq!(
                    descriptor(&executor, carrier),
                    Err(FactError::NotProvided),
                    "{label}"
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
                        descriptor(&executor, fd),
                        Err(FactError::NotProvided),
                        "{label}, fd {fd}"
                    );
                }
            }
            let named = memfd(&mut executor, &mut memory, b"reverie-kvm-proc");
            assert_eq!(descriptor(&executor, named), Err(FactError::NotProvided));
        }

        /// A descriptor the table does not hold is `EBADF`: one the guest
        /// closed, one below zero and one above every open one.
        #[test]
        fn descriptors_the_table_does_not_hold_are_ebadf() {
            let directory = Directory::new();
            let mut memory = GuestMemory::new(0, PAGE as usize).unwrap();
            let mut executor = ElfExecutor::new(test_state(&directory.0), false);
            let closed = open(
                &mut executor,
                &mut memory,
                b"closed",
                libc::O_RDWR | libc::O_CREAT,
            );
            assert!(descriptor(&executor, closed).is_ok());
            close(&mut executor, &memory, closed);
            for fd in [closed, -1, i32::MIN, 4096] {
                assert_eq!(
                    descriptor(&executor, fd),
                    Err(FactError::Errno(reverie::Errno::EBADF)),
                    "fd {fd}"
                );
            }
        }

        /// The image is the file the executor read it from, by the name that
        /// file has at the call, ` (deleted)` once it is unlinked; the working
        /// directory likewise, walked at the call. An image the executor was
        /// given without a file is not provided.
        #[test]
        fn the_image_and_the_working_directory_are_named_at_the_call() {
            let directory = Directory::new();
            let mut memory = GuestMemory::new(0, PAGE as usize).unwrap();
            std::fs::write(directory.0.join("image"), b"an image").unwrap();
            let mut state = test_state(&directory.0);
            state.executable_file = Some(Arc::new(
                std::fs::File::open(directory.0.join("image")).unwrap(),
            ));
            let mut executor = ElfExecutor::new(state, false);
            let tid = executor.state.tid;
            assert_eq!(
                executor.link_path(tid, PathLink::Executable),
                path(&canonical(&directory, "image"))
            );
            std::fs::rename(directory.0.join("image"), directory.0.join("renamed")).unwrap();
            let renamed = canonical(&directory, "renamed");
            assert_eq!(
                executor.link_path(tid, PathLink::Executable),
                path(&renamed)
            );
            std::fs::remove_file(directory.0.join("renamed")).unwrap();
            assert_eq!(
                executor.link_path(tid, PathLink::Executable),
                path(&[renamed.as_slice(), b" (deleted)"].concat())
            );
            assert_eq!(
                executor.link_path(tid, PathLink::WorkingDirectory),
                path(&canonical(&directory, "."))
            );
            std::fs::create_dir(directory.0.join("sub")).unwrap();
            let sub = canonical(&directory, "sub");
            memory.write(PATH, b"sub\0").unwrap();
            assert_eq!(
                call(
                    &mut executor,
                    &memory,
                    libc::SYS_chdir,
                    [PATH, 0, 0, 0, 0, 0]
                ),
                0
            );
            assert_eq!(
                executor.link_path(tid, PathLink::WorkingDirectory),
                path(&sub)
            );
            std::fs::remove_dir(directory.0.join("sub")).unwrap();
            assert_eq!(
                executor.link_path(tid, PathLink::WorkingDirectory),
                path(&[sub.as_slice(), b" (deleted)"].concat())
            );
            let bare = ElfExecutor::new(test_state(&directory.0), false);
            assert_eq!(
                bare.link_path(bare.state.tid, PathLink::Executable),
                Err(FactError::NotProvided)
            );
        }

        /// Another live thread of the process shares the process's
        /// descriptor table and image, and answers with them; its working
        /// directory is its own, and not provided. Once it has exited, every
        /// link is `ESRCH`.
        #[test]
        fn a_sibling_shares_the_descriptors_and_the_image_but_not_the_working_directory() {
            let directory = Directory::new();
            let mut memory = GuestMemory::new(0, PAGE as usize).unwrap();
            std::fs::write(directory.0.join("image"), b"an image").unwrap();
            let mut state = test_state(&directory.0);
            state.executable_file = Some(Arc::new(
                std::fs::File::open(directory.0.join("image")).unwrap(),
            ));
            let mut executor = ElfExecutor::new(state, false);
            let (pid, tid) = (executor.state.pid, executor.state.tid);
            let file = open(
                &mut executor,
                &mut memory,
                b"shared",
                libc::O_RDWR | libc::O_CREAT,
            );
            let generation =
                executor
                    .state
                    .task_lifecycle
                    .lock()
                    .unwrap()
                    .register(tid + 1, pid, pid, true);
            let sibling = |link| executor.link_path(tid + 1, link);
            assert_eq!(
                sibling(PathLink::Descriptor(file)),
                path(&canonical(&directory, "shared"))
            );
            assert_eq!(
                sibling(PathLink::Executable),
                path(&canonical(&directory, "image"))
            );
            assert_eq!(
                sibling(PathLink::WorkingDirectory),
                Err(FactError::NotProvided)
            );
            assert_eq!(
                sibling(PathLink::Descriptor(-1)),
                Err(FactError::Errno(reverie::Errno::EBADF))
            );
            executor
                .state
                .task_lifecycle
                .lock()
                .unwrap()
                .remove(tid + 1, generation);
            for link in [
                PathLink::Descriptor(file),
                PathLink::Executable,
                PathLink::WorkingDirectory,
            ] {
                assert_eq!(
                    executor.link_path(tid + 1, link),
                    Err(FactError::Errno(reverie::Errno::ESRCH)),
                    "{link:?}"
                );
            }
        }

        /// A live thread of another process is not provided, for every link,
        /// even for a descriptor number this process holds; a number that
        /// names no live task is `ESRCH`, zero and below included.
        #[test]
        fn another_processs_threads_are_not_provided_and_dead_numbers_are_esrch() {
            let directory = Directory::new();
            let mut memory = GuestMemory::new(0, PAGE as usize).unwrap();
            let mut executor = ElfExecutor::new(test_state(&directory.0), false);
            let pid = executor.state.pid;
            let file = open(
                &mut executor,
                &mut memory,
                b"held",
                libc::O_RDWR | libc::O_CREAT,
            );
            {
                let mut lifecycle = executor.state.task_lifecycle.lock().unwrap();
                lifecycle.register(pid + 10, pid + 10, pid + 10, true);
                lifecycle.register(pid + 11, pid + 10, pid + 10, true);
            }
            let links = [
                PathLink::Descriptor(file),
                PathLink::Descriptor(-1),
                PathLink::Executable,
                PathLink::WorkingDirectory,
            ];
            for tid in [pid + 10, pid + 11] {
                for link in links {
                    assert_eq!(
                        executor.link_path(tid, link),
                        Err(FactError::NotProvided),
                        "tid {tid} {link:?}"
                    );
                }
            }
            for tid in [0, -1, i32::MIN, pid + 5, i32::MAX] {
                for link in links {
                    assert_eq!(
                        executor.link_path(tid, link),
                        Err(FactError::Errno(reverie::Errno::ESRCH)),
                        "tid {tid} {link:?}"
                    );
                }
            }
        }

        /// A link query, answered or not, releases no lock the supervisor
        /// process holds on the guest's file: it opens and closes nothing of
        /// that file.
        #[test]
        fn a_link_query_leaves_the_supervisors_record_locks_held() {
            let directory = Directory::new();
            let mut memory = GuestMemory::new(0, PAGE as usize).unwrap();
            let mut executor = ElfExecutor::new(test_state(&directory.0), true);
            let guest = open(
                &mut executor,
                &mut memory,
                b"shared",
                libc::O_RDWR | libc::O_CREAT,
            );
            let path_name = directory.0.join("shared");
            let supervisor = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(&path_name)
                .unwrap();
            let lock = record_lock(libc::F_WRLCK, 0, 10);
            // SAFETY: supervisor owns a live descriptor and lock is a valid flock.
            assert_eq!(
                unsafe { libc::fcntl(supervisor.as_raw_fd(), libc::F_SETLK, &lock) },
                0
            );
            assert!(supervisor_lock_held(&path_name, &supervisor), "before");
            assert_eq!(
                descriptor(&executor, guest),
                path(&canonical(&directory, "shared"))
            );
            assert!(supervisor_lock_held(&path_name, &supervisor), "answered");
            let [sender, receiver] = socket_pair(&mut executor, &memory);
            send_descriptor(&mut executor, &mut memory, sender, 1);
            let received = receive_descriptor(&mut executor, &mut memory, receiver);
            assert_eq!(descriptor(&executor, received), Err(FactError::NotProvided));
            assert!(supervisor_lock_held(&path_name, &supervisor), "declined");
            assert_eq!(
                descriptor(&executor, 4000),
                Err(FactError::Errno(reverie::Errno::EBADF))
            );
            assert!(supervisor_lock_held(&path_name, &supervisor), "not open");
        }
    }
}

#[cfg(test)]
#[allow(clippy::disallowed_methods, reason = "tests may make threads freely")]
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

    /// A run-scoped read (`reverie::ProcessSignalControl::signal_sets`) of
    /// another process's thread, asked inside that process's exec window
    /// (the thread's state replaced, the process's not yet rebound) after it
    /// has cloned the process's image, cannot answer there: the exec holds
    /// the process's transaction. Once the exec completes it answers EAGAIN,
    /// never the replaced image's sets. Asked again, it answers the new
    /// image, the handler reset and the mask kept, as the `Guest` fact does.
    /// The exec runs on its own thread, so that a read that took a lock out
    /// of order, and with it the exec, deadlocks into a failure at a 60 s
    /// bound instead of a hang; a read that never answers fails at the 10 s
    /// wait for its answer. Neither failure drops an executor that a stuck
    /// thread's locks would make wait.
    #[test]
    fn a_run_scoped_read_waits_out_an_exec_and_answers_eagain() {
        use crate::executor::process_signal_publication::IMAGE_CLONED_TEST_HOOK;

        let directory = Directory::new();
        let mut memory = GuestMemory::new(0, PAGE).unwrap();
        let parent = ElfExecutor::new(test_state(&directory.0), false);
        let mut child = parent
            .fork_child(parent.state.pid + 30, false, false)
            .unwrap();
        mask(&mut child, &mut memory, libc::SIG_BLOCK, bit(libc::SIGUSR2));
        assert_eq!(act(&mut child, &mut memory, libc::SIGUSR1, 0x1000), 0);
        let task = child.signal_task_identity().unwrap();
        let control = parent.backend_signal_control().process;
        let before = SignalSets::new(0, 0, bit(libc::SIGUSR2), 0, bit(libc::SIGUSR1));
        let after = SignalSets::new(0, 0, bit(libc::SIGUSR2), 0, 0);
        assert_eq!(control.signal_sets(task), Ok(before));
        let (ask, asked) = std::sync::mpsc::channel::<()>();
        let (cloned, image_cloned) = std::sync::mpsc::channel::<()>();
        let (answer, answered) = std::sync::mpsc::channel();
        let reading = std::thread::spawn({
            let control = control.clone();
            move || {
                let _hook = Hook::install(&IMAGE_CLONED_TEST_HOOK, move || {
                    let _ = cloned.send(());
                });
                asked.recv().unwrap();
                answer.send(control.signal_sets(task)).unwrap();
            }
        });
        let (exec_done, exec_result) = std::sync::mpsc::channel();
        let image = directory.0.clone();
        let execing = std::thread::spawn(move || {
            let answered = std::rc::Rc::new(answered);
            let during = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
            let hook = Hook::install(&EXEC_WINDOW_TEST_HOOK, {
                let (answered, during) = (answered.clone(), during.clone());
                move || {
                    ask.send(()).unwrap();
                    image_cloned
                        .recv_timeout(std::time::Duration::from_secs(10))
                        .expect("the read never cloned the process's image");
                    // A read that answers here answers from the replaced image.
                    if let Ok(sets) = answered.recv_timeout(std::time::Duration::from_millis(200)) {
                        during.borrow_mut().push(sets);
                    }
                }
            });
            child.replace_after_exec(test_state(&image));
            drop(hook);
            let late = answered.recv_timeout(std::time::Duration::from_secs(10));
            let during = during.borrow().clone();
            exec_done.send((child, during, late)).unwrap();
        });
        let (child, during, late) =
            match exec_result.recv_timeout(std::time::Duration::from_secs(60)) {
                Ok(result) => result,
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                    // The read may still hold the run's locks, which dropping
                    // the parent's executor would wait for: leak it.
                    std::mem::forget(parent);
                    std::panic::resume_unwind(execing.join().unwrap_err())
                }
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                    // The stuck threads hold the run's locks, which dropping
                    // the parent's executor would wait for: leak it, so that
                    // the test fails here instead of hanging in its unwind.
                    std::mem::forget(parent);
                    panic!("the exec or the read did not finish in 60 s: a lock-order deadlock?")
                }
            };
        execing.join().unwrap();
        if during.is_empty() && late.is_err() {
            // The read has given no answer at all, so it may be stuck, and
            // may hold the run's locks, which joining it or dropping either
            // executor would wait for: leak both, and fail without waiting.
            std::mem::forget(child);
            std::mem::forget(parent);
            panic!("the read gave no answer within 10 s of the exec: a stuck read?");
        }
        reading.join().unwrap();
        assert_eq!(during, []);
        assert_eq!(late, Ok(Err(FactError::Errno(reverie::Errno::EAGAIN))));
        assert_eq!(control.signal_sets(task), Ok(after));
        assert_eq!(parent.signal_sets(task.tid.as_raw()), Ok(after));
        drop(child);
    }
}

#[cfg(test)]
mod memory_map_tests {
    use std::sync::atomic::AtomicU64;
    use std::sync::atomic::Ordering;

    use reverie::FactError;
    use reverie::MappingBacking::Stack;

    use super::*;

    struct Directory(std::path::PathBuf);

    impl Directory {
        fn new() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let path = std::env::temp_dir().join(format!(
                "reverie-linuxemu-memory-map-{}-{}",
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

    /// The address space of `image`, loaded as the backend loads a program,
    /// and an executor of its first thread.
    fn loaded_image(directory: &Directory, image: &[u8]) -> (ElfExecutor, GuestMemory) {
        let mut memory = GuestMemory::new(0, 16 * 1024 * 1024).unwrap();
        let mut state =
            crate::elf::load_static_elf(&mut memory, image, &["maps"], &[], &directory.0).unwrap();
        state.children = ChildWaitContext::test_root();
        (ElfExecutor::new(state, false), memory)
    }

    /// A two-page image at 2 MiB without a `PT_GNU_STACK` header, the stack
    /// window at the top of the user address space.
    fn loaded(directory: &Directory) -> (ElfExecutor, GuestMemory) {
        loaded_image(directory, &crate::elf::minimal_test_elf(&[0xf4]))
    }

    /// The minimal image with a `PT_GNU_STACK` header after its `PT_LOAD`
    /// for each of `flags`, in order.
    fn gnu_stack_image(flags: &[u32]) -> Vec<u8> {
        let mut image = crate::elf::minimal_test_elf(&[0xf4]);
        let headers = 1 + flags.len() as u16;
        image[56..58].copy_from_slice(&headers.to_le_bytes());
        for (index, flags) in flags.iter().enumerate() {
            let header = 64 + 56 * (index + 1);
            image[header..header + 56].fill(0);
            image[header..header + 4].copy_from_slice(&PT_GNU_STACK.to_le_bytes());
            image[header + 4..header + 8].copy_from_slice(&flags.to_le_bytes());
            image[header + 48..header + 56].copy_from_slice(&16u64.to_le_bytes());
        }
        image
    }

    const RW: u32 = (libc::PROT_READ | libc::PROT_WRITE) as u32;
    const RWX: u32 = (libc::PROT_READ | libc::PROT_WRITE | libc::PROT_EXEC) as u32;
    const PT_GNU_STACK: u32 = 0x6474_e551;
    /// The stack window's user range.
    const WINDOW: (u64, u64) = (
        crate::memory::USER_ADDRESS_END - STACK_LIMIT,
        crate::memory::USER_STACK_TOP,
    );
    /// A page of the window well below the initial stack.
    const STACK_PAGE: u64 = WINDOW.0 + 16 * PAGE_SIZE;

    fn stack(prot: u32) -> reverie::MemoryMap {
        reverie::MemoryMap::new(
            vec![reverie::MappingFact::new(WINDOW.0, WINDOW.1, prot, false, 0, Stack).unwrap()],
            false,
        )
        .unwrap()
    }

    fn empty() -> reverie::MemoryMap {
        reverie::MemoryMap::new(Vec::new(), false).unwrap()
    }

    fn change(
        executor: &mut ElfExecutor,
        memory: &GuestMemory,
        number: libc::c_long,
        args: [u64; 3],
    ) -> i64 {
        call(
            executor,
            memory,
            number,
            [args[0], args[1], args[2], 0, 0, 0],
        )
    }

    /// As the image starts, the map lists its stack window, readable and
    /// writable, as `[stack]`: the range and protection of the one line of
    /// the executor's own `/proc/self/maps` today. Nothing else is listed,
    /// and the list is never complete. Growing the break, which leaves the
    /// window alone, changes nothing.
    #[test]
    fn the_map_lists_the_stack_window_as_the_image_starts_with_it() {
        let directory = Directory::new();
        let (mut executor, memory) = loaded(&directory);
        let tid = executor.state.tid;
        assert!(!executor.state.stack_executable);
        assert_eq!(executor.memory_map(&memory, tid), Ok(stack(RW)));
        let text = String::from_utf8(proc_self_maps_content(&executor.state)).unwrap();
        assert_eq!(
            text,
            format!(
                "{:012x}-{:012x} rw-p 00000000 00:00 0 [stack]\n",
                WINDOW.0, WINDOW.1
            )
        );
        let base = executor.state.heap_base;
        assert_eq!(
            change(
                &mut executor,
                &memory,
                libc::SYS_brk,
                [base + 3 * PAGE_SIZE, 0, 0]
            ),
            (base + 3 * PAGE_SIZE) as i64
        );
        assert_eq!(executor.memory_map(&memory, tid), Ok(stack(RW)));
    }

    /// Each change to the window, and each call that changes what Linux
    /// keeps on the stack's mapping without changing the pages' states,
    /// empties the map for good: protecting a page, as anything at all,
    /// read-write and read-write-execute included, which leave its state
    /// as it is, and again after setting it back; unmapping a page; madvise
    /// advice that sets or clears a mapping flag on Linux; munlock of a page;
    /// munlockall.
    #[test]
    fn each_change_to_the_stack_window_empties_the_map() {
        let page = PAGE_SIZE;
        let protect = |prot: i32| (libc::SYS_mprotect, [STACK_PAGE, page, prot as u64]);
        let advise = |advice: i32| (libc::SYS_madvise, [STACK_PAGE, page, advice as u64]);
        let mut changes = vec![
            ("read-only", protect(libc::PROT_READ)),
            ("inaccessible", protect(libc::PROT_NONE)),
            ("read-write", protect(libc::PROT_READ | libc::PROT_WRITE)),
            (
                "read-write-execute",
                protect(libc::PROT_READ | libc::PROT_WRITE | libc::PROT_EXEC),
            ),
            ("write-only", protect(libc::PROT_WRITE)),
            ("unmapped", (libc::SYS_munmap, [STACK_PAGE, page, 0])),
            ("munlock", (libc::SYS_munlock, [STACK_PAGE, page, 0])),
            ("munlockall", (libc::SYS_munlockall, [0, 0, 0])),
        ];
        let flagging = [
            ("MADV_NORMAL", libc::MADV_NORMAL),
            ("MADV_RANDOM", libc::MADV_RANDOM),
            ("MADV_SEQUENTIAL", libc::MADV_SEQUENTIAL),
            ("MADV_MERGEABLE", libc::MADV_MERGEABLE),
            ("MADV_UNMERGEABLE", libc::MADV_UNMERGEABLE),
            ("MADV_HUGEPAGE", libc::MADV_HUGEPAGE),
            ("MADV_NOHUGEPAGE", libc::MADV_NOHUGEPAGE),
        ];
        changes.extend(
            flagging
                .iter()
                .map(|&(name, advice)| (name, advise(advice))),
        );
        // A hint over a range with a hole is reported as ENOMEM, and Linux
        // applies it to the mapped part all the same.
        changes.push((
            "MADV_RANDOM with a hole",
            (
                libc::SYS_madvise,
                [WINDOW.1 - page, 2 * page, libc::MADV_RANDOM as u64],
            ),
        ));
        for (name, (number, args)) in changes {
            let directory = Directory::new();
            let (mut executor, memory) = loaded(&directory);
            let tid = executor.state.tid;
            let result = change(&mut executor, &memory, number, args);
            assert!(
                result == 0 || result == -i64::from(libc::ENOMEM),
                "{name}: {result}"
            );
            assert_eq!(executor.memory_map(&memory, tid), Ok(empty()), "{name}");
            // Setting the page back as the image started with it does not
            // bring the line back.
            let _ = change(
                &mut executor,
                &memory,
                libc::SYS_mprotect,
                [
                    STACK_PAGE,
                    page,
                    (libc::PROT_READ | libc::PROT_WRITE) as u64,
                ],
            );
            assert_eq!(executor.memory_map(&memory, tid), Ok(empty()), "{name}");
        }
    }

    /// Calls that change nothing Linux keeps on the stack's mapping leave
    /// it listed: madvise advice that never sets or clears a mapping flag;
    /// madvise this executor refuses, or that Linux refuses; madvise and
    /// munlock over no page; a munlock that starts at no mapping, which Linux
    /// refuses first; mprotect and munlock of the heap. So do the calls this
    /// executor refuses before anything changes, where Linux applies them:
    /// mlock and mlock2 (ENOSYS), mprotect with `PROT_GROWSDOWN` (EINVAL),
    /// a fixed mapping in the window (ENOMEM) and an mremap there (EINVAL).
    #[test]
    fn calls_that_keep_the_stacks_mapping_leave_it_listed() {
        let directory = Directory::new();
        let (mut executor, memory) = loaded(&directory);
        let tid = executor.state.tid;
        let page = PAGE_SIZE;
        for advice in mm::MADVISE_KEEPS_MAPPING_FLAGS {
            assert_eq!(
                change(
                    &mut executor,
                    &memory,
                    libc::SYS_madvise,
                    [STACK_PAGE, page, advice as u64]
                ),
                0,
                "advice {advice}"
            );
        }
        let refused: [(&str, libc::c_long, [u64; 3], i32); 9] = [
            (
                "MADV_DONTFORK",
                libc::SYS_madvise,
                [STACK_PAGE, page, libc::MADV_DONTFORK as u64],
                libc::ENOSYS,
            ),
            (
                "unaligned advice",
                libc::SYS_madvise,
                [STACK_PAGE + 1, page, libc::MADV_RANDOM as u64],
                libc::EINVAL,
            ),
            (
                "unknown advice",
                libc::SYS_madvise,
                [STACK_PAGE, page, 77],
                libc::EINVAL,
            ),
            (
                "mlock",
                libc::SYS_mlock,
                [STACK_PAGE, page, 0],
                libc::ENOSYS,
            ),
            // Linux refuses a munlock that starts at no mapping before it
            // changes anything.
            (
                "munlock from below the window",
                libc::SYS_munlock,
                [WINDOW.0 - 8, 16, 0],
                libc::ENOMEM,
            ),
            ("mlockall", libc::SYS_mlockall, [1, 0, 0], libc::ENOSYS),
            (
                "mlock2",
                libc::SYS_mlock2,
                [STACK_PAGE, page, 0],
                libc::ENOSYS,
            ),
            (
                "mprotect with PROT_GROWSDOWN",
                libc::SYS_mprotect,
                [
                    STACK_PAGE,
                    page,
                    (libc::PROT_READ | libc::PROT_WRITE | libc::PROT_GROWSDOWN) as u64,
                ],
                libc::EINVAL,
            ),
            (
                "mprotect with an unknown bit",
                libc::SYS_mprotect,
                [STACK_PAGE, page, 0x10],
                libc::EINVAL,
            ),
        ];
        for (name, number, args, errno) in refused {
            assert_eq!(
                change(&mut executor, &memory, number, args),
                -i64::from(errno),
                "{name}"
            );
        }
        // Calls over no page change nothing.
        assert_eq!(
            change(
                &mut executor,
                &memory,
                libc::SYS_madvise,
                [STACK_PAGE, 0, libc::MADV_RANDOM as u64]
            ),
            0
        );
        assert_eq!(
            change(
                &mut executor,
                &memory,
                libc::SYS_munlock,
                [STACK_PAGE, 0, 0]
            ),
            0
        );
        let base = executor.state.heap_base;
        assert_eq!(
            change(&mut executor, &memory, libc::SYS_brk, [base + page, 0, 0]),
            (base + page) as i64
        );
        assert_eq!(
            change(
                &mut executor,
                &memory,
                libc::SYS_mprotect,
                [base, page, libc::PROT_READ as u64]
            ),
            0
        );
        assert_eq!(
            change(&mut executor, &memory, libc::SYS_munlock, [base, page, 0]),
            0
        );
        let fixed = (libc::MAP_FIXED | libc::MAP_PRIVATE | libc::MAP_ANONYMOUS) as u64;
        assert_eq!(
            call(
                &mut executor,
                &memory,
                libc::SYS_mmap,
                [STACK_PAGE, page, RW as u64, fixed, u64::MAX, 0]
            ),
            -i64::from(libc::ENOMEM)
        );
        let private = (libc::MAP_PRIVATE | libc::MAP_ANONYMOUS) as u64;
        let source = call(
            &mut executor,
            &memory,
            libc::SYS_mmap,
            [0, page, RW as u64, private, u64::MAX, 0],
        );
        assert!(source > 0, "{source}");
        let moving = (libc::MREMAP_MAYMOVE | libc::MREMAP_FIXED) as u64;
        assert_eq!(
            call(
                &mut executor,
                &memory,
                libc::SYS_mremap,
                [source as u64, page, page, moving, STACK_PAGE, 0]
            ),
            -i64::from(libc::EINVAL)
        );
        assert_eq!(executor.memory_map(&memory, tid), Ok(stack(RW)));
    }

    /// The stack is executable for an image whose last `PT_GNU_STACK` header
    /// has `PF_X`, as Linux maps it, and not otherwise. A process with the
    /// `READ_IMPLIES_EXEC` personality, and a window that does not hold the
    /// initial stack pointer, have the entry omitted.
    #[test]
    fn the_stack_is_executable_as_linux_maps_it() {
        const PF_RW: u32 = 6;
        const PF_RWX: u32 = 7;
        for (flags, prot) in [
            (&[][..], RW),
            (&[PF_RW][..], RW),
            (&[PF_RWX][..], RWX),
            (&[PF_RW, PF_RWX][..], RWX),
            (&[PF_RWX, PF_RW][..], RW),
        ] {
            let directory = Directory::new();
            let (executor, memory) = loaded_image(&directory, &gnu_stack_image(flags));
            assert_eq!(executor.state.stack_executable, prot == RWX, "{flags:?}");
            let tid = executor.state.tid;
            assert_eq!(
                executor.memory_map(&memory, tid),
                Ok(stack(prot)),
                "{flags:?}"
            );
        }
        let directory = Directory::new();
        let (mut executor, memory) = loaded(&directory);
        let tid = executor.state.tid;
        executor.state.linux_personality |= READ_IMPLIES_EXEC;
        assert_eq!(executor.memory_map(&memory, tid), Ok(empty()));
        executor.state.linux_personality &= !READ_IMPLIES_EXEC;
        assert_eq!(executor.memory_map(&memory, tid), Ok(stack(RW)));
        let start_stack = executor.state.stack_pointer;
        executor.state.stack_pointer = WINDOW.0 - PAGE_SIZE;
        assert_eq!(executor.memory_map(&memory, tid), Ok(empty()));
        executor.state.stack_pointer = start_stack;
        assert_eq!(executor.memory_map(&memory, tid), Ok(stack(RW)));
    }

    /// A forked child's address space is a copy of its parent's, as the
    /// backend makes it: it lists the stack while the parent's window was
    /// unchanged at the fork, whatever either changes afterwards, and not
    /// once the parent's had changed. Its threads share its answer.
    #[test]
    fn a_fork_inherits_the_stack_window_as_it_was() {
        let directory = Directory::new();
        let (mut executor, memory) = loaded(&directory);
        let tid = executor.state.tid;
        let child = executor
            .fork_child(executor.state.pid + 40, false, false)
            .unwrap();
        let child_memory = memory.snapshot().unwrap();
        let child_tid = child.state.tid;
        assert_eq!(child.memory_map(&child_memory, child_tid), Ok(stack(RW)));
        assert_eq!(
            change(
                &mut executor,
                &memory,
                libc::SYS_mprotect,
                [STACK_PAGE, PAGE_SIZE, libc::PROT_READ as u64]
            ),
            0
        );
        assert_eq!(executor.memory_map(&memory, tid), Ok(empty()));
        assert_eq!(child.memory_map(&child_memory, child_tid), Ok(stack(RW)));
        let grandchild = executor
            .fork_child(executor.state.pid + 41, false, false)
            .unwrap();
        let grandchild_memory = memory.snapshot().unwrap();
        assert_eq!(
            grandchild.memory_map(&grandchild_memory, grandchild.state.tid),
            Ok(empty())
        );
        let sibling = child.thread_child(child_tid + 1).unwrap();
        assert_eq!(
            sibling.memory_map(&child_memory, child_tid + 1),
            Ok(stack(RW))
        );
    }

    /// Linux applies mprotect(2) one mapping at a time and keeps what it
    /// applied when it then finds a hole: protecting the stack's top page
    /// and the page past the top of the user address space changes the top
    /// page, splitting the stack, and fails with ENOMEM. The executor refuses
    /// the whole range, after it records the part in the window, so the map
    /// is empty. It records a range that starts below the window as well,
    /// where Linux finds the hole first and changes nothing: the stack is then
    /// left out where Linux still shows it, which only omits.
    #[test]
    fn a_partly_applied_mprotect_empties_the_map() {
        let page = PAGE_SIZE;
        for (name, start) in [
            ("over the top", WINDOW.1 - page),
            ("from below the window", WINDOW.0 - page),
        ] {
            for prot in [
                libc::PROT_READ,
                libc::PROT_READ | libc::PROT_WRITE | libc::PROT_EXEC,
            ] {
                let directory = Directory::new();
                let (mut executor, memory) = loaded(&directory);
                let tid = executor.state.tid;
                assert_eq!(
                    change(
                        &mut executor,
                        &memory,
                        libc::SYS_mprotect,
                        [start, 2 * page, prot as u64]
                    ),
                    -i64::from(libc::ENOMEM),
                    "{name}, {prot}"
                );
                assert_eq!(
                    executor.memory_map(&memory, tid),
                    Ok(empty()),
                    "{name}, {prot}"
                );
            }
        }
    }

    /// A vfork child shares its parent's address space on Linux. The backend
    /// gives it a copy of the memory that shares the stack window's record
    /// with the parent's (`GuestMemory::share_user_stack_window_watch`), so
    /// a change the child makes to the window empties the parent's map, and
    /// one the parent's other threads make while the child runs empties the
    /// child's.
    #[test]
    fn a_vfork_child_and_its_parent_share_the_stack_windows_record() {
        let protect = [STACK_PAGE, PAGE_SIZE, libc::PROT_READ as u64];
        for child_changes in [true, false] {
            let directory = Directory::new();
            let (mut executor, memory) = loaded(&directory);
            let tid = executor.state.tid;
            let mut child = executor
                .fork_child(executor.state.pid + 40, false, true)
                .unwrap();
            let child_memory = memory.snapshot().unwrap();
            child_memory.share_user_stack_window_watch(&memory);
            let child_tid = child.state.tid;
            assert_eq!(child.memory_map(&child_memory, child_tid), Ok(stack(RW)));
            let result = if child_changes {
                change(&mut child, &child_memory, libc::SYS_mprotect, protect)
            } else {
                change(&mut executor, &memory, libc::SYS_mprotect, protect)
            };
            assert_eq!(result, 0, "child changes: {child_changes}");
            assert_eq!(
                executor.memory_map(&memory, tid),
                Ok(empty()),
                "child changes: {child_changes}"
            );
            assert_eq!(
                child.memory_map(&child_memory, child_tid),
                Ok(empty()),
                "child changes: {child_changes}"
            );
        }
    }

    /// A vfork child that executes another image, as posix_spawn's does,
    /// leaves its parent's map as it was: the new image is a new address
    /// space, which lists its own stack, and from then on neither's change
    /// reaches the other.
    #[test]
    fn a_vfork_child_that_executes_leaves_its_parents_stack_listed() {
        let protect = [STACK_PAGE, PAGE_SIZE, libc::PROT_READ as u64];
        for image_changes in [true, false] {
            let label = format!("image changes: {image_changes}");
            let directory = Directory::new();
            let (mut executor, memory) = loaded(&directory);
            let tid = executor.state.tid;
            let mut child_memory = memory.snapshot().unwrap();
            child_memory.share_user_stack_window_watch(&memory);
            let mut state = crate::elf::load_static_elf(
                &mut child_memory,
                &crate::elf::minimal_test_elf(&[0xf4]),
                &["spawned"],
                &[],
                &directory.0,
            )
            .unwrap();
            state.children = ChildWaitContext::test_root();
            let mut image = ElfExecutor::new(state, false);
            let image_tid = image.state.tid;
            assert_eq!(executor.memory_map(&memory, tid), Ok(stack(RW)), "{label}");
            assert_eq!(
                image.memory_map(&child_memory, image_tid),
                Ok(stack(RW)),
                "{label}"
            );
            let (changed, unchanged) = if image_changes {
                let result = change(&mut image, &child_memory, libc::SYS_mprotect, protect);
                assert_eq!(result, 0, "{label}");
                (
                    image.memory_map(&child_memory, image_tid),
                    executor.memory_map(&memory, tid),
                )
            } else {
                let result = change(&mut executor, &memory, libc::SYS_mprotect, protect);
                assert_eq!(result, 0, "{label}");
                (
                    executor.memory_map(&memory, tid),
                    image.memory_map(&child_memory, image_tid),
                )
            };
            assert_eq!(changed, Ok(empty()), "{label}");
            assert_eq!(unchanged, Ok(stack(RW)), "{label}");
        }
    }

    /// The threads of a process share its address space, so a sibling's map
    /// is the same, asked by either thread, and a change either makes shows
    /// to both. Another process's address space is not this executor's: not
    /// provided. A number with no live task is ESRCH.
    #[test]
    fn the_map_answers_for_the_threads_of_this_process() {
        let directory = Directory::new();
        let (executor, memory) = loaded(&directory);
        let tid = executor.state.tid;
        let own = executor.memory_map(&memory, tid).unwrap();
        assert_eq!(own, stack(RW));
        let mut sibling = executor.thread_child(tid + 1).unwrap();
        assert_eq!(executor.memory_map(&memory, tid + 1), Ok(own.clone()));
        assert_eq!(sibling.memory_map(&memory, tid + 1), Ok(own.clone()));
        assert_eq!(sibling.memory_map(&memory, tid), Ok(own.clone()));
        let child = executor
            .fork_child(executor.state.pid + 40, false, false)
            .unwrap();
        let child_tid = child.state.tid;
        assert_eq!(
            executor.memory_map(&memory, child_tid),
            Err(FactError::NotProvided)
        );
        assert_eq!(child.memory_map(&memory, tid), Err(FactError::NotProvided));
        for number in [0, -1, i32::MIN, tid + 5, i32::MAX] {
            assert_eq!(
                executor.memory_map(&memory, number),
                Err(FactError::Errno(reverie::Errno::ESRCH)),
                "tid {number}"
            );
        }
        assert_eq!(
            change(
                &mut sibling,
                &memory,
                libc::SYS_madvise,
                [STACK_PAGE, PAGE_SIZE, libc::MADV_HUGEPAGE as u64]
            ),
            0
        );
        assert_eq!(executor.memory_map(&memory, tid), Ok(empty()));
        assert_eq!(sibling.memory_map(&memory, tid + 1), Ok(empty()));
    }
}
