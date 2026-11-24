/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! Asynchronous signal mode: `O_ASYNC` through `F_SETFL`, and `FIOASYNC`
//! (Linux v7.1.3 fs/fcntl.c `setfl`, 39-87; fs/ioctl.c `ioctl_fioasync`,
//! 363-382).
//!
//! Turning the mode on registers the host file with its `fasync`, and the
//! kernel then signals the description's owner when it is ready
//! (`send_sigio`, which sends nothing while the owner is NULL). This executor
//! never gives a host description an owner, since a host PID must never
//! stand in for a guest PID (`pipe_owner`). So the mode is forwarded to the
//! host only where no process outside the run can own the description and
//! the file's own `fasync` picks no owner itself (a terminal's makes its
//! foreground group, or the caller, the owner: `__tty_fasync`). That is the
//! admission domain:
//!
//! - the standard input the embedder declared, while it is a pipe, a FIFO or
//!   a regular file (`Provenance::Declared`);
//! - a memory device whose operations have no `fasync` at all: major 1,
//!   minors 1 (mem), 3 (null), 4 (port), 5 (zero) and 7 (full), whatever its
//!   provenance (drivers/char/mem.c:647-719);
//! - a description this run created and never let leave the run: a pipe,
//!   FIFO, socket, regular file or directory by its host type, or an
//!   anonymous-inode keeper by its record.
//!
//! Every change of the status flags of a declared or created description
//! runs under its record's lock, as its export mark does, so the flag seen
//! under the lock is the one the kernel sees. Outside the domain no host
//! call that could turn the mode on is ever made: a request that changes
//! nothing (the mode already on and, for `F_SETFL`, no other settable flag
//! changing) is answered with 0 here, as if it ran when the flags were read,
//! and every other is refused by name
//! (`reverie::UnsupportedOperation::AsyncSignalMode`). Turning the mode off
//! is always forwarded.

use super::pipe_owner::Provenance;
use super::*;

/// The memory devices' major (`MEM_MAJOR`, drivers/char/mem.c:767).
const MEMORY_MAJOR: u32 = 1;

/// The memory devices' minors whose operations have no `.fasync`, so that
/// turning the mode on can neither register nor signal anything: mem
/// (`mem_fops`, mem.c:647-658), null (660-668), port (671-676), zero
/// (679-692) and full (694-699). random and urandom (8 and 9) have
/// `random_fasync` (drivers/char/random.c:1555-1576), which signals at the
/// pool's readiness (712-737). kmsg (11) is left out until its operations
/// are audited on the supported host kernels.
pub(super) const NO_FASYNC_MEMORY_MINORS: [u32; 5] = [1, 3, 4, 5, 7];

/// The status flags `F_SETFL` changes besides `O_ASYNC` (`SETFL_MASK`,
/// fs/fcntl.c:37): what an `F_SETFL` carrying `O_ASYNC` must leave alone to
/// change nothing. The access mode, `O_LARGEFILE` and the rest of the word
/// are ignored by Linux and so here.
pub(super) const SETTABLE_BESIDES_ASYNC: libc::c_int =
    libc::O_APPEND | libc::O_DIRECT | libc::O_NOATIME | libc::O_NONBLOCK;

pub(super) const INHERITED_ASYNC_REFUSAL: &str = "KVM does not turn on asynchronous signal \
     mode for a descriptor this run did not create: an inherited or received description may \
     have an owner outside the run";
pub(super) const CHARACTER_DEVICE_ASYNC_REFUSAL: &str = "KVM does not turn on asynchronous \
     signal mode for a character device other than the memory devices without fasync (mem, \
     null, port, zero and full): a terminal's fasync makes a host process its owner";
pub(super) const RANDOM_ASYNC_REFUSAL: &str = "KVM does not turn on asynchronous signal mode \
     for /dev/random or /dev/urandom: their fasync signals an owner outside the run when the \
     entropy pool becomes ready";
pub(super) const KMSG_ASYNC_REFUSAL: &str = "KVM does not turn on asynchronous signal mode \
     for /dev/kmsg: its operations are not audited on the supported host kernels";
pub(super) const BLOCK_DEVICE_ASYNC_REFUSAL: &str =
    "KVM does not turn on asynchronous signal mode for a block device";
pub(super) const KIND_ASYNC_REFUSAL: &str =
    "KVM does not turn on asynchronous signal mode for a description of this kind";
pub(super) const FSTAT_ASYNC_REFUSAL: &str =
    "KVM does not turn on asynchronous signal mode for a descriptor it cannot fstat";

/// Where a request that turns the mode on may go.
pub(super) enum AsyncAdmission {
    /// To a declared or created description, under this record's lock.
    Record(Arc<pipe_owner::PipeOwner>),
    /// To a memory device without `fasync`, as it is.
    Device,
    /// Nowhere: the description is outside the domain, for this reason.
    Outside(&'static str),
}

/// Classifies the description behind `guest_fd` (`host_fd`) from `fstat` of
/// that descriptor itself, never of a path, so what is checked is what a
/// forwarded call reaches.
pub(super) fn classify_async_admission(
    state: &LoadedStaticElf,
    guest_fd: i32,
    host_fd: RawFd,
) -> AsyncAdmission {
    let Some(stat) = descriptor_stat(host_fd) else {
        return AsyncAdmission::Outside(FSTAT_ASYNC_REFUSAL);
    };
    let kind = stat.st_mode & libc::S_IFMT;
    // A block device is refused before the major is read: block major 1 is
    // the RAM disk.
    if kind == libc::S_IFBLK {
        return AsyncAdmission::Outside(BLOCK_DEVICE_ASYNC_REFUSAL);
    }
    if kind == libc::S_IFCHR {
        return memory_device_admission(libc::major(stat.st_rdev), libc::minor(stat.st_rdev));
    }
    let Some(record) = pipe_owner::description_record(state, guest_fd) else {
        return AsyncAdmission::Outside(INHERITED_ASYNC_REFUSAL);
    };
    let admitted = match record.provenance() {
        Provenance::Pipe => kind == libc::S_IFIFO,
        Provenance::Declared => kind == libc::S_IFIFO || kind == libc::S_IFREG,
        Provenance::Created => matches!(
            kind,
            libc::S_IFIFO | libc::S_IFSOCK | libc::S_IFREG | libc::S_IFDIR
        ),
        // An anonymous inode has no file type (`alloc_anon_inode`,
        // fs/libfs.c:1684): its record is what says what it is.
        Provenance::AnonymousKeeper => true,
    };
    if admitted {
        AsyncAdmission::Record(record)
    } else {
        AsyncAdmission::Outside(KIND_ASYNC_REFUSAL)
    }
}

/// Rule B for a character device: `S_ISCHR` was checked, then the major,
/// then the minor (an unlisted minor of major 1 cannot be opened:
/// `memory_open`, mem.c:723-745).
pub(super) fn memory_device_admission(major: u32, minor: u32) -> AsyncAdmission {
    match (major, minor) {
        (MEMORY_MAJOR, minor) if NO_FASYNC_MEMORY_MINORS.contains(&minor) => {
            AsyncAdmission::Device
        }
        (MEMORY_MAJOR, 8 | 9) => AsyncAdmission::Outside(RANDOM_ASYNC_REFUSAL),
        (MEMORY_MAJOR, 11) => AsyncAdmission::Outside(KMSG_ASYNC_REFUSAL),
        _ => AsyncAdmission::Outside(CHARACTER_DEVICE_ASYNC_REFUSAL),
    }
}

fn descriptor_stat(fd: RawFd) -> Option<libc::stat> {
    let mut stat = std::mem::MaybeUninit::<libc::stat>::zeroed();
    // SAFETY: stat is writable and fd names a live descriptor.
    if unsafe { libc::fstat(fd, stat.as_mut_ptr()) } != 0 {
        return None;
    }
    // SAFETY: fstat initialized stat on success.
    Some(unsafe { stat.assume_init() })
}

fn refuse(state: &mut LoadedStaticElf, reason: &'static str) -> i64 {
    state.async_signal_refusal = Some(reason);
    negative_errno(libc::ENOSYS)
}

/// `ioctl(guest_fd, FIOASYNC, address)`, the command matched on its low 32
/// bits by the caller, which has also refused an `O_PATH` descriptor.
// TODO-HUMAN-REVIEW(reverie-kvm): Review the asynchronous signal mode admission domain.
pub(super) fn fioasync(
    memory: &GuestMemory,
    state: &mut LoadedStaticElf,
    guest_fd: i32,
    host_fd: RawFd,
    address: u64,
) -> i64 {
    // Linux copies the int first (`get_user`, fs/ioctl.c:369); the host call
    // gets this copy, never the guest's pointer.
    let value = match read_guest_struct::<libc::c_int>(&ProviderMemory(memory), address) {
        Ok(value) => value,
        Err(error) => return error,
    };
    // SAFETY: host_fd is pinned by the caller's file-table transaction, and
    // value is readable supervisor storage for one int.
    let forward = || zero_or_errno(unsafe { libc::ioctl(host_fd, libc::FIOASYNC, &value) });
    if value == 0 {
        return match pipe_owner::description_record(state, guest_fd) {
            Some(record) => record.locked(forward),
            None => forward(),
        };
    }
    let reason = match classify_async_admission(state, guest_fd, host_fd) {
        AsyncAdmission::Device => return forward(),
        AsyncAdmission::Record(record) => match record.with_async_on(forward) {
            Ok(result) => return result,
            Err(reason) => reason,
        },
        AsyncAdmission::Outside(reason) => reason,
    };
    let current = fd_status_flags(host_fd);
    #[cfg(test)]
    run_one_shot_test_hook(&ASYNC_OBSERVED_TEST_HOOK);
    match current {
        // FIOASYNC on a description whose FASYNC is already set changes
        // nothing, and Linux answers 0 (fs/ioctl.c:375).
        Ok(flags) if flags & libc::O_ASYNC != 0 => 0,
        Ok(_) => refuse(state, reason),
        Err(error) => error,
    }
}

/// `F_SETFL` of `flags`, already reduced to the settable flags, when they
/// carry `O_ASYNC`; `apply` makes the host change.
pub(super) fn status_flags_async_on(
    state: &mut LoadedStaticElf,
    guest_fd: i32,
    host_fd: RawFd,
    flags: libc::c_int,
    apply: impl FnOnce(&LoadedStaticElf) -> i64,
) -> i64 {
    // fcntl runs no F_SETFL on an O_PATH description (`check_fcntl_cmd`,
    // fs/fcntl.c:575-584).
    match fd_status_flags(host_fd) {
        Ok(current) if current & libc::O_PATH != 0 => return negative_errno(libc::EBADF),
        Ok(_) => {}
        Err(error) => return error,
    }
    let reason = match classify_async_admission(state, guest_fd, host_fd) {
        AsyncAdmission::Device => return apply(state),
        AsyncAdmission::Record(record) => match record.with_async_on(|| apply(state)) {
            Ok(result) => return result,
            Err(reason) => reason,
        },
        AsyncAdmission::Outside(reason) => reason,
    };
    // setfl checks the word before it turns the mode on (fs/fcntl.c:58-65):
    // O_DIRECT on a file without FMODE_CAN_ODIRECT is EINVAL, and a
    // character device's or a socket's file never has it, having no
    // `direct_IO` (fs/open.c:960-961). The refusal keeps that answer.
    if flags & libc::O_DIRECT != 0
        && descriptor_stat(host_fd).is_some_and(|stat| {
            matches!(stat.st_mode & libc::S_IFMT, libc::S_IFCHR | libc::S_IFSOCK)
        })
    {
        return negative_errno(libc::EINVAL);
    }
    let current = fd_status_flags(host_fd);
    #[cfg(test)]
    run_one_shot_test_hook(&ASYNC_OBSERVED_TEST_HOOK);
    match current {
        // setfl turns the mode on only through `fasync`, on a change of
        // FASYNC, and writes the rest of SETFL_MASK from the word (fs/fcntl.c
        // 75-85): with the mode on and no other settable flag changing, the
        // call changes nothing. A concurrent outside setter's word then
        // stands, as if this call ran first.
        Ok(current)
            if current & libc::O_ASYNC != 0
                && (current ^ flags) & SETTABLE_BESIDES_ASYNC == 0 =>
        {
            0
        }
        Ok(_) => refuse(state, reason),
        Err(error) => error,
    }
}

#[cfg(test)]
std::thread_local! {
    /// Runs once on the calling thread when a request that would turn the
    /// mode on, outside the domain, has read the host's flags and has yet to
    /// answer: where a process outside the run can change them meanwhile.
    pub(crate) static ASYNC_OBSERVED_TEST_HOOK: OneShotTestHook =
        const { std::cell::RefCell::new(None) };
}
