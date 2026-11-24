/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! Pipe-owner configuration without asynchronous signal delivery.
//!
//! A host PID must never stand in for a guest PID. Each pipe end has a shared
//! record, and only its creator process incarnation may configure/query it.
//! Fork retains the record (including permanent restrictions), but gives the
//! child a different process token. Threads and exec retain their token.
//!
//! Lock order is file table, then this description's leaf mutex. Never call
//! guest-memory APIs or callbacks while holding the leaf mutex. It serializes
//! owner admission, host F_SETFL, and pre-send escape marking across *all*
//! aliases, including aliases in different forked descriptor tables.
//!
//! The same record, with its leaf mutex and its irreversible export mark,
//! also stands for every other description this run created, and for the
//! standard input its embedder declared (`Provenance`): the descriptions
//! whose host file no process outside the run can own, which is what lets
//! asynchronous signal mode be forwarded to them (`async_signal_mode`). Only
//! a pipe's record configures the owner commands. Only `mark_exported` holds
//! more than one leaf mutex at a time, each once, in address order.

use super::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    Fresh,
    Managed,
    Escaped,
}

/// How the description a record stands for came to be in the run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Provenance {
    /// A pipe end this run created with pipe or pipe2, whose owner commands
    /// this record configures.
    Pipe,
    /// Another description this run created by opening or making a file,
    /// whose host type (`fstat`) decides what it is.
    Created,
    /// An anonymous-inode description this run created (an eventfd, a
    /// signalfd's carrier, a timerfd, an inotify instance or an epoll set),
    /// which has no file type of its own: this record is what says what it
    /// is.
    AnonymousKeeper,
    /// The standard input its embedder declared it made for the run
    /// (`LoadedStaticElf::declare_owned_stdin`).
    Declared,
}

/// Why a record no longer admits asynchronous signal mode.
pub(super) const ASYNC_EXPORTED_REFUSAL: &str = "KVM does not turn on asynchronous signal \
     mode for a description that left the run in an SCM_RIGHTS message: a process outside \
     the run may own it";
/// Why a pipe whose owner is recorded here does not take asynchronous signal mode.
pub(super) const ASYNC_OWNER_RECORDED_REFUSAL: &str = "KVM does not turn on asynchronous \
     signal mode for a pipe with an owner recorded for it: it cannot deliver the signal";
/// Why a description whose record is unusable does not take it.
const ASYNC_RECORD_POISONED_REFUSAL: &str = "KVM does not turn on asynchronous signal mode \
     for a description whose record a failed call left unusable";

/// Why a send of rights stops at their records (`mark_exported`).
pub(super) enum ExportRefusal {
    /// Linux's own answer, or this backend's refusal by errno alone.
    Errno(i64),
    /// A description that still takes asynchronous signal mode in the run
    /// has it on (`ASYNC_EXPORT_REFUSAL`).
    AsyncSignalMode(&'static str),
}

/// Why a send of a description with asynchronous signal mode on is refused.
pub(super) const ASYNC_EXPORT_REFUSAL: &str = "KVM does not send, in an SCM_RIGHTS message, \
     a description on which it turned on asynchronous signal mode: the receiver could give \
     it an owner outside the run";

#[derive(Debug)]
struct Configuration {
    phase: Phase,
    // Linux initially reports F_OWNER_TID/0, including after F_SETSIG alone.
    // F_SETOWN(0), unlike that initial state, reports F_OWNER_PID/0.
    owner_type: i32,
    owner_pid: i32,
}

#[derive(Debug)]
pub(crate) struct PipeOwner {
    creator: Arc<()>,
    configuration: Mutex<Configuration>,
    /// Shared by both ends of one pipe: set once either end is exported.
    /// An exported end can be given a host fasync owner, so a successful
    /// write can then raise a configured signal (SIGPIPE included).
    pair_exported: Arc<AtomicBool>,
    provenance: Provenance,
}

impl PipeOwner {
    #[cfg(test)]
    pub(super) fn configuration_available_for_test(&self) -> bool {
        self.configuration.try_lock().is_ok()
    }

    /// The record of one end of a pipe this run created, or, for another
    /// `provenance`, of a description of that kind (`pair_exported` is then
    /// that description's own).
    pub(super) fn new(
        creator: Arc<()>,
        pair_exported: Arc<AtomicBool>,
        provenance: Provenance,
    ) -> Self {
        Self {
            creator,
            pair_exported,
            provenance,
            configuration: Mutex::new(Configuration {
                phase: Phase::Fresh,
                owner_type: 0, // F_OWNER_TID
                owner_pid: 0,
            }),
        }
    }

    /// The record of a standard input its embedder declared
    /// (`LoadedStaticElf::declare_owned_stdin`).
    pub(crate) fn declared(creator: Arc<()>) -> Self {
        Self::new(creator, Arc::default(), Provenance::Declared)
    }

    pub(super) fn provenance(&self) -> Provenance {
        self.provenance
    }

    pub(super) fn is_pipe(&self) -> bool {
        self.provenance == Provenance::Pipe
    }

    /// Whether [`mark_exported`] would refuse this pipe's export now, which
    /// a sender checks before it marks any right as exported.
    pub(super) fn export_refused(&self) -> bool {
        self.configuration
            .lock()
            .map_or(true, |configuration| configuration.phase == Phase::Managed)
    }

    /// Whether this description still takes asynchronous signal mode and
    /// has it on, as `host_fd`, which names it, shows: then it may not be
    /// sent (`ASYNC_EXPORT_REFUSAL`). The early check of a send; the
    /// authoritative one is [`mark_exported`]'s, under the lock.
    pub(super) fn async_export_refused(&self, host_fd: RawFd) -> bool {
        self.configuration.lock().map_or(true, |configuration| {
            configuration.phase != Phase::Escaped && host_async_on(host_fd)
        })
    }

    /// Runs `apply`, a status-flag change that may turn asynchronous signal
    /// mode on, under the leaf mutex, while this description is still the
    /// run's own: never exported and, for a pipe, with no owner recorded.
    /// Otherwise runs nothing and returns why.
    pub(super) fn with_async_on(&self, apply: impl FnOnce() -> i64) -> Result<i64, &'static str> {
        let configuration = self
            .configuration
            .lock()
            .map_err(|_| ASYNC_RECORD_POISONED_REFUSAL)?;
        match configuration.phase {
            Phase::Fresh => Ok(apply()),
            Phase::Managed => Err(ASYNC_OWNER_RECORDED_REFUSAL),
            Phase::Escaped => Err(ASYNC_EXPORTED_REFUSAL),
        }
    }

    /// Runs `apply`, any other change of this description's status flags,
    /// under the leaf mutex, so it serializes with admission and export.
    pub(super) fn locked(&self, apply: impl FnOnce() -> i64) -> i64 {
        match self.configuration.lock() {
            Ok(_configuration) => apply(),
            Err(_) => negative_errno(libc::ENOSYS),
        }
    }

    /// Marks the pipe as no longer private to the guest without an export
    /// record: a reopened /proc/self/fd description of it carries no owner
    /// record, so its own export could not be seen.
    pub(super) fn forfeit_privacy(&self) {
        self.pair_exported.store(true, Ordering::SeqCst);
    }

    /// Whether neither end of this pipe has been exported. Until one is, no
    /// host fasync owner can exist for it (F_SETOWN stays in this record), so
    /// a write that completes raises no signal.
    pub(super) fn pair_never_exported(&self) -> bool {
        !self.pair_exported.load(Ordering::SeqCst)
    }

    pub(super) fn set_nonblocking(&self, host_fd: RawFd, value: libc::c_int) -> i64 {
        let Ok(_configuration) = self.configuration.lock() else {
            return negative_errno(libc::ENOSYS);
        };
        // Serialize with owner admission, F_SETFL and export across all aliases.
        // Guest input was copied before this leaf lock. FIONBIO cannot enable
        // fasync or overwrite unrelated flags with a stale F_GETFL snapshot.
        set_fd_nonblocking(host_fd, value)
    }
}

/// Whether the host description `host_fd` names has asynchronous signal
/// mode on (`FASYNC`, which `F_GETFL` reports as `O_ASYNC`). A description
/// whose flags cannot be read counts as on, so a check fails closed.
pub(super) fn host_async_on(host_fd: RawFd) -> bool {
    fd_status_flags(host_fd).map_or(true, |flags| flags & libc::O_ASYNC != 0)
}

/// The record of guest descriptor `fd`: its table entry's, or, for
/// descriptor 0 while it is still the standard input the embedder declared,
/// that declaration's. Aliases made from descriptor 0 copy this record into
/// their own entries, so it outlives a later replacement of descriptor 0.
pub(super) fn description_record(state: &LoadedStaticElf, fd: i32) -> Option<Arc<PipeOwner>> {
    if let Some(record) = state.pipe_owners.get(&fd) {
        return Some(record.clone());
    }
    let (entry, record) = state.declared_stdin.as_ref()?;
    (fd == libc::STDIN_FILENO
        && state.stdin_copy().is_some()
        && Arc::ptr_eq(entry, &state.stdin_entry_id))
    .then(|| record.clone())
}

/// Marks the records of the rights a send carries as exported, before the
/// host send, or refuses the send and marks none. Each right comes with a
/// host descriptor that names its description and whether the description
/// is of a kind that admits asynchronous signal mode. Each distinct record
/// is locked once, all in address order, so a message naming one
/// description twice, or two of its aliases, cannot wait on its own lock.
/// Under the locks, a pipe with an owner recorded refuses as before, and an
/// admitted description that is still the run's own refuses if it has the
/// mode on: the re-check that the early scan (`async_export_refused`)
/// cannot make authoritative, since another thread may turn the mode on
/// after it. A description outside that domain is not checked: the guest
/// never turns the mode on there, and its sends are Linux's. The marks are
/// irreversible, even if the host send then fails.
pub(super) fn mark_exported(
    rights: &[(Arc<PipeOwner>, RawFd, bool)],
) -> Result<(), ExportRefusal> {
    let mut distinct: Vec<&(Arc<PipeOwner>, RawFd, bool)> = rights.iter().collect();
    distinct.sort_by_key(|(record, _, _)| Arc::as_ptr(record) as usize);
    distinct.dedup_by(|a, b| Arc::ptr_eq(&a.0, &b.0));
    let mut guards = Vec::with_capacity(distinct.len());
    for (record, _, _) in &distinct {
        guards.push(
            record
                .configuration
                .lock()
                .map_err(|_| ExportRefusal::Errno(negative_errno(libc::ENOSYS)))?,
        );
    }
    for (configuration, (_, host_fd, admitted)) in guards.iter().zip(&distinct) {
        if configuration.phase == Phase::Managed {
            return Err(ExportRefusal::Errno(negative_errno(libc::ENOSYS)));
        }
        if *admitted && configuration.phase != Phase::Escaped && host_async_on(*host_fd) {
            return Err(ExportRefusal::AsyncSignalMode(ASYNC_EXPORT_REFUSAL));
        }
    }
    // The record is shared by every known alias; imported aliases never
    // acquire a new positive witness by inode or host-fd resemblance.
    for (configuration, (record, _, _)) in guards.iter_mut().zip(&distinct) {
        configuration.phase = Phase::Escaped;
        record.pair_exported.store(true, Ordering::SeqCst);
    }
    Ok(())
}

fn owner_pointer_valid(address: u64) -> bool {
    address
        .checked_add(8)
        .is_some_and(|end| end <= X86_64_GUEST_USER_LIMIT)
}

/// Implements only known private-pipe configuration. Socket SIGURG, foreign
/// owner liveness, asynchronous delivery and descriptor export stay unsupported.
/// Linux source: fs/fcntl.c f_setown/f_{get,set}own_ex/f_owner_sig at
/// https://github.com/torvalds/linux/blob/7d0a66e4bb9081d75c82ec4957c50034cb0ea449/fs/fcntl.c
// AUTONOMOUS-BOT-IMPLEMENTED
// TODO-HUMAN-REVIEW(PR-910): Review creator-incarnation and permanent shared pipe guards.
pub(super) fn fcntl(
    memory: &GuestMemory,
    state: &LoadedStaticElf,
    guest_fd: i32,
    host_fd: RawFd,
    command: i32,
    argument: u64,
) -> i64 {
    // Descriptor existence is checked by the caller. Linux rejects O_PATH
    // before copying any owner structure, including on unsupported descriptors.
    let flags = match fd_status_flags(host_fd) {
        Ok(flags) => flags,
        Err(error) => return error,
    };
    if flags & libc::O_PATH != 0 {
        return negative_errno(libc::EBADF);
    }
    let requested_owner = match command {
        libc::F_SETOWN => {
            let pid = argument as i32;
            if pid == i32::MIN {
                return negative_errno(libc::EINVAL);
            }
            Some((1, pid))
        }
        LINUX_F_SETOWN_EX => {
            let mut bytes = [0; 8];
            if !owner_pointer_valid(argument) || memory.user().read(argument, &mut bytes).is_err() {
                return negative_errno(libc::EFAULT);
            }
            let kind = i32::from_ne_bytes(bytes[..4].try_into().expect("owner type"));
            let pid = i32::from_ne_bytes(bytes[4..].try_into().expect("owner pid"));
            if !(0..=2).contains(&kind) {
                return negative_errno(libc::EINVAL);
            }
            if pid < 0 {
                // EX never uses a negative PID as process-group shorthand.
                // Linux find_vpid cannot resolve a negative namespace number.
                return negative_errno(libc::ESRCH);
            }
            Some((kind, pid))
        }
        _ => None,
    };
    let Some(owner) = state.pipe_owners.get(&guest_fd).filter(|owner| owner.is_pipe()) else {
        return negative_errno(libc::ENOSYS);
    };
    if !Arc::ptr_eq(&owner.creator, &state.pipe_owner_process)
        || state.poll_table_id.unsafe_history.load(Ordering::SeqCst)
    {
        return negative_errno(libc::ENOSYS);
    }
    let Ok(mut configuration) = owner.configuration.lock() else {
        return negative_errno(libc::ENOSYS);
    };
    if configuration.phase == Phase::Escaped {
        return negative_errno(libc::ENOSYS);
    }
    // Re-read flags under the shared lock; another process's F_SETFL can
    // race the earlier O_PATH check, but cannot race this admission decision.
    match fd_status_flags(host_fd) {
        Ok(flags) if flags & libc::O_ASYNC == 0 => {}
        Ok(_) => return negative_errno(libc::ENOSYS),
        Err(error) => return error,
    }
    if let Some((kind, pid)) = requested_owner {
        if kind != 1 || (pid != 0 && pid != state.pid) {
            // No host namespace lookup, invented ESRCH, or process-group
            // success. Cross-incarnation queries also refuse above; a cached
            // PID must not pretend to model zombies, reaping or PID reuse.
            return negative_errno(libc::ENOSYS);
        }
        configuration.phase = Phase::Managed;
        configuration.owner_type = kind;
        configuration.owner_pid = pid;
        return 0;
    }
    match command {
        libc::F_GETOWN => i64::from(configuration.owner_pid),
        LINUX_F_GETOWN_EX => {
            let mut bytes = [0; 8];
            bytes[..4].copy_from_slice(&configuration.owner_type.to_ne_bytes());
            bytes[4..].copy_from_slice(&configuration.owner_pid.to_ne_bytes());
            drop(configuration);
            if !owner_pointer_valid(argument)
                || memory.user().copy_to_user(argument, &bytes).is_err()
            {
                negative_errno(libc::EFAULT)
            } else {
                0
            }
        }
        LINUX_F_SETSIG | LINUX_F_GETSIG => {
            // Linux v6.1 validates the whole unsigned-long SETSIG argument;
            // v6.18 first narrows to int. Retain the exact host ABI, with no
            // classifier/accept-set. This changes only the owned pipe's signal
            // number: host owner remains NULL and no fasync registration is
            // permitted after successful configuration. GETSIG ignores arg.
            // https://github.com/torvalds/linux/blob/830b3c68c1fb1e9176028d02ef86f3cf76aa2476/fs/fcntl.c#L391
            // SAFETY: fd is held by the caller; both commands take no pointer.
            let result = unsafe { libc::syscall(libc::SYS_fcntl, host_fd, command, argument) };
            if result < 0 {
                io_error(std::io::Error::last_os_error())
            } else {
                if command == LINUX_F_SETSIG && result == 0 {
                    configuration.phase = Phase::Managed;
                }
                result
            }
        }
        _ => unreachable!("only pipe-owner commands reach this helper"),
    }
}
