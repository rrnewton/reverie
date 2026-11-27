/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! pidfds (gVisor: pkg/sentry/kernel/pidfd.go).

use super::*;

/// A pidfd for a guest process: the process it names, and its shadow socket
/// (`process_signal_publication::pidfd_shadow`), which shows the pidfd's
/// readiness. In the guest's table the descriptor itself is a pidfd for the
/// supervisor, which the kernel answers for as for any pidfd.
///
/// The record is the pidfd's open file description: every alias of the
/// descriptor, from dup or fork, shares it. The descriptor tables that hold
/// the pidfd hold the record (`FdSlot`), and a task only names it
/// (`LoadedStaticElf::guest_pidfds`), so the record goes with the pidfd's
/// last descriptor, as Linux releases the description, and so do its shadow
/// and every epoll registration of the pidfd, whatever any other thread of
/// the process is doing. A call that finds the descriptor holds the record
/// until the call ends, even if another thread closes the descriptor
/// meanwhile ([`guest_pidfd`]), as Linux's `fdget` holds the file.
#[derive(Debug)]
pub(crate) struct GuestPidfd {
    target: reverie::SignalProcessId,
    shadow: Arc<std::fs::File>,
    /// A host descriptor of the shadow for each guest descriptor number by
    /// which an epoll registration names the pidfd ([`Self::epoll_control`]).
    epoll_keys: Mutex<std::collections::BTreeMap<libc::c_int, EpollKey>>,
}

/// The host descriptor that stands for one guest descriptor number of a
/// pidfd in epoll registrations: Linux keys a registration by descriptor
/// number and open file description, so each number needs a descriptor of
/// its own. `epolls` holds the registrations that name it, one for each
/// epoll with one: a successful `EPOLL_CTL_ADD` of the number adds the
/// epoll's, and its successful `EPOLL_CTL_DEL` removes it, as does the end
/// of the epoll's open file description ([`EpollIdentity`]), as Linux frees
/// an eventpoll's registrations with it. The key closes with the last of
/// them, or with the pidfd.
#[derive(Debug)]
struct EpollKey {
    file: std::fs::File,
    epolls: Vec<KeyRegistration>,
}

/// One epoll's registration of an epoll key.
#[derive(Debug)]
struct KeyRegistration {
    /// The epoll's identity, which every descriptor of its open file
    /// description shares, received ones included, and whose end ends the
    /// registration.
    identity: std::sync::Weak<EpollIdentity>,
    /// A host descriptor of the epoll, through which the pidfd's end removes
    /// the registration (`GuestPidfd`'s `Drop`). It goes with the
    /// registration, so it keeps the epoll no longer than the registration
    /// lasts.
    epoll: std::fs::File,
}

impl EpollKey {
    /// Where `epolls` holds the registration in the epoll whose identity is
    /// `epoll`.
    fn position(&self, epoll: &EpollIdentity) -> Option<usize> {
        self.epolls
            .iter()
            .position(|held| std::ptr::eq(held.identity.as_ptr(), epoll))
    }
}

// TODO-HUMAN-REVIEW(https://github.com/rrnewton/reverie/pull/1410): Review pidfd ends.
/// Removes the pidfd's epoll registrations, as Linux's `eventpoll_release`
/// does when it releases the pidfd's open file description. The host would
/// remove them itself when the shadow's open file description goes, but a
/// copy of the shadow that a fork of the supervisor holds keeps that
/// description, and the registrations with it, past the pidfd's last close.
impl Drop for GuestPidfd {
    fn drop(&mut self) {
        let keys = self
            .epoll_keys
            .get_mut()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        for key in keys.values() {
            for epoll in key.epolls.iter().map(|held| &held.epoll) {
                // SAFETY: both descriptors are live, and a DEL reads no
                // event.
                unsafe {
                    libc::epoll_ctl(
                        epoll.as_raw_fd(),
                        libc::EPOLL_CTL_DEL,
                        key.file.as_raw_fd(),
                        std::ptr::null_mut(),
                    )
                };
            }
        }
    }
}

#[cfg(test)]
thread_local! {
    /// Makes this thread's next epoll copy fail, as a full descriptor table
    /// would.
    pub(super) static EPOLL_COPY_FAILS_TEST_HOOK: std::cell::Cell<bool> =
        const { std::cell::Cell::new(false) };
}

/// A new host descriptor of the epoll whose host descriptor is `epoll_fd`.
fn copy_epoll(epoll_fd: RawFd) -> Option<std::fs::File> {
    #[cfg(test)]
    if EPOLL_COPY_FAILS_TEST_HOOK.with(|fails| fails.replace(false)) {
        return None;
    }
    // SAFETY: epoll_fd is the caller's live host descriptor of the epoll;
    // F_DUPFD_CLOEXEC returns a new one.
    let copy = unsafe { libc::fcntl(epoll_fd, libc::F_DUPFD_CLOEXEC, libc::STDERR_FILENO + 1) };
    // SAFETY: fcntl returned a new descriptor that nothing else owns.
    (copy >= 0).then(|| unsafe { std::fs::File::from_raw_fd(copy) })
}

impl GuestPidfd {
    /// The process the pidfd names.
    pub(crate) fn target(&self) -> reverie::SignalProcessId {
        self.target
    }

    /// The host descriptor that poll and select watch for the pidfd.
    pub(super) fn readiness_fd(&self) -> RawFd {
        self.shadow.as_raw_fd()
    }

    /// `epoll_ctl` of `operation` in the epoll with host descriptor
    /// `epoll_fd` and identity `epoll`, for guest descriptor number `fd` of
    /// the pidfd, which `control` makes on the host descriptor it is given:
    /// the number's epoll key. An ADD makes the key if the number has none
    /// and closes it again if the ADD fails; it also keeps a host descriptor
    /// of the epoll, for the pidfd's end. An ADD that cannot make either
    /// fails as Linux fails an allocation, ENOMEM, before the host sees it. A
    /// MOD or DEL of a number without a key names the shadow itself, which no
    /// registration names, so the host answers it as Linux answers for a
    /// registration that does not exist: ENOENT, after its other checks. A
    /// DEL that succeeds removes the registration in the epoll it named,
    /// whichever descriptor of the epoll named it, and closes the key with
    /// its last registration. A successful ADD also tells the epoll, whose
    /// end ends the registration ([`Self::epoll_ended`]).
    pub(super) fn epoll_control(
        self: &Arc<Self>,
        fd: libc::c_int,
        operation: libc::c_int,
        epoll_fd: RawFd,
        epoll: &Arc<EpollIdentity>,
        control: impl FnOnce(RawFd) -> i64,
    ) -> i64 {
        let mut keys = self
            .epoll_keys
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut made = None;
        let key = match keys.get(&fd) {
            Some(key) => key.file.as_raw_fd(),
            None if operation == libc::EPOLL_CTL_ADD => {
                // SAFETY: shadow owns a live descriptor; F_DUPFD_CLOEXEC
                // returns a new one above the standard descriptors.
                let key = unsafe {
                    libc::fcntl(
                        self.shadow.as_raw_fd(),
                        libc::F_DUPFD_CLOEXEC,
                        libc::STDERR_FILENO + 1,
                    )
                };
                if key < 0 {
                    return negative_errno(libc::ENOMEM);
                }
                // SAFETY: fcntl returned a new descriptor that nothing else
                // owns.
                made = Some(unsafe { std::fs::File::from_raw_fd(key) });
                key
            }
            None => self.shadow.as_raw_fd(),
        };
        // The host descriptor of the epoll that the registration keeps
        // (`KeyRegistration::epoll`).
        let mut epoll_copy = None;
        if operation == libc::EPOLL_CTL_ADD {
            match copy_epoll(epoll_fd) {
                Some(copy) => epoll_copy = Some(copy),
                None => return negative_errno(libc::ENOMEM),
            }
        }
        let result = control(key);
        match (operation, result) {
            (libc::EPOLL_CTL_ADD, 0) => {
                keys.entry(fd)
                    .or_insert_with(|| EpollKey {
                        file: made.take().expect("an ADD without a key made one"),
                        epolls: Vec::new(),
                    })
                    .epolls
                    .push(KeyRegistration {
                        identity: Arc::downgrade(epoll),
                        epoll: epoll_copy.take().expect("an ADD made its epoll copy"),
                    });
                epoll.register_pidfd(self, fd);
            }
            (libc::EPOLL_CTL_DEL, 0) => {
                if let std::collections::btree_map::Entry::Occupied(mut key) = keys.entry(fd) {
                    if let Some(held) = key.get().position(epoll) {
                        key.get_mut().epolls.swap_remove(held);
                    }
                    if key.get().epolls.is_empty() {
                        key.remove();
                    }
                }
                epoll.unregister_pidfd(self, fd);
            }
            _ => {}
        }
        result
    }

    /// Ends the registration of guest descriptor number `fd` of the pidfd
    /// in `epoll`, whose open file description has ended, and closes the
    /// number's key if it was the last. The host's registration goes too,
    /// with a DEL through the registration's host descriptor of the epoll:
    /// a host descriptor of the epoll that outlives its guest owners, as a
    /// fork of the supervisor or a retained descriptor table holds one, would
    /// otherwise keep watching the shadow, and report the exit to an epoll
    /// that watches it, after Linux has freed the registration
    /// (`eventpoll_release`).
    pub(super) fn epoll_ended(&self, fd: libc::c_int, epoll: &EpollIdentity) {
        let _ended;
        let mut keys = self
            .epoll_keys
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let std::collections::btree_map::Entry::Occupied(mut key) = keys.entry(fd) {
            if let Some(held) = key.get().position(epoll) {
                let registration = key.get_mut().epolls.swap_remove(held);
                // SAFETY: both descriptors are live, and a DEL reads no
                // event.
                unsafe {
                    libc::epoll_ctl(
                        registration.epoll.as_raw_fd(),
                        libc::EPOLL_CTL_DEL,
                        key.get().file.as_raw_fd(),
                        std::ptr::null_mut(),
                    )
                };
            }
            if key.get().epolls.is_empty() {
                _ended = key.remove();
            }
        }
    }

    #[cfg(test)]
    pub(super) fn shadow(&self) -> std::sync::Weak<std::fs::File> {
        Arc::downgrade(&self.shadow)
    }

    /// How many epoll registrations name the pidfd, each with its host
    /// descriptor of the epoll.
    #[cfg(test)]
    pub(super) fn epoll_registrations(&self) -> usize {
        self.epoll_keys
            .lock()
            .unwrap()
            .values()
            .map(|key| key.epolls.len())
            .sum()
    }
}

/// The record of guest descriptor `fd`, if it is a pidfd for a guest process
/// that this call can use. It is found with the descriptor
/// (`LoadedStaticElf::guest_file`), so a call that finds the descriptor open
/// finds the record, and one that finds it closed finds none. The record
/// then lives until the call ends: the table holds it until a retirement of
/// the descriptor, and a retirement leaves it, with the descriptor's copy,
/// to a call that found the descriptor first (`CopyLedger::retire_copy`).
/// So a host descriptor of the record stays the record's for the whole call.
pub(super) fn guest_pidfd(state: &LoadedStaticElf, fd: libc::c_int) -> Option<Arc<GuestPidfd>> {
    let pidfd = state.guest_pidfds.get(&fd)?;
    state.guest_file(fd)?;
    pidfd.upgrade()
}

/// The inode number that `stat` reports for a pidfd for `target`, a guest
/// process. Linux gives every process one pidfs inode, which all its pidfds
/// share, whoever opened them, and numbers them from 2 upward as pids are
/// made (`pidfs_add_pid`, fs/pidfs.c), so pidfds compare equal by inode number
/// exactly when they name the same process. The stand-in's own inode is the
/// supervisor's; this number is one per guest process instead, with the top
/// bit set, which no pidfs inode number reaches.
pub(super) fn pidfd_inode(target: reverie::SignalProcessId) -> u64 {
    1 << 63 | target.generation
}

/// The host descriptor whose readiness is guest descriptor `fd`'s: the shadow
/// of a pidfd for a guest process, whose own host descriptor never becomes
/// readable while the run lasts, and otherwise the descriptor itself.
/// `None` when `fd` is not open. A caller asks a shadow only for
/// [`PIDFD_POLL_EVENTS`]; the shadow stays the pidfd's for the rest of the
/// call ([`guest_pidfd`]).
pub(super) fn readiness_host_fd(state: &LoadedStaticElf, fd: libc::c_int) -> Option<RawFd> {
    let pidfd = guest_pidfd(state, fd);
    let host_fd = host_fd(state, fd)?;
    Some(pidfd.map_or(host_fd, |pidfd| pidfd.readiness_fd()))
}

/// The events that poll and select ask a pidfd's shadow for: all that
/// Linux's `pidfd_poll` reports besides `POLLHUP`, which poll reports
/// unasked (fs/pidfs.c). The shadow is a socket, which also reports that it
/// is writable and, once hung up, `POLLRDHUP`; a pidfd reports neither.
pub(super) const PIDFD_POLL_EVENTS: libc::c_short = libc::POLLIN | libc::POLLRDNORM;

/// The events of an epoll registration of a pidfd's shadow, for a guest's
/// registration of the pidfd with `events`: the same, without the readiness
/// a pidfd never reports and its shadow socket does (see
/// [`PIDFD_POLL_EVENTS`]). With `EPOLLEXCLUSIVE`, Linux refuses
/// (EINVAL) any of those but `EPOLLOUT`, so only `EPOLLOUT` is taken then,
/// and the host refuses the rest as Linux does.
pub(super) fn pidfd_epoll_events(events: u32) -> u32 {
    let mut never = libc::EPOLLOUT as u32;
    if events & libc::EPOLLEXCLUSIVE as u32 == 0 {
        never |= (libc::EPOLLWRNORM | libc::EPOLLWRBAND | libc::EPOLLRDHUP) as u32;
    }
    events & !never
}

// AUTONOMOUS-BOT-IMPLEMENTED
// TODO-HUMAN-REVIEW(PR-235): Review self-only virtual pidfd translation.
pub(super) fn pidfd_open(state: &mut LoadedStaticElf, args: &[u64; 6]) -> i64 {
    let flags = args[1] as libc::c_uint;
    if flags & !(libc::O_NONBLOCK as libc::c_uint) != 0 {
        return negative_errno(libc::EINVAL);
    }
    // Linux refuses a pid that cannot name one process before it looks the
    // pid up (`pidfd_open`, kernel/pid.c).
    let pid = args[0] as libc::pid_t;
    if pid <= 0 {
        return negative_errno(libc::EINVAL);
    }
    // AUTONOMOUS-BOT-IMPLEMENTED
    guest_pidfd_open(state, pid, flags)
}

/// Linux's ptrace-access predicate for `pidfd_getfd`
/// (`PTRACE_MODE_ATTACH_REALCREDS`, kernel/ptrace.c v6.17:298-346 and
/// security/commoncap.c v6.17:164-182), shared by every caller that
/// checks it (this handler and the robust-list reader in `futex.rs`):
/// the caller's own thread group is always allowed; otherwise the
/// target must be dumpable or the caller must hold effective
/// `CAP_SYS_PTRACE`, AND the caller's real ids must match the target's
/// or the target's permitted set must be a subset of the caller's
/// permitted set or the caller holds effective `CAP_SYS_PTRACE`. A
/// dumpable target alone never grants access.
pub(super) fn pidfd_ptrace_access_allowed(
    same_process: bool,
    target_dumpable: bool,
    caller_ids_match: bool,
    caller_effective: u64,
    caller_permitted: u64,
    target_permitted: u64,
) -> bool {
    if same_process {
        return true;
    }
    let capable = caller_effective & (1_u64 << CAP_SYS_PTRACE) != 0;
    // The mm gate, then the credential gate, then the commoncap
    // permitted-subset gate, in the kernel's order.
    if !target_dumpable && !capable {
        return false;
    }
    if !caller_ids_match && !capable {
        return false;
    }
    capable || target_permitted & !caller_permitted == 0
}

// AUTONOMOUS-BOT-IMPLEMENTED
// TODO-HUMAN-REVIEW(D133876931): Review cross-process descriptor duplication.
/// `pidfd_getfd(pidfd, targetfd, flags)` (kernel `pidfd_getfd`,
/// kernel/pid.c v6.17:828-884, 903-921): duplicate `targetfd` of the
/// pidfd's process into the caller, sharing its open file description,
/// with `FD_CLOEXEC` set. Linux's order: reserved flags (`EINVAL`), the
/// pidfd itself (`EBADF`), the target task (`ESRCH` when it has gone,
/// including a leader that exited while its group lives on), the
/// ptrace-access check (`EPERM`), then the target descriptor (`EBADF`,
/// or `ESRCH` for a task on its way out whose files are gone), and only
/// then allocation (`EMFILE`). The permission check runs first under
/// the lifecycle lock, and again with the descriptor read under the
/// target table's lock (the table-then-lifecycle order an exec's table
/// publication uses), so no exec of the target can change its
/// credentials or its descriptors between the check the capture relies
/// on and the capture itself, as Linux's `exec_update_lock` window
/// across `ptrace_may_access` and `fget_task` excludes one
/// (kernel/pid.c v6.17:833-842). The duplicate carries the source's
/// executor records (its pidfd record, pipe owner, synthetic `/proc`
/// description and stable inode, native poll binding, output alias), as
/// a `dup` carries them: the guest gets the same open file description,
/// not only its host file. A virtual signalfd and an epoll are refused
/// by name instead: their records live in the owner's own process state
/// and cannot cross. This handler only captures the description: the
/// dispatch runs it with the caller's own table lock released (see
/// `execute_checked`), so the target table is locked with no other
/// table lock held, and completes the call by installing the capture
/// in the caller's current table under that table's lock, where the
/// lowest-free allocation and publication are one transaction a
/// sibling thread's concurrent table change cannot be lost from. A
/// call that captures nothing installs and publishes nothing.
pub(super) fn pidfd_getfd(state: &mut LoadedStaticElf, args: &[u64; 6]) -> i64 {
    let flags = args[2] as libc::c_uint;
    if flags != 0 {
        return negative_errno(libc::EINVAL);
    }
    let pidfd = args[0] as libc::c_int;
    let Some(record) = guest_pidfd(state, pidfd) else {
        // A descriptor that is a host pidfd but has no guest-process
        // record (a stand-in for the caller's own process outside any
        // process family) is refused by name, not misnamed `EBADF`.
        if let Some(host) = host_fd(state, pidfd)
            && host_pidfd(host)
        {
            state.pidfd_refusal = Some(GETFD_WITHOUT_RECORD_REFUSAL);
            return negative_errno(libc::ENOSYS);
        }
        return negative_errno(libc::EBADF);
    };
    let target = record.target();
    let target_pid = target.tgid.as_raw();
    let targetfd = args[1] as libc::c_int;

    // One lifecycle window for the permission check and the target
    // table: the caller's generation distinguishes its own process from
    // an earlier process that held the same numeric pid.
    enum TargetTable {
        Own,
        Foreign(Arc<Mutex<FileTableState>>),
    }
    let table = {
        let lifecycle = state
            .task_lifecycle
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let caller_generation = lifecycle.get(state.tid).map(|task| task.process_generation);
        // The leader task of the pidfd's exact process, if it still
        // lives: a pidfd names its leader (`get_pid_task`), so a group
        // whose leader has exited is gone for this call even where the
        // caller is a surviving thread of it (kernel/pid.c
        // v6.17:844-874).
        let leader = lifecycle
            .get(target_pid)
            .filter(|task| task.tgid == target_pid && task.process_generation == target.generation);
        if target_pid == state.pid && caller_generation == Some(target.generation) {
            if leader.is_none() {
                return negative_errno(libc::ESRCH);
            }
            TargetTable::Own
        } else {
            let task = match leader {
                Some(task) => Some((task.dumpable, task.capability_permitted)),
                None => lifecycle
                    .zombie_capabilities(target_pid, target.generation)
                    .map(|(_, permitted, _)| (true, permitted)),
            };
            let Some((dumpable, target_permitted)) = task else {
                // No live leader task and no zombie record for the
                // pidfd's exact process: reaped (kernel/pid.c
                // v6.17:857-858, 872-874).
                return negative_errno(libc::ESRCH);
            };
            // Every guest task holds the launcher's ids, so the id
            // check matches; the capability half is the live one.
            if !pidfd_ptrace_access_allowed(
                false,
                dumpable,
                true,
                state.credentials.capability_effective,
                state.credentials.capability_permitted,
                target_permitted,
            ) {
                return negative_errno(libc::EPERM);
            }
            if leader.is_none() {
                // Access passed, but the leader is a zombie: the
                // exit check answers ESRCH (kernel/pid.c
                // v6.17:857-858).
                return negative_errno(libc::ESRCH);
            }
            match lifecycle
                .parent_death
                .file_tables
                .get(&(target_pid, target.generation))
                .and_then(|table| table.upgrade())
            {
                Some(table) => TargetTable::Foreign(table),
                None => return negative_errno(libc::ESRCH),
            }
        }
    };

    if matches!(table, TargetTable::Own) {
        // Capture exactly what the `fcntl(fd, F_DUPFD_CLOEXEC, 0)` path
        // installs, which also resolves inherited standard descriptors
        // outside the files map and carries every record of the
        // description. The host file and its records — the signalfd
        // mask and keeper above all, which live in the shared process
        // signal state, not in the snapshot — must be taken atomically
        // against a sibling's close or dup2 of the source, so the
        // capture runs under the caller's own table lock, the lock
        // every table-mutating call of a sibling takes, after
        // installing the table as it is then. (The dispatch released
        // that lock for this call, so taking it here cannot
        // self-deadlock, and no target table is involved.) Once
        // captured, every record is strongly held, so the dispatch's
        // later installation under the same lock needs no live source
        // alias. An executor with no recorded table cannot have a
        // record-bearing pidfd for its own process, so that state is
        // unreachable in production and fails closed below rather than
        // capture from an unprotected snapshot. A source a sibling already
        // closed before the capture is not in the installed table and
        // answers EBADF, as a close ordered before the call does.
        fn capture_own(state: &mut LoadedStaticElf, targetfd: libc::c_int) -> i64 {
            let Some(host) = host_fd(state, targetfd) else {
                return negative_errno(libc::EBADF);
            };
            // SAFETY: `host` is a live descriptor of the caller's
            // table; F_DUPFD_CLOEXEC returns a new owned descriptor
            // of the same open file description.
            let duplicated = unsafe { libc::fcntl(host, libc::F_DUPFD_CLOEXEC, 0) };
            if duplicated < 0 {
                return io_error(std::io::Error::last_os_error());
            }
            // SAFETY: fcntl returned a new owned descriptor.
            let file = unsafe { std::fs::File::from_raw_fd(duplicated) };
            #[cfg(test)]
            run_one_shot_test_hook(&GETFD_SELF_CAPTURE_TEST_HOOK);
            // Mask and keeper in one hold of the signal state: the
            // pair the description had at one instant.
            let (signalfd_mask, signalfd_carrier) = signalfd_record(state, targetfd);
            state.captured_getfd_install = Some(CapturedGetfdInstall {
                file,
                source: DuplicateFdSource {
                    output_alias: output_alias(state, targetfd),
                    proc_inode: state.proc_files.get(&targetfd).copied(),
                    synthetic_proc_nofollow: state.synthetic_proc_nofollow_fds.contains(&targetfd),
                    fdinfo: state.fdinfo_files.get(&targetfd).cloned(),
                    object_inode: guest_fd_object_identity(state, targetfd),
                    signalfd_mask,
                    signalfd_carrier,
                    native_poll: native_poll_description(state, targetfd),
                    pipe_owner: state.pipe_owners.get(&targetfd).cloned(),
                    guest_pidfd: guest_pidfd(state, targetfd),
                },
            });
            // The dispatch installs the capture and answers with the
            // descriptor it allocated; this value never reaches the
            // guest.
            0
        }
        // The caller's own recorded table (the dispatch's table, as
        // the run recorded it for this exact process).
        let own_table = {
            let lifecycle = state
                .task_lifecycle
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            lifecycle
                .get(state.tid)
                .filter(|task| task.tgid == state.pid)
                .and_then(|task| {
                    lifecycle
                        .parent_death
                        .file_tables
                        .get(&(state.pid, task.process_generation))
                        .and_then(|table| table.upgrade())
                })
        };
        return match own_table {
            Some(own_table) => {
                let mut own_table = own_table.lock().expect("KVM file-table lock poisoned");
                match own_table.install(state) {
                    Ok(()) => capture_own(state, targetfd),
                    Err(error) => io_error(error),
                }
            }
            None => {
                // Production executors register their table before a
                // record-bearing pidfd can exist. Fail closed rather
                // than capture a description without the lock that
                // makes the capture atomic.
                debug_assert!(
                    false,
                    "record-bearing self pidfd_getfd without a recorded caller table"
                );
                negative_errno(libc::EBADF)
            }
        };
    }
    let TargetTable::Foreign(table) = table else {
        unreachable!("the own table returned above");
    };
    #[cfg(test)]
    run_one_shot_test_hook(&GETFD_CHECKED_TEST_HOOK);
    // Capture the description under the target table's lock, then let
    // the lock go before the dispatch installs it in the caller's
    // table. The capture re-checks the target's life and credentials
    // with the target table held, in the table-then-lifecycle order an
    // exec's publication uses: an exec of the target either published
    // its new credentials and descriptors under this same table lock
    // before the capture, or waits for it, so the check and the read
    // see one image of the target, never an old authorization over a
    // new image's descriptor.
    let (duplicate, source) = {
        let table = table.lock().expect("KVM file-table lock poisoned");
        {
            let lifecycle = state
                .task_lifecycle
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let Some(task) = lifecycle.get(target_pid).filter(|task| {
                task.tgid == target_pid && task.process_generation == target.generation
            }) else {
                return negative_errno(libc::ESRCH);
            };
            if !pidfd_ptrace_access_allowed(
                false,
                task.dumpable,
                true,
                state.credentials.capability_effective,
                state.credentials.capability_permitted,
                task.capability_permitted,
            ) {
                return negative_errno(libc::EPERM);
            }
        }
        if table.signalfd_fds.contains(&targetfd) {
            state.pidfd_refusal = Some(GETFD_SIGNALFD_REFUSAL);
            return negative_errno(libc::ENOSYS);
        }
        let slot = table.slots.get(&targetfd);
        // Only an epoll is refused here. An ordinary eventfd carries a
        // native poll binding too, and duplicates as Linux duplicates
        // it, with its binding carried below.
        if slot.is_some_and(|slot| {
            matches!(
                slot.native_poll
                    .as_ref()
                    .map(|binding| &binding.description),
                Some(NativePollDescription::Epoll(_))
            )
        }) {
            state.pidfd_refusal = Some(GETFD_EPOLL_REFUSAL);
            return negative_errno(libc::ENOSYS);
        }
        let (host, open_standard) = match super::facts::table_host_fd(&table, targetfd) {
            Ok(found) => found,
            Err(_) => return negative_errno(libc::EBADF),
        };
        // SAFETY: `host` is a live descriptor of the target table;
        // F_DUPFD_CLOEXEC returns a new owned descriptor of the same
        // open file description.
        let duplicated = unsafe { libc::fcntl(host, libc::F_DUPFD_CLOEXEC, 0) };
        if duplicated < 0 {
            // A host duplication failure (EMFILE, ENOMEM) is the host's
            // own answer, not a missing target descriptor.
            return io_error(std::io::Error::last_os_error());
        }
        // SAFETY: fcntl returned a new owned descriptor.
        let duplicate = unsafe { std::fs::File::from_raw_fd(duplicated) };
        // An eventfd that crosses to another process is exported as
        // one sent by `SCM_RIGHTS` is: a writer outside its owner's
        // process controls its readiness, so no alias may be admitted
        // to a native poll interest set on its provenance alone.
        let native_poll = slot
            .and_then(|slot| slot.native_poll.as_ref())
            .map(|binding| binding.description.clone());
        if let Some(NativePollDescription::EventFd(eventfd)) = &native_poll {
            eventfd.escaped.store(true, Ordering::SeqCst);
        }
        let source = DuplicateFdSource {
            output_alias: output_alias_from_sets(
                targetfd,
                &table.stdout_alias_fds,
                &table.stderr_alias_fds,
                open_standard,
            ),
            proc_inode: slot.and_then(|slot| slot.proc_inode),
            synthetic_proc_nofollow: table.synthetic_proc_nofollow_fds.contains(&targetfd),
            fdinfo: slot.and_then(|slot| slot.fdinfo.clone()),
            object_inode: slot
                .and_then(|slot| slot.object_inode.clone())
                .unwrap_or_else(|| {
                    // No recorded identity: fall back to the synthetic
                    // identity a descriptor without one gets, as
                    // `guest_fd_object_identity` does. The description
                    // itself is still shared.
                    Arc::new(GuestFileIdentity {
                        inode: synthetic_guest_fd_inode(targetfd, false),
                        timestamps: IdentityTimestamps::Host,
                    })
                }),
            signalfd_mask: None,
            signalfd_carrier: None,
            native_poll,
            pipe_owner: slot.and_then(|slot| slot.pipe_owner.clone()),
            guest_pidfd: slot.and_then(|slot| slot.guest_pidfd.clone()),
        };
        (duplicate, source)
    };
    // The dispatch installs the capture exactly as a dup installs, at
    // the caller's lowest free descriptor under its table lock. The
    // captured records stay strongly held in the capture until that
    // publication, so the source closing its last copy meanwhile
    // cannot erase the duplicate's identity.
    state.captured_getfd_install = Some(CapturedGetfdInstall {
        file: duplicate,
        source,
    });
    // The dispatch answers with the descriptor it allocated; this
    // value never reaches the guest.
    0
}

/// A new pidfd for the supervisor process, with `flags`.
fn supervisor_pidfd(flags: libc::c_uint) -> Result<std::fs::File, i64> {
    // SAFETY: pidfd_open takes no pointers.
    let host_fd = unsafe {
        libc::syscall(libc::SYS_pidfd_open, libc::getpid(), flags as libc::c_uint) as libc::c_int
    };
    if host_fd < 0 {
        return Err(io_error(std::io::Error::last_os_error()));
    }
    // SAFETY: pidfd_open returned a new owned descriptor.
    Ok(unsafe { std::fs::File::from_raw_fd(host_fd) })
}

/// `pidfd_open(pid, flags)` for a guest process, the caller's own included:
/// a pidfd for the supervisor stands in for it in the guest's table, and a
/// shadow socket shows its readiness ([`GuestPidfd`]). Every pidfd for one
/// process, whoever opened it, so reports one inode ([`pidfd_inode`]). Like
/// Linux, it opens one for a process that has exited and has not been
/// reaped, which is then readable at once.
// TODO-HUMAN-REVIEW(https://github.com/rrnewton/reverie/pull/1410): Review pidfds for guest
// processes, their stand-ins and their shadow sockets.
fn guest_pidfd_open(state: &mut LoadedStaticElf, pid: libc::pid_t, flags: libc::c_uint) -> i64 {
    let target = match pidfd_target(state, pid) {
        Ok(target) => target,
        Err(errno) => return negative_errno(errno),
    };
    let Some(registry) = state.children.registry() else {
        // A task outside any process family has no other process to name,
        // and no record of its own process's life. The supervisor outlives
        // it, so the stand-in alone, never readable, is a pidfd for it, whose
        // metadata is the kernel's, as for a pidfd from outside the run.
        if pid != state.pid {
            return negative_errno(libc::ESRCH);
        }
        return match supervisor_pidfd(flags) {
            Ok(file) => insert_file_with_flags(state, file, true, None),
            Err(error) => error,
        };
    };
    let shadow = match registry.open_pidfd_shadow(target) {
        Ok(shadow) => shadow,
        Err(errno) => return negative_errno(errno),
    };
    let file = match supervisor_pidfd(flags) {
        Ok(file) => file,
        Err(error) => return error,
    };
    let fd = insert_file_with_flags(state, file, true, None);
    if fd >= 0 {
        let pidfd = Arc::new(GuestPidfd {
            target,
            shadow,
            epoll_keys: Default::default(),
        });
        state
            .guest_pidfds
            .insert(fd as libc::c_int, Arc::downgrade(&pidfd));
        // The table holds the record once this call has published it.
        state.pidfd_holds.push(pidfd);
    }
    fd
}

/// The process `pid` names for a pidfd, as Linux's `pidfd_open` finds it
/// (kernel/pid.c): a live process whose thread group id `pid` is, even once
/// its leader has exited, or a process that has exited and has not been
/// reaped. ENOENT for a live thread that leads no thread group
/// (`pidfd_prepare`, [kernel/fork.c:1844] in v6.17), ESRCH for a pid that
/// names no process.
///
/// [kernel/fork.c:1844]: https://git.kernel.org/pub/scm/linux/kernel/git/stable/linux.git/tree/kernel/fork.c?h=v6.17#n1844
fn pidfd_target(
    state: &LoadedStaticElf,
    pid: libc::pid_t,
) -> Result<reverie::SignalProcessId, libc::c_int> {
    let live = {
        let lifecycle = state
            .task_lifecycle
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        match lifecycle.get(pid) {
            Some(task) if task.tgid != pid => return Err(libc::ENOENT),
            Some(task) => Some(task.process_generation),
            None => lifecycle.live_process_generation(pid),
        }
    };
    if let Some(generation) = live {
        return Ok(reverie::SignalProcessId {
            tgid: reverie::Pid::from_raw(pid),
            generation,
        });
    }
    state
        .children
        .registry()
        .and_then(|registry| registry.unreaped_exited_process(pid))
        .ok_or(libc::ESRCH)
}

/// Classifies a carrier received in an SCM_RIGHTS message from the carrier
/// itself, so that the class survives every sender descriptor closing before
/// the receive, with nothing to account for while the right is in flight. A
/// `/proc` snapshot carrier is a memfd with its own name and the full
/// synthetic seal set. A guest that makes such a memfd itself gets
/// only fixed timestamps for it. Anything else, including a failed probe, is a
/// real host object, except a probe that fails only because no descriptor or
/// memory was left to reopen an O_PATH right (see [`CarrierSealProbe`]): the
/// receive needs a descriptor number but no new open file, so it can succeed
/// when the probe cannot. Such an object already matches the carrier name, a
/// regular file and the shared-memory filesystem, and keeps the class of its
/// name: a fixed time on a guest memfd with that name is deterministic, a host
/// time on the backend's carrier is not.
pub(super) fn received_carrier_timestamps(
    state: &LoadedStaticElf,
    file: &std::fs::File,
) -> IdentityTimestamps {
    if !state.host_metadata_timestamps {
        return IdentityTimestamps::Host;
    }
    if !fd_link_target_is(file.as_raw_fd(), b"/memfd:reverie-kvm-proc (deleted)") {
        return IdentityTimestamps::Host;
    }
    let timestamps = IdentityTimestamps::ProcSnapshot;
    match carrier_seals(file) {
        CarrierSealProbe::Seals(seals) if supported_synthetic_proc_seals(seals) => timestamps,
        CarrierSealProbe::OutOfResources => timestamps,
        CarrierSealProbe::Seals(_) | CarrierSealProbe::Absent => IdentityTimestamps::Host,
    }
}

/// What the seal probe of a received object found.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum CarrierSealProbe {
    /// The object's seals.
    Seals(libc::c_int),
    /// A regular file on the shared-memory filesystem whose seals could not be
    /// read because reopening it failed with EMFILE, ENFILE or ENOMEM.
    OutOfResources,
    /// No seals: the object is not a sealable regular shared-memory file, or
    /// the probe failed for another reason.
    Absent,
}

/// The seals of a memfd. An O_PATH description cannot query them, so a fresh
/// read-only description of the same memfd answers for it. Only a regular file
/// on the shared-memory filesystem, where every memfd lives, is reopened, and
/// without blocking: a guest object whose link text merely matches a carrier
/// name, such as a FIFO or a device, could block that open or act on it.
pub(super) fn carrier_seals(file: &std::fs::File) -> CarrierSealProbe {
    use std::os::unix::fs::OpenOptionsExt;

    // SAFETY: file owns a live descriptor and F_GET_SEALS has no third argument.
    let seals = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GET_SEALS) };
    if seals >= 0 {
        return CarrierSealProbe::Seals(seals);
    }
    let mut stat = std::mem::MaybeUninit::<libc::stat>::zeroed();
    // SAFETY: stat is writable and file owns a live descriptor; fstat accepts O_PATH.
    if unsafe { libc::fstat(file.as_raw_fd(), stat.as_mut_ptr()) } != 0 {
        return CarrierSealProbe::Absent;
    }
    // SAFETY: fstat initialized stat on success.
    if unsafe { stat.assume_init() }.st_mode & libc::S_IFMT != libc::S_IFREG {
        return CarrierSealProbe::Absent;
    }
    let mut filesystem = std::mem::MaybeUninit::<libc::statfs>::zeroed();
    // SAFETY: filesystem is writable and file owns a live descriptor; fstatfs accepts O_PATH.
    if unsafe { libc::fstatfs(file.as_raw_fd(), filesystem.as_mut_ptr()) } != 0 {
        return CarrierSealProbe::Absent;
    }
    // SAFETY: fstatfs initialized filesystem on success.
    if unsafe { filesystem.assume_init() }.f_type != libc::TMPFS_MAGIC {
        return CarrierSealProbe::Absent;
    }
    let reopened = match std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK | libc::O_NOCTTY)
        .open(format!("/proc/self/fd/{}", file.as_raw_fd()))
    {
        Ok(reopened) => reopened,
        Err(error)
            if matches!(
                error.raw_os_error(),
                Some(libc::EMFILE | libc::ENFILE | libc::ENOMEM)
            ) =>
        {
            return CarrierSealProbe::OutOfResources;
        }
        Err(_) => return CarrierSealProbe::Absent,
    };
    // SAFETY: reopened owns a live descriptor and F_GET_SEALS has no third argument.
    let seals = unsafe { libc::fcntl(reopened.as_raw_fd(), libc::F_GET_SEALS) };
    if seals >= 0 {
        CarrierSealProbe::Seals(seals)
    } else {
        CarrierSealProbe::Absent
    }
}

pub(super) fn synthetic_guest_fd_symlink_stat(guest_fd: libc::c_int) -> libc::stat {
    // SAFETY: libc::stat is plain-old-data; a zeroed value is valid.
    let mut stat = unsafe { std::mem::zeroed::<libc::stat>() };
    stat.st_dev =
        crate::proc_mechanics::synthetic_dev(SYNTHETIC_DEV_MAJOR, SYNTHETIC_GUEST_FD_DEV_MINOR);
    stat.st_ino = synthetic_guest_fd_inode(guest_fd, true);
    stat.st_mode = libc::S_IFLNK | 0o777;
    stat.st_nlink = 1;
    stat.st_uid = 0;
    stat.st_gid = 0;
    stat.st_blksize = PAGE_SIZE as libc::blksize_t;
    stat
}

pub(super) fn synthetic_guest_fd_symlink_statx(guest_fd: libc::c_int) -> libc::statx {
    // SAFETY: libc::statx is plain-old-data; a zeroed value is valid.
    let mut stat = unsafe { std::mem::zeroed::<libc::statx>() };
    stat.stx_mask = libc::STATX_TYPE
        | libc::STATX_MODE
        | libc::STATX_NLINK
        | libc::STATX_UID
        | libc::STATX_GID
        | libc::STATX_INO
        | libc::STATX_SIZE;
    stat.stx_blksize = PAGE_SIZE as u32;
    stat.stx_nlink = 1;
    stat.stx_mode = (libc::S_IFLNK | 0o777) as u16;
    stat.stx_ino = synthetic_guest_fd_inode(guest_fd, true);
    stat.stx_dev_minor = SYNTHETIC_GUEST_FD_DEV_MINOR;
    stat
}
