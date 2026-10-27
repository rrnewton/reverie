/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

// TODO-HUMAN-REVIEW(PR-1015): Review host-backed KVM inotify semantics.
//
// An inotify instance is an ordinary host inotify descriptor in the guest file
// table, as eventfd and timerfd are. Its events come only from the host kernel
// observing filesystem operations, and every filesystem operation the guest
// performs is a host syscall issued synchronously by this executor, so the
// queue is a function of the guest's own operation order. The host kernel
// applies Linux's queueing, coalescing, ordering, read sizing, EINVAL and EFAULT
// rules itself:
//
// - reads go through the same staged host buffers as readv, whose inaccessible
//   guest suffix is PROT_NONE, so the kernel stops and faults at the guest's own
//   boundary and an event that does not fit the count is refused natively; the
//   events it copied before a fault reach the guest even where the count
//   leaves them out (publish_uncounted_prefix, for a readv's later iovec);
// - watches attach to the inode selected by one guest-namespace lookup (held as
//   O_PATH), never to a second lookup of the pathname;
// - FIONREAD returns the host queue's byte count, which is the guest's.
//
// Rename cookies are the host's too: Linux draws IN_MOVED_FROM/IN_MOVED_TO
// cookies from one host-wide counter that every process's renames advance, so
// a Tool that needs reproducible values renumbers them after the read, as it
// must on every backend.
//
// One thing is not left to the host: blocking mode. A blocking read waits for
// an event that only another guest task can produce, in a host wait that a
// scheduling Tool cannot see or order. As for signalfd, creating or switching
// an instance into blocking mode is refused with ENOSYS, and the read path
// refuses a blocking instance as a defensive invariant.

pub(super) fn host_fd_is_inotify(host_fd: RawFd) -> Result<bool, i64> {
    host_fd_mode_is_inotify(host_fd, fd_mode(host_fd)?)
}

/// [`host_fd_is_inotify`] with the descriptor's mode already in hand.
pub(super) fn host_fd_mode_is_inotify(host_fd: RawFd, mode: libc::mode_t) -> Result<bool, i64> {
    // Anonymous-inode descriptors have no file type bits. Only those need the
    // name check, so ordinary files pay one fstat.
    if mode & libc::S_IFMT != 0 {
        return Ok(false);
    }
    let target = canonical_fd_path(host_fd)?;
    Ok(target.as_os_str().as_bytes() == b"anon_inode:inotify")
}

/// Refuse a blocking inotify wait: no event can arrive while the reading task
/// holds its turn, and a host wait would escape Detcore's scheduler.
pub(super) fn ensure_inotify_nonblocking(host_fd: RawFd) -> Result<(), i64> {
    if fd_status_flags(host_fd)? & libc::O_NONBLOCK == 0 {
        return Err(negative_errno(libc::ENOSYS));
    }
    Ok(())
}

pub(super) fn inotify_init1(state: &mut LoadedStaticElf, raw_flags: u64) -> i64 {
    // SYSCALL_DEFINE1(inotify_init1, int, flags) consumes the low int word.
    let flags = raw_flags as libc::c_int;
    if flags & !(libc::IN_CLOEXEC | libc::IN_NONBLOCK) != 0 {
        return negative_errno(libc::EINVAL);
    }
    if flags & libc::IN_NONBLOCK == 0 {
        // See ensure_inotify_nonblocking: blocking waits are unsupported.
        return negative_errno(libc::ENOSYS);
    }
    // Keep the supervisor descriptor private; guest CLOEXEC is modeled
    // separately in the guest file table.
    // SAFETY: inotify_init1 takes only flags and returns a new descriptor.
    let host_fd = unsafe { libc::inotify_init1(flags | libc::IN_CLOEXEC) };
    if host_fd < 0 {
        return io_error(std::io::Error::last_os_error());
    }
    // SAFETY: inotify_init1 returned a new owned descriptor.
    let file = unsafe { std::fs::File::from_raw_fd(host_fd) };
    insert_file_with_flags(state, file, flags & libc::IN_CLOEXEC != 0, None)
}

/// The host kernel's verdict for an inotify_add_watch operand that cannot name
/// a file. Linux checks the mask, then the descriptor and its type, before it
/// copies the pathname, so an empty pathname reaches ENOENT only when both are
/// valid. Nothing is attached.
fn inotify_add_watch_preflight(host_fd: RawFd, mask: u32, valid: libc::c_int) -> Result<(), i64> {
    // SAFETY: the pathname is a live NUL-terminated empty string.
    let result = unsafe { libc::inotify_add_watch(host_fd, c"".as_ptr(), mask) };
    if result >= 0 {
        // An empty pathname never resolves. Do not keep an unexpected watch.
        // SAFETY: result is the watch descriptor just returned for host_fd.
        unsafe { libc::inotify_rm_watch(host_fd, result) };
        return Err(negative_errno(libc::EIO));
    }
    match std::io::Error::last_os_error().raw_os_error() {
        Some(errno) if errno == valid => Ok(()),
        Some(errno) => Err(negative_errno(errno)),
        None => Err(negative_errno(libc::EIO)),
    }
}

pub(super) fn inotify_add_watch(memory: &GuestMemory, state: &LoadedStaticElf, args: &[u64; 6]) -> i64 {
    let guest_fd = args[0] as libc::c_int;
    let mask = args[2] as u32;
    // The mask is validated before the descriptor. Use the host kernel's exact
    // mask rules: with fd -1, a valid mask reaches EBADF.
    if let Err(error) = inotify_add_watch_preflight(-1, mask, libc::EBADF) {
        return error;
    }
    let Some(host_fd) = host_fd(state, guest_fd) else {
        return negative_errno(libc::EBADF);
    };
    // A non-inotify descriptor is EINVAL and an O_PATH descriptor EBADF, both
    // before the pathname is read.
    if let Err(error) = inotify_add_watch_preflight(host_fd, mask, libc::ENOENT) {
        return error;
    }
    let path = match read_c_string(&ProviderMemory(memory), args[1], libc::PATH_MAX as usize) {
        Ok(path) if !path.is_empty() => path,
        Ok(_) => return negative_errno(libc::ENOENT),
        Err(error) => return read_c_string_errno(error),
    };
    // One guest-namespace lookup selects the inode, honoring the guest cwd,
    // IN_DONT_FOLLOW for the final component and the procfs restriction shared
    // by the other metadata operations.
    let held = match open_metadata_path(
        state,
        libc::AT_FDCWD,
        &path,
        mask & libc::IN_DONT_FOLLOW != 0,
    ) {
        Ok(file) => file,
        Err(error) => return error,
    };
    // Attach through the held descriptor's proc link: the kernel's lookup jumps
    // to exactly the held object (the symlink itself under IN_DONT_FOLLOW) and
    // stops there, so a concurrent rename cannot substitute another inode.
    // IN_DONT_FOLLOW would instead select the proc link, so it is not passed.
    // IN_ONLYDIR and the read-permission check still apply to the held object.
    let target = CString::new(format!("/proc/self/fd/{}", held.as_raw_fd()))
        .expect("host descriptor path has no NUL");
    // SAFETY: target is NUL-terminated and resolves only the held descriptor.
    let watch =
        unsafe { libc::inotify_add_watch(host_fd, target.as_ptr(), mask & !libc::IN_DONT_FOLLOW) };
    if watch < 0 {
        io_error(std::io::Error::last_os_error())
    } else {
        i64::from(watch)
    }
}

pub(super) fn inotify_rm_watch(state: &LoadedStaticElf, args: &[u64; 6]) -> i64 {
    let Some(host_fd) = host_fd(state, args[0] as libc::c_int) else {
        return negative_errno(libc::EBADF);
    };
    // The kernel validates the descriptor type and the instance-local watch.
    // SAFETY: host_fd is pinned by the caller's file-table snapshot.
    zero_or_errno(unsafe { libc::inotify_rm_watch(host_fd, args[1] as libc::c_int) })
}
