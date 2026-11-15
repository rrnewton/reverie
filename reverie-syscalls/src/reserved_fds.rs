/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! The one rule for the descriptors a backend keeps for itself, shared by
//! every backend that runs inside the guest process (the in-guest runtime and
//! LiteInst, SaBRe and e9patch's tool host): which syscalls name a descriptor,
//! how the kernel reads a descriptor argument, what a guest `epoll_wait` on a
//! descriptor it does not have returns, how a guest `close_range` closes
//! everything in its range except the reserved descriptors, and how a private
//! copy of a message's control data is kept from carrying a reserved
//! descriptor in `SCM_RIGHTS` data ([`scm_rights_hide_reserved`]).
//!
//! To the guest a reserved descriptor is not open. Each backend decides which
//! numbers it reserves and keeps its own registry of them; this module only
//! answers questions about a syscall and a number, and runs `close_range`
//! through the caller's raw syscall entry, so it allocates nothing and is safe
//! to call while dispatching a guest syscall.

// Numbers with no entry in the aarch64 generic table (`getdents`,
// `signalfd`, and `epoll_wait` exist there only as `getdents64`,
// `signalfd4`, and `epoll_pwait`) are `None` off x86-64. Callers compare
// with `Some(number) == SYS_...`, so absence can never equal a real
// (even invalid, e.g. -1) input number. `fchmodat2` does exist on
// aarch64 (452); libc's musl table has no constant for it yet, so the
// number is named directly there.
#[cfg(target_arch = "x86_64")]
const SYS_GETDENTS: Option<i64> = Some(libc::SYS_getdents);
#[cfg(not(target_arch = "x86_64"))]
const SYS_GETDENTS: Option<i64> = None;
#[cfg(target_arch = "x86_64")]
const SYS_SIGNALFD: Option<i64> = Some(libc::SYS_signalfd);
#[cfg(not(target_arch = "x86_64"))]
const SYS_SIGNALFD: Option<i64> = None;
#[cfg(target_arch = "x86_64")]
const SYS_EPOLL_WAIT: Option<i64> = Some(libc::SYS_epoll_wait);
#[cfg(not(target_arch = "x86_64"))]
const SYS_EPOLL_WAIT: Option<i64> = None;
#[cfg(target_arch = "x86_64")]
const SYS_FCHMODAT2: i64 = libc::SYS_fchmodat2;
#[cfg(not(target_arch = "x86_64"))]
const SYS_FCHMODAT2: i64 = 452;

/// Syscall argument `index` as the kernel reads a descriptor, a `close_range`
/// bound or its flags: the low 32 bits of the register. Comparing the full
/// register would let `fd | 1 << 32` reach a reserved descriptor.
pub fn descriptor_arg(args: &[u64; 6], index: usize) -> u64 {
    u64::from(args[index] as u32)
}

/// Whether syscall `number` with `args` operates on descriptor `fd` through
/// one of its descriptor arguments: reads and writes, fcntl, ioctl, dup, mmap
/// of a file, splice, sendfile, epoll_ctl, perf_event_open's group leader and
/// cgroup, and the other descriptor calls listed below. It does not cover the
/// socket calls ([`socket_call_names_descriptor`]), `close`, `close_range` or
/// epoll readiness ([`epoll_wait_result`]).
pub fn names_descriptor(number: i64, args: &[u64; 6], fd: u64) -> bool {
    let is = |index| descriptor_arg(args, index) == fd;
    match number {
        // Numbers absent from this architecture's table (see the Option
        // constants above) name the descriptor only where they exist.
        n if Some(n) == SYS_GETDENTS || Some(n) == SYS_SIGNALFD => is(0),
        libc::SYS_read
        | libc::SYS_readv
        | libc::SYS_pread64
        | libc::SYS_preadv
        | libc::SYS_preadv2
        | libc::SYS_write
        | libc::SYS_writev
        | libc::SYS_pwrite64
        | libc::SYS_pwritev
        | libc::SYS_pwritev2
        | libc::SYS_vmsplice
        | libc::SYS_dup
        // Operations that change, lock or describe the open file itself.
        | libc::SYS_ftruncate
        | libc::SYS_fallocate
        | libc::SYS_fchmod
        | libc::SYS_fchown
        | libc::SYS_fsetxattr
        | libc::SYS_fremovexattr
        | libc::SYS_fgetxattr
        | libc::SYS_flistxattr
        | libc::SYS_flock
        | libc::SYS_fsync
        | libc::SYS_fdatasync
        | libc::SYS_sync_file_range
        | libc::SYS_lseek
        | libc::SYS_fstat
        | libc::SYS_fstatfs
        | libc::SYS_fadvise64
        | libc::SYS_readahead
        | libc::SYS_getdents64
        | libc::SYS_syncfs
        | libc::SYS_fchdir
        // Descriptors of special kinds: a reserved number is not one, and to
        // the guest it is not open at all.
        | libc::SYS_signalfd4
        | libc::SYS_timerfd_settime
        | libc::SYS_timerfd_gettime
        | libc::SYS_inotify_add_watch
        | libc::SYS_inotify_rm_watch
        | libc::SYS_fanotify_mark
        | libc::SYS_setns
        | libc::SYS_pidfd_send_signal
        | libc::SYS_process_madvise
        | libc::SYS_process_mrelease
        | libc::SYS_finit_module
        | libc::SYS_fsmount
        | libc::SYS_quotactl_fd
        | libc::SYS_landlock_add_rule
        | libc::SYS_landlock_restrict_self => is(0),
        // fcntl acts on its descriptor, and F_DUPFD_QUERY compares that open
        // file with a second descriptor named by value in argument 2
        // (fs/fcntl.c:396-412, 463-464). Linux reads the command and that
        // descriptor as ints.
        libc::SYS_fcntl => is(0) || (args[1] as u32 == F_DUPFD_QUERY && is(2)),
        // ioctl acts on its descriptor, and three commands name a second one
        // by value in argument 2: PERF_EVENT_IOC_SET_OUTPUT, the event whose
        // buffer takes this one's output (-1 for none, which no descriptor
        // is); PERF_EVENT_IOC_SET_BPF, a BPF program; and FICLONE, the file
        // whose whole contents are cloned into this one (fs/ioctl.c:230-238,
        // 540). Linux reads the command as 32 bits and the descriptor as an
        // int.
        libc::SYS_ioctl => {
            is(0)
                || (matches!(
                    args[1] as u32,
                    PERF_EVENT_IOC_SET_OUTPUT | PERF_EVENT_IOC_SET_BPF | FICLONE
                ) && is(2))
        }
        // perf_event_open(attr, pid, cpu, group_fd, flags) looks up its group
        // leader unless group_fd is -1, and under PERF_FLAG_PID_CGROUP its pid
        // argument is a cgroup directory's descriptor. With only the flags
        // Linux 7.1.3 defines, a cgroup event with no pid or no cpu fails with
        // EINVAL before either lookup (kernel/events/core.c:13914-13915), so
        // Linux answers that call, whatever group_fd names. A flag 7.1.3 does
        // not define fails there with EINVAL too (:13863-13864), but a later
        // kernel may give it a meaning, so with one, group_fd or pid naming
        // the descriptor refuses the call: EBADF where 7.1.3 says EINVAL.
        libc::SYS_perf_event_open => {
            let flags = args[4];
            let cgroup = flags & PERF_FLAG_PID_CGROUP != 0;
            let minus_one = |index: usize| args[index] as u32 == u32::MAX;
            if flags & !PERF_FLAGS_DEFINED_AT_LINUX_7_1_3 != 0 {
                is(1) || (!minus_one(3) && is(3))
            } else if cgroup && (minus_one(1) || minus_one(2)) {
                false
            } else {
                (!minus_one(3) && is(3)) || (cgroup && is(1))
            }
        }
        libc::SYS_sendfile => is(0) || is(1),
        libc::SYS_epoll_ctl => is(0) || is(2),
        // waitid(P_PIDFD, fd, ...).
        libc::SYS_waitid => args[0] as u32 == 3 && is(1),
        // A file mapping of the descriptor.
        libc::SYS_mmap => args[3] & libc::MAP_ANONYMOUS as u64 == 0 && is(4),
        // dup2 has no aarch64 number (dup3 only there).
        #[cfg(target_arch = "x86_64")]
        libc::SYS_dup2 | libc::SYS_dup3 => is(0) || is(1),
        #[cfg(not(target_arch = "x86_64"))]
        libc::SYS_dup3 => is(0) || is(1),
        libc::SYS_splice | libc::SYS_copy_file_range => is(0) || is(2),
        libc::SYS_tee => is(0) || is(1),
        // The pidfd, and the number it copies from the target's table, which
        // aliases a reserved descriptor when the target is this process or a
        // fork of it.
        libc::SYS_pidfd_getfd => is(0) || is(1),
        // Calls whose descriptor argument is always one.
        libc::SYS_io_uring_enter | libc::SYS_io_uring_register | libc::SYS_fsconfig => is(0),
        // `*at` calls that act on the directory descriptor itself when given
        // `AT_EMPTY_PATH` (glibc's `fstat` is `newfstatat(fd, "", ...,
        // AT_EMPTY_PATH)`), or `utimensat` with a null path. Without either
        // the descriptor is used only for a relative path, which would take a
        // guest-memory read to tell from an absolute one, so those calls are
        // not checked and run as Linux runs them.
        libc::SYS_newfstatat | SYS_FCHMODAT2 => empty_path(args[3]) && is(0),
        libc::SYS_statx | libc::SYS_open_tree => empty_path(args[2]) && is(0),
        libc::SYS_execveat
        | libc::SYS_fchownat
        | libc::SYS_linkat
        | libc::SYS_name_to_handle_at => empty_path(args[4]) && is(0),
        libc::SYS_faccessat2 => empty_path(args[3]) && is(0),
        libc::SYS_utimensat => args[1] == 0 && is(0),
        // fspick(FSPICK_EMPTY_PATH); move_mount(MOVE_MOUNT_F_EMPTY_PATH) for
        // its source and MOVE_MOUNT_T_EMPTY_PATH for its target.
        libc::SYS_fspick => args[2] & FSPICK_EMPTY_PATH != 0 && is(0),
        libc::SYS_move_mount => {
            (args[4] & MOVE_MOUNT_F_EMPTY_PATH != 0 && is(0))
                || (args[4] & MOVE_MOUNT_T_EMPTY_PATH != 0 && is(2))
        }
        _ => false,
    }
}

const FSPICK_EMPTY_PATH: u64 = 0x8;
const MOVE_MOUNT_F_EMPTY_PATH: u64 = 0x4;
const MOVE_MOUNT_T_EMPTY_PATH: u64 = 0x40;
/// `PERF_EVENT_IOC_SET_OUTPUT`, `_IO('$', 5)`.
const PERF_EVENT_IOC_SET_OUTPUT: u32 = 0x2405;
/// `PERF_EVENT_IOC_SET_BPF`, `_IOW('$', 8, __u32)`.
const PERF_EVENT_IOC_SET_BPF: u32 = 0x4004_2408;
/// `PERF_FLAG_PID_CGROUP`.
const PERF_FLAG_PID_CGROUP: u64 = 1 << 2;
/// The `perf_event_open` flags Linux 7.1.3 defines, its `PERF_FLAG_ALL`
/// (kernel/events/core.c:427; include/uapi/linux/perf_event.h:1314-1317):
/// `FD_NO_GROUP`, `FD_OUTPUT`, `PID_CGROUP` and `FD_CLOEXEC`. A later kernel
/// that defines another flag needs this list, and the rule, reviewed.
const PERF_FLAGS_DEFINED_AT_LINUX_7_1_3: u64 = 0xf;
/// `FICLONE`, `_IOW(0x94, 9, int)`.
const FICLONE: u32 = 0x4004_9409;
/// `F_DUPFD_QUERY`, `F_LINUX_SPECIFIC_BASE + 3`.
const F_DUPFD_QUERY: u32 = 1027;
/// `__X32_SYSCALL_BIT`: an x32 call is the x86-64 number with this bit set.
#[cfg(target_arch = "x86_64")]
const X32_SYSCALL_BIT: u32 = 0x4000_0000;
/// `io_uring_setup`, `io_uring_enter` and `io_uring_register`, the same
/// numbers on every architecture.
const IO_URING_SYSCALLS: core::ops::RangeInclusive<u32> = 425..=427;
/// kcmp's `KCMP_FILE`: compare two tasks' descriptors.
pub const KCMP_FILE: u32 = 0;
/// kcmp's `KCMP_EPOLL_TFD`: whether a file is a target of an epoll.
pub const KCMP_EPOLL_TFD: u32 = 7;
/// The descriptor number put in place of a hidden one: above every possible
/// `max_fds` (fs/file.c:100-101), so it is never open.
pub const CLOSED_DESCRIPTOR: u32 = u32::MAX;
/// Size of `struct file_clone_range`: `src_fd` (s64), then `src_offset`,
/// `src_length` and `dest_offset` (u64).
pub const FILE_CLONE_RANGE_SIZE: usize = 32;
/// Size of `struct file_dedupe_range`'s header: `src_offset`, `src_length`
/// (u64), `dest_count`, `reserved1` (u16) and `reserved2` (u32).
pub const DEDUPE_HEADER_SIZE: usize = 24;
/// Offset of `dest_count` in that header.
pub const DEDUPE_COUNT_OFFSET: usize = 16;
/// Size of one `struct file_dedupe_range_info`: `dest_fd` (s64),
/// `dest_offset`, `bytes_deduped` (u64), `status` (s32) and `reserved`
/// (u32).
pub const DEDUPE_INFO_SIZE: usize = 32;
/// The `dest_count` forwarded when a request does not fit the runtime's
/// buffer: 24 + 32 × 65535 = 2,097,144 bytes, more than any x86-64 or arm64
/// page, so Linux answers ENOMEM itself (fs/ioctl.c:428-432) after reading
/// only the count.
pub const DEDUPE_OVERSIZE_COUNT: u16 = u16::MAX;
/// Size of `struct kcmp_epoll_slot`: `efd`, `tfd` and `toff` (u32).
pub const KCMP_EPOLL_SLOT_SIZE: usize = 12;

/// Whether `*at` flags `flags` hold `AT_EMPTY_PATH`.
fn empty_path(flags: u64) -> bool {
    flags & libc::AT_EMPTY_PATH as u64 != 0
}

/// Whether syscall `number` shuts down, configures, connects, or sends or
/// receives on socket `fd`. Backends check these for guest syscalls only, when
/// their own client uses the reserved socket through the same calls.
pub fn socket_call_names_descriptor(number: i64, args: &[u64; 6], fd: u64) -> bool {
    match number {
        libc::SYS_shutdown
        | libc::SYS_getsockopt
        | libc::SYS_setsockopt
        | libc::SYS_sendto
        | libc::SYS_recvfrom
        | libc::SYS_sendmsg
        | libc::SYS_recvmsg
        | libc::SYS_sendmmsg
        | libc::SYS_recvmmsg
        | libc::SYS_connect
        | libc::SYS_bind
        | libc::SYS_listen
        | libc::SYS_accept
        | libc::SYS_accept4
        | libc::SYS_getsockname
        | libc::SYS_getpeername => descriptor_arg(args, 0) == fd,
        _ => false,
    }
}

/// The result Linux gives `epoll_wait`, `epoll_pwait` or `epoll_pwait2` on
/// `fd`, a number the caller does not have open: `EBADF`, after Linux's own
/// `EINVAL` check of the event count. `None` for any other call. (`poll`,
/// `ppoll`, `select` and `pselect6` name descriptors inside guest memory, and
/// `*at` calls use their directory argument only for some paths; checking them
/// would cost correct programs' calls a guest-memory read, so a buggy program
/// that passes a reserved number there is a known limit.)
pub fn epoll_wait_result(number: i64, args: &[u64; 6], fd: u64) -> Option<i64> {
    let is_epoll_wait = number == libc::SYS_epoll_pwait
        || number == libc::SYS_epoll_pwait2
        || Some(number) == SYS_EPOLL_WAIT;
    if is_epoll_wait && descriptor_arg(args, 0) == fd {
        let events = args[2] as i32;
        let most = i32::MAX / core::mem::size_of::<libc::epoll_event>() as i32;
        Some(-i64::from(if events <= 0 || events > most {
            libc::EINVAL
        } else {
            libc::EBADF
        }))
    } else {
        None
    }
}

/// The size of a `cmsghdr` on the 64-bit ABIs: `cmsg_len` (a `size_t`), then
/// `cmsg_level` and `cmsg_type`.
const CMSGHDR_SIZE: usize = 16;

/// In `control`, a private copy of one message's control data that the kernel
/// will read in place of the caller's, replaces with -1 every descriptor an
/// `SCM_RIGHTS` message names that is in `reserved`, and returns how many it
/// replaced. Linux then fails the whole call with EBADF when its walk reaches
/// that descriptor, as for any number the caller does not have open
/// (`scm_fp_copy`), after every check it makes first and before anything of
/// the message is queued.
///
/// The walk is Linux's (`for_each_cmsghdr` and `CMSG_OK` in `__scm_send`): the
/// first header when the data holds a whole one, then each next header
/// `CMSG_ALIGN(cmsg_len)` further on while a whole header fits, stopping at a
/// header that fails `CMSG_OK`, where Linux refuses the message with EINVAL
/// before it reads any later descriptor. Headers of another level or type are
/// left alone: Linux refuses an unknown `SOL_SOCKET` type itself, and other
/// levels belong to the protocol. A negative number is left alone too; Linux
/// answers it with EBADF. Allocates nothing.
pub fn scm_rights_hide_reserved(control: &mut [u8], reserved: &[u64]) -> usize {
    let length = control.len();
    let mut offset = 0;
    let mut hidden = 0;
    while length - offset >= CMSGHDR_SIZE {
        let header = &control[offset..offset + CMSGHDR_SIZE];
        let cmsg_len = u64::from_ne_bytes(header[..8].try_into().unwrap());
        let level = i32::from_ne_bytes(header[8..12].try_into().unwrap());
        let kind = i32::from_ne_bytes(header[12..16].try_into().unwrap());
        // CMSG_OK: at least a header, and no more than the data left.
        if cmsg_len < CMSGHDR_SIZE as u64 || cmsg_len > (length - offset) as u64 {
            break;
        }
        let cmsg_len = cmsg_len as usize;
        if level == libc::SOL_SOCKET && kind == libc::SCM_RIGHTS {
            for index in 0..(cmsg_len - CMSGHDR_SIZE) / 4 {
                let at = offset + CMSGHDR_SIZE + 4 * index;
                let fd = i32::from_ne_bytes(control[at..at + 4].try_into().unwrap());
                if fd >= 0 && reserved.contains(&(fd as u64)) {
                    control[at..at + 4].copy_from_slice(&(-1_i32).to_ne_bytes());
                    hidden += 1;
                }
            }
        }
        // CMSG_ALIGN rounds to the size of a long. The next header must fit
        // whole, which the loop condition checks once offset is in range.
        offset += (cmsg_len + 7) & !7;
        if offset > length {
            break;
        }
    }
    hidden
}

/// Whether a `close_range` with `args` covers one of `reserved`.
pub fn close_range_covers(args: &[u64; 6], reserved: &[u64]) -> bool {
    let (first, last) = (descriptor_arg(args, 0), descriptor_arg(args, 1));
    reserved.iter().any(|&fd| first <= fd && fd <= last)
}

/// The ranges of `[first, last]` that lie between the descriptors of
/// `reserved`, which must be sorted ascending (duplicates are allowed), in
/// order. Empty when `first > last`.
pub fn close_range_gaps(first: u32, last: u32, reserved: &[u64]) -> CloseRangeGaps<'_> {
    CloseRangeGaps {
        next: Some(u64::from(first)),
        last: u64::from(last),
        reserved,
    }
}

/// The iterator [`close_range_gaps`] returns.
pub struct CloseRangeGaps<'a> {
    /// The lowest descriptor not yet covered, or `None` past `u32::MAX`.
    next: Option<u64>,
    last: u64,
    reserved: &'a [u64],
}

impl Iterator for CloseRangeGaps<'_> {
    type Item = (u32, u32);

    fn next(&mut self) -> Option<(u32, u32)> {
        loop {
            let start = self.next?;
            if start > self.last {
                return None;
            }
            match self.reserved.split_first() {
                Some((&fd, rest)) if fd <= self.last => {
                    self.reserved = rest;
                    if fd < start {
                        continue;
                    }
                    self.next = (fd < u64::from(u32::MAX)).then_some(fd + 1);
                    if start < fd {
                        return Some((start as u32, (fd - 1) as u32));
                    }
                }
                _ => {
                    self.next = None;
                    return Some((start as u32, self.last as u32));
                }
            }
        }
    }
}

/// Runs a `close_range` with `args` without closing any of `reserved` (sorted
/// ascending), through `syscall`, the caller's raw syscall entry, which takes a
/// syscall number and its six arguments and returns the kernel's result. The
/// checks come first, in the kernel's order: flags other than
/// `CLOSE_RANGE_UNSHARE` and `CLOSE_RANGE_CLOEXEC`, then `first > last`, both
/// `EINVAL`. `CLOSE_RANGE_UNSHARE` unshares the descriptor table once, and then
/// each gap between the reserved descriptors is closed with the remaining
/// flags. Returns 0, or the first error.
///
/// # Safety
///
/// `args` must be a `close_range` the guest asked for, run in the guest's own
/// descriptor table: every descriptor in its range other than `reserved` is
/// closed. The caller must hold no live owner or borrow (`OwnedFd`, `File`,
/// `BorrowedFd` and the like) of a descriptor the call closes.
pub unsafe fn close_range_sparing(
    args: &[u64; 6],
    reserved: &[u64],
    mut syscall: impl FnMut(i64, [u64; 6]) -> i64,
) -> i64 {
    const CLOSE_RANGE_UNSHARE: u64 = 1 << 1;
    const CLOSE_RANGE_CLOEXEC: u64 = 1 << 2;

    let first = descriptor_arg(args, 0);
    let last = descriptor_arg(args, 1);
    let mut flags = descriptor_arg(args, 2);
    if flags & !(CLOSE_RANGE_UNSHARE | CLOSE_RANGE_CLOEXEC) != 0 || first > last {
        return -i64::from(libc::EINVAL);
    }
    if flags & CLOSE_RANGE_UNSHARE != 0 {
        let result = syscall(libc::SYS_unshare, [libc::CLONE_FILES as u64, 0, 0, 0, 0, 0]);
        if result < 0 {
            return result;
        }
        flags &= !CLOSE_RANGE_UNSHARE;
    }
    for (gap_first, gap_last) in close_range_gaps(first as u32, last as u32, reserved) {
        let result = syscall(
            libc::SYS_close_range,
            [u64::from(gap_first), u64::from(gap_last), flags, 0, 0, 0],
        );
        if result < 0 {
            return result;
        }
    }
    0
}

/// Whether syscall `number` is one of io_uring's three entries. Their
/// submission entries, fixed-file tables and registered descriptors reach
/// descriptors through lookups no syscall argument shows, so a backend that
/// keeps descriptors from its guest refuses them outright. Linux reads the
/// number as 32 bits (arch/x86/entry/syscall_64.c:87, :59;
/// arch/arm64/kernel/syscall.c:62), and on x86-64 an x32 call is the same
/// number with `__X32_SYSCALL_BIT` set (arch/x86/entry/syscall_64.c:76).
pub fn unchecked_descriptor_syscall(number: i64) -> bool {
    let number = number as u32;
    #[cfg(target_arch = "x86_64")]
    let number = number & !X32_SYSCALL_BIT;
    IO_URING_SYSCALLS.contains(&number)
}

/// Whether the descriptor value `fd`, as the kernel reads it (32 bits), is
/// one of `reserved`. Every operand check below goes through here, descriptor
/// 0 included: a runtime descriptor that could not be moved keeps the number
/// it was created at, whatever that is.
fn is_reserved(reserved: &[u64], fd: u32) -> bool {
    reserved.contains(&u64::from(fd))
}

fn read_i64(bytes: &[u8], at: usize) -> Option<i64> {
    Some(i64::from_ne_bytes(bytes.get(at..at + 8)?.try_into().ok()?))
}

fn write_i64(bytes: &mut [u8], at: usize, value: i64) -> Option<()> {
    bytes
        .get_mut(at..at + 8)?
        .copy_from_slice(&value.to_ne_bytes());
    Some(())
}

/// Hides a reserved `src_fd` in a runtime copy of FICLONERANGE's operand.
/// Linux passes `src_fd` on as an unsigned long and looks it up as an
/// unsigned int (fs/ioctl.c:233, 257), so a reserved low 32 bits makes it -1,
/// which no descriptor is. Returns whether it hid one.
pub fn clone_range_hide_reserved(
    range: &mut [u8; FILE_CLONE_RANGE_SIZE],
    reserved: &[u64],
) -> bool {
    let Some(src_fd) = read_i64(range, 0) else {
        return false;
    };
    if !is_reserved(reserved, src_fd as u32) {
        return false;
    }
    write_i64(range, 0, -1).is_some()
}

/// How the runtime passes a FIDEDUPERANGE request of `count` entries on to
/// Linux, given a buffer of `capacity` bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DedupePlan {
    /// Copy the header and `count` entries, `size` bytes, into the buffer.
    Copy {
        /// The header and `count` entries, in bytes.
        size: usize,
    },
    /// The request does not fit: forward a header holding
    /// [`DEDUPE_OVERSIZE_COUNT`], for Linux's own ENOMEM.
    OversizeHeader,
}

/// The plan for a request whose `dest_count` read `count`.
pub fn dedupe_plan(count: u16, capacity: usize) -> DedupePlan {
    let size = DEDUPE_HEADER_SIZE + DEDUPE_INFO_SIZE * usize::from(count);
    if size <= capacity {
        DedupePlan::Copy { size }
    } else {
        DedupePlan::OversizeHeader
    }
}

/// Pins `count`, the `dest_count` read once from the guest, into a runtime
/// copy of FIDEDUPERANGE's operand, as Linux pins it in its own copy
/// (fs/ioctl.c:441). It then hides every reserved `dest_fd` among entries
/// `0..count`: Linux looks each up as an unsigned int (fs/remap_range.c:539),
/// so a reserved low 32 bits makes it -1. `originals[i]` receives entry i's
/// `dest_fd` before hiding, for [`dedupe_restore`].
///
/// Returns how many entries it hid, or None, changing nothing, when `copy`
/// is shorter than the header and `count` entries or `originals` is shorter
/// than `count`.
pub fn dedupe_hide_reserved(
    copy: &mut [u8],
    count: u16,
    reserved: &[u64],
    originals: &mut [i64],
) -> Option<usize> {
    let entries = usize::from(count);
    if copy.len() < DEDUPE_HEADER_SIZE + DEDUPE_INFO_SIZE * entries || originals.len() < entries
    {
        return None;
    }
    copy[DEDUPE_COUNT_OFFSET..DEDUPE_COUNT_OFFSET + 2].copy_from_slice(&count.to_ne_bytes());
    let mut hidden = 0;
    for (index, original) in originals[..entries].iter_mut().enumerate() {
        let at = DEDUPE_HEADER_SIZE + DEDUPE_INFO_SIZE * index;
        let dest_fd = read_i64(copy, at)?;
        *original = dest_fd;
        if is_reserved(reserved, dest_fd as u32) {
            write_i64(copy, at, -1)?;
            hidden += 1;
        }
    }
    Some(hidden)
}

/// Puts back the `dest_fd` values [`dedupe_hide_reserved`] saved, so that
/// the guest's struct gets its own values beside Linux's per-entry results,
/// as Linux writes back what it read (fs/ioctl.c:446). Returns None,
/// changing nothing, under the same length conditions.
pub fn dedupe_restore(copy: &mut [u8], count: u16, originals: &[i64]) -> Option<()> {
    let entries = usize::from(count);
    if copy.len() < DEDUPE_HEADER_SIZE + DEDUPE_INFO_SIZE * entries || originals.len() < entries
    {
        return None;
    }
    for (index, original) in originals[..entries].iter().enumerate() {
        write_i64(copy, DEDUPE_HEADER_SIZE + DEDUPE_INFO_SIZE * index, *original)?;
    }
    Some(())
}

/// kcmp's arguments with each reserved descriptor index replaced by
/// [`CLOSED_DESCRIPTOR`], whatever task the index names, or None when there
/// is none. Linux reads `type` as an int (kernel/kcmp.c:135) and an index as
/// an unsigned int (:62). KCMP_FILE looks up `idx1` and `idx2` (:170-174);
/// KCMP_EPOLL_TFD looks up `idx1` only (:109), and its last argument is a
/// pointer to the slot [`kcmp_slot_hide_reserved`] checks.
pub fn kcmp_hidden_indices(args: &[u64; 6], reserved: &[u64]) -> Option<[u64; 6]> {
    let indices: &[usize] = match args[2] as u32 {
        KCMP_FILE => &[3, 4],
        KCMP_EPOLL_TFD => &[3],
        _ => return None,
    };
    let mut hidden = *args;
    let mut any = false;
    for &index in indices {
        if is_reserved(reserved, args[index] as u32) {
            hidden[index] = u64::from(CLOSED_DESCRIPTOR);
            any = true;
        }
    }
    any.then_some(hidden)
}

/// Hides a reserved `efd` in a runtime copy of KCMP_EPOLL_TFD's slot, which
/// Linux reads (kernel/kcmp.c:106-107) and whose `efd` it looks up (:113).
/// `tfd` and `toff` are an epoll key, not a descriptor lookup. Returns
/// whether it hid one.
pub fn kcmp_slot_hide_reserved(slot: &mut [u8; KCMP_EPOLL_SLOT_SIZE], reserved: &[u64]) -> bool {
    let efd = u32::from_ne_bytes([slot[0], slot[1], slot[2], slot[3]]);
    if !is_reserved(reserved, efd) {
        return false;
    }
    slot[..4].copy_from_slice(&CLOSED_DESCRIPTOR.to_ne_bytes());
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gaps(first: u32, last: u32, reserved: &[u64]) -> Vec<(u32, u32)> {
        close_range_gaps(first, last, reserved).collect()
    }

    #[test]
    fn close_range_gaps_skip_every_reserved_descriptor() {
        assert_eq!(
            gaps(10, 20, &[12, 15, 20]),
            vec![(10, 11), (13, 14), (16, 19)]
        );
        assert_eq!(gaps(10, 12, &[9, 13]), vec![(10, 12)]);
        assert_eq!(gaps(10, 12, &[10, 10, 11]), vec![(12, 12)]);
        assert_eq!(
            gaps(0, u32::MAX, &[1024]),
            vec![(0, 1023), (1025, u32::MAX)]
        );
        assert!(gaps(u32::MAX, u32::MAX, &[u64::from(u32::MAX)]).is_empty());
        assert!(gaps(5, 4, &[]).is_empty());
        assert_eq!(gaps(3, 3, &[]), vec![(3, 3)]);
    }

    #[test]
    fn a_descriptor_argument_is_its_low_32_bits() {
        let args = [1024 | (1 << 32), 0, 0, 0, 0, 0];
        assert_eq!(descriptor_arg(&args, 0), 1024);
        assert!(names_descriptor(libc::SYS_read, &args, 1024));
        assert!(socket_call_names_descriptor(libc::SYS_sendto, &args, 1024));
    }

    /// Control data built from (level, type, payload words) entries, each
    /// header's `cmsg_len` covering its payload and each entry starting
    /// `CMSG_ALIGN`ed, as a caller lays it out.
    fn control_data(entries: &[(i32, i32, &[i32])]) -> Vec<u8> {
        let mut bytes = Vec::new();
        for &(level, kind, words) in entries {
            let start = bytes.len();
            bytes.extend_from_slice(&((CMSGHDR_SIZE + 4 * words.len()) as u64).to_ne_bytes());
            bytes.extend_from_slice(&level.to_ne_bytes());
            bytes.extend_from_slice(&kind.to_ne_bytes());
            for word in words {
                bytes.extend_from_slice(&word.to_ne_bytes());
            }
            bytes.resize(start + ((bytes.len() - start + 7) & !7), 0);
        }
        bytes
    }

    fn words_at(control: &[u8], offset: usize, count: usize) -> Vec<i32> {
        (0..count)
            .map(|index| {
                let at = offset + 4 * index;
                i32::from_ne_bytes(control[at..at + 4].try_into().unwrap())
            })
            .collect()
    }

    #[test]
    fn scm_rights_hides_only_reserved_descriptors() {
        let (sol, rights) = (libc::SOL_SOCKET, libc::SCM_RIGHTS);
        let mut control = control_data(&[(sol, rights, &[3, 1025, 4, 1026])]);
        assert_eq!(scm_rights_hide_reserved(&mut control, &[1025, 1026]), 2);
        assert_eq!(words_at(&control, CMSGHDR_SIZE, 4), [3, -1, 4, -1]);
        // Nothing reserved named: unchanged.
        let mut clean = control_data(&[(sol, rights, &[3, 4])]);
        let before = clean.clone();
        assert_eq!(scm_rights_hide_reserved(&mut clean, &[1025]), 0);
        assert_eq!(clean, before);
        // A negative number is Linux's to refuse.
        let mut negative = control_data(&[(sol, rights, &[-5])]);
        assert_eq!(scm_rights_hide_reserved(&mut negative, &[u64::MAX]), 0);
        assert_eq!(words_at(&negative, CMSGHDR_SIZE, 1), [-5]);
    }

    #[test]
    fn other_levels_and_types_are_left_alone() {
        // SCM_CREDENTIALS (2) whose words equal a reserved number, and a
        // protocol level's option: neither is SCM_RIGHTS data.
        let mut control = control_data(&[
            (libc::SOL_SOCKET, libc::SCM_CREDENTIALS, &[1025, 1025, 1025]),
            (libc::IPPROTO_IP, libc::SCM_RIGHTS, &[1025]),
            (libc::SOL_SOCKET, libc::SCM_RIGHTS, &[1025]),
        ]);
        assert_eq!(scm_rights_hide_reserved(&mut control, &[1025]), 1);
        assert_eq!(words_at(&control, CMSGHDR_SIZE, 3), [1025, 1025, 1025]);
        assert_eq!(words_at(&control, 32 + CMSGHDR_SIZE, 1), [1025]);
        assert_eq!(words_at(&control, 56 + CMSGHDR_SIZE, 1), [-1]);
    }

    #[test]
    fn the_walk_steps_by_cmsg_align() {
        // One descriptor makes cmsg_len 20; the next header starts at 24.
        let mut control = control_data(&[
            (libc::SOL_SOCKET, libc::SCM_RIGHTS, &[7]),
            (libc::SOL_SOCKET, libc::SCM_RIGHTS, &[1025, 8]),
        ]);
        assert_eq!(control.len(), 24 + 24);
        assert_eq!(scm_rights_hide_reserved(&mut control, &[1025]), 1);
        assert_eq!(words_at(&control, 24 + CMSGHDR_SIZE, 2), [-1, 8]);
    }

    #[test]
    fn the_walk_stops_where_linux_refuses_a_header() {
        let mut control = control_data(&[
            (libc::SOL_SOCKET, libc::SCM_RIGHTS, &[1025]),
            (libc::SOL_SOCKET, libc::SCM_RIGHTS, &[1025]),
        ]);
        // The second header claims more than the data holds (CMSG_OK fails):
        // Linux refuses there with EINVAL, so its descriptor is never read.
        let second = 24;
        control[second..second + 8].copy_from_slice(&64_u64.to_ne_bytes());
        assert_eq!(scm_rights_hide_reserved(&mut control, &[1025]), 1);
        assert_eq!(words_at(&control, second + CMSGHDR_SIZE, 1), [1025]);
        // A cmsg_len shorter than a header fails CMSG_OK too.
        let mut short = control_data(&[(libc::SOL_SOCKET, libc::SCM_RIGHTS, &[1025])]);
        short[..8].copy_from_slice(&8_u64.to_ne_bytes());
        assert_eq!(scm_rights_hide_reserved(&mut short, &[1025]), 0);
        // Data shorter than one header holds no header.
        let mut tiny = vec![0_u8; CMSGHDR_SIZE - 1];
        assert_eq!(scm_rights_hide_reserved(&mut tiny, &[0]), 0);
        // A huge cmsg_len never wraps the offset.
        let mut huge = control_data(&[(libc::SOL_SOCKET, libc::SCM_RIGHTS, &[1025])]);
        huge[..8].copy_from_slice(&u64::MAX.to_ne_bytes());
        assert_eq!(scm_rights_hide_reserved(&mut huge, &[1025]), 0);
    }

    #[test]
    fn a_perf_event_names_its_group_leader_and_cgroup() {
        let open = libc::SYS_perf_event_open;
        let none = u64::from(u32::MAX);
        let cgroup = PERF_FLAG_PID_CGROUP;
        let defined = PERF_FLAGS_DEFINED_AT_LINUX_7_1_3;
        // perf_event_open(attr, pid 0, cpu -1, group_fd 7, flags).
        assert!(names_descriptor(open, &[0, 0, none, 7, 0, 0], 7));
        // Every defined flag but PID_CGROUP keeps the leader named;
        // PERF_FLAG_FD_OUTPUT sends the new event's output to the leader's
        // buffer. All four, in a cgroup shape Linux accepts (pid a cgroup
        // directory's descriptor, cpu 0), name it too.
        assert!(names_descriptor(
            open,
            &[0, 0, none, 7, defined & !cgroup, 0],
            7
        ));
        assert!(names_descriptor(open, &[0, 5, 0, 7, defined, 0], 7));
        // Linux reads group_fd as an int.
        assert!(names_descriptor(open, &[0, 0, none, 7 | 1 << 32, 0, 0], 7));
        // No group: not named.
        assert!(!names_descriptor(open, &[0, 0, none, none, 0, 0], 7));
        assert!(!names_descriptor(open, &[0, 0, none, u64::MAX, 0, 0], 7));
        // Under PERF_FLAG_PID_CGROUP the pid argument is a cgroup directory.
        assert!(names_descriptor(open, &[0, 7, 0, none, cgroup, 0], 7));
        assert!(names_descriptor(
            open,
            &[0, 7 | 1 << 32, 0, none, cgroup, 0],
            7
        ));
        // Without the flag it is a pid.
        assert!(!names_descriptor(open, &[0, 7, 0, none, 0, 0], 7));
        // The attr pointer and the cpu are never descriptors.
        assert!(!names_descriptor(open, &[7, 0, 7, none, 0, 0], 7));
    }

    /// A cgroup event with no pid or no cpu fails with EINVAL before Linux
    /// looks up either descriptor (kernel/events/core.c:13914-13915), so it is
    /// left to Linux even when group_fd or the cgroup names a reserved number.
    #[test]
    fn a_cgroup_event_linux_refuses_first_is_left_to_linux() {
        let open = libc::SYS_perf_event_open;
        let none = u64::from(u32::MAX);
        let cgroup = PERF_FLAG_PID_CGROUP;
        // Both register spellings of int -1.
        for minus_one in [none, u64::MAX] {
            // No cpu, with a reserved group leader (pid 0 or a cgroup).
            assert!(!names_descriptor(open, &[0, 0, minus_one, 7, cgroup, 0], 7));
            assert!(!names_descriptor(open, &[0, 5, minus_one, 7, cgroup, 0], 7));
            // No pid, with a reserved group leader.
            assert!(!names_descriptor(open, &[0, minus_one, 0, 7, cgroup, 0], 7));
            // No cpu, with the cgroup itself reserved, with and without a leader.
            assert!(!names_descriptor(
                open,
                &[0, 7, minus_one, none, cgroup, 0],
                7
            ));
            assert!(!names_descriptor(open, &[0, 7, minus_one, 7, cgroup, 0], 7));
            // Every other defined flag too.
            let all = PERF_FLAGS_DEFINED_AT_LINUX_7_1_3;
            assert!(!names_descriptor(open, &[0, 0, minus_one, 7, all, 0], 7));
        }
        // High register bits do not change group_fd.
        assert!(!names_descriptor(
            open,
            &[0, 0, none, 7 | 1 << 32, cgroup, 0],
            7
        ));
    }

    /// A flag Linux 7.1.3 does not define fails there with EINVAL before
    /// either lookup, but a later kernel may define it. So with one, a
    /// reserved number in group_fd or pid refuses the call (EBADF, a different
    /// errno), even in a shape the defined flags leave to Linux.
    #[test]
    fn an_undefined_perf_flag_with_a_reserved_number_is_refused() {
        let open = libc::SYS_perf_event_open;
        let none = u64::from(u32::MAX);
        let cgroup = PERF_FLAG_PID_CGROUP;
        for flag in [1_u64 << 4, 1 << 31, 1 << 32, 1 << 63] {
            assert_eq!(flag & PERF_FLAGS_DEFINED_AT_LINUX_7_1_3, 0);
            // A reserved group leader, alone or with defined flags.
            assert!(names_descriptor(open, &[0, 0, none, 7, flag, 0], 7));
            assert!(names_descriptor(
                open,
                &[0, 0, none, 7 | 1 << 32, flag | 0xb, 0],
                7
            ));
            // With PID_CGROUP and no cpu as well: that EINVAL is 7.1.3's.
            assert!(names_descriptor(
                open,
                &[0, 0, none, 7, flag | cgroup, 0],
                7
            ));
            assert!(names_descriptor(
                open,
                &[0, 7, none, none, flag | cgroup, 0],
                7
            ));
            // The pid argument, with or without PID_CGROUP.
            assert!(names_descriptor(open, &[0, 7, 0, none, flag, 0], 7));
            // Nothing reserved named: not refused.
            assert!(!names_descriptor(open, &[0, 0, none, none, flag, 0], 7));
            assert!(!names_descriptor(open, &[0, 8, 0, 9, flag | cgroup, 0], 7));
        }
    }

    #[test]
    fn perf_set_output_and_set_bpf_name_their_argument() {
        let ioctl = libc::SYS_ioctl;
        let set_output = u64::from(PERF_EVENT_IOC_SET_OUTPUT);
        let set_bpf = u64::from(PERF_EVENT_IOC_SET_BPF);
        // The event itself, under any command.
        assert!(names_descriptor(ioctl, &[7, 0x2401, 0, 0, 0, 0], 7));
        // SET_OUTPUT and SET_BPF name their argument; Linux reads the command
        // as 32 bits and the descriptor as an int.
        assert!(names_descriptor(ioctl, &[3, set_output, 7, 0, 0, 0], 7));
        assert!(names_descriptor(ioctl, &[3, set_bpf, 7, 0, 0, 0], 7));
        assert!(names_descriptor(
            ioctl,
            &[3, set_output | 1 << 32, 7 | 1 << 32, 0, 0, 0],
            7
        ));
        // SET_OUTPUT with -1 clears the redirect and names nothing.
        assert!(!names_descriptor(
            ioctl,
            &[3, set_output, u64::MAX, 0, 0, 0],
            7
        ));
        // Other perf commands' argument is a flag, a value or a pointer:
        // ENABLE, DISABLE, RESET, PERIOD, ID and PAUSE_OUTPUT.
        for command in [
            0x2400u64,
            0x2401,
            0x2403,
            0x4008_2404,
            0x8008_2407,
            0x4004_2409,
        ] {
            assert!(
                !names_descriptor(ioctl, &[3, command, 7, 0, 0, 0], 7),
                "{command:#x}"
            );
        }
    }

    #[test]
    fn only_a_file_mapping_names_its_descriptor() {
        let file = [0, 4096, 1, libc::MAP_PRIVATE as u64, 7, 0];
        let anonymous = [
            0,
            4096,
            1,
            (libc::MAP_PRIVATE | libc::MAP_ANONYMOUS) as u64,
            7,
            0,
        ];
        assert!(names_descriptor(libc::SYS_mmap, &file, 7));
        assert!(!names_descriptor(libc::SYS_mmap, &anonymous, 7));
    }

    #[test]
    fn an_at_call_names_its_descriptor_only_when_it_acts_on_it() {
        let empty = libc::AT_EMPTY_PATH as u64;
        // glibc's fstat.
        assert!(names_descriptor(
            libc::SYS_newfstatat,
            &[9, 0, 0, empty, 0, 0],
            9
        ));
        // A path relative to, or ignoring, the descriptor: run as Linux runs it.
        assert!(!names_descriptor(
            libc::SYS_newfstatat,
            &[9, 0, 0, 0, 0, 0],
            9
        ));
        assert!(!names_descriptor(libc::SYS_openat, &[9, 0, 0, 0, 0, 0], 9));
        assert!(names_descriptor(
            libc::SYS_statx,
            &[9, 0, empty, 0, 0, 0],
            9
        ));
        assert!(names_descriptor(
            libc::SYS_execveat,
            &[9, 0, 0, 0, empty, 0],
            9
        ));
        assert!(names_descriptor(
            libc::SYS_utimensat,
            &[9, 0, 0, 0, 0, 0],
            9
        ));
        assert!(!names_descriptor(
            libc::SYS_utimensat,
            &[9, 0x1000, 0, 0, 0, 0],
            9
        ));
        assert!(names_descriptor(
            libc::SYS_move_mount,
            &[0, 0, 9, 0, 0x40, 0],
            9
        ));
        assert!(!names_descriptor(
            libc::SYS_move_mount,
            &[0, 0, 9, 0, 0x4, 0],
            9
        ));
        assert!(names_descriptor(
            libc::SYS_io_uring_enter,
            &[9, 0, 0, 0, 0, 0],
            9
        ));
        // pidfd_getfd's copied number.
        assert!(names_descriptor(
            libc::SYS_pidfd_getfd,
            &[3, 9, 0, 0, 0, 0],
            9
        ));
    }

    #[test]
    fn epoll_wait_on_a_reserved_descriptor_fails_as_linux_does() {
        // epoll_pwait exists on every architecture; epoll_wait itself is
        // x86-only, so its positives run only there. The pwait positives
        // must hold everywhere the family is recognized.
        for wait in [libc::SYS_epoll_pwait, libc::SYS_epoll_pwait2] {
            assert_eq!(
                epoll_wait_result(wait, &[9, 0, 8, 0, 0, 0], 9),
                Some(-i64::from(libc::EBADF)),
                "{wait}"
            );
            assert_eq!(
                epoll_wait_result(wait, &[9, 0, 0, 0, 0, 0], 9),
                Some(-i64::from(libc::EINVAL)),
                "{wait}"
            );
        }
        #[cfg(target_arch = "x86_64")]
        {
            assert_eq!(
                epoll_wait_result(libc::SYS_epoll_wait, &[9, 0, 8, 0, 0, 0], 9),
                Some(-i64::from(libc::EBADF))
            );
            assert_eq!(
                epoll_wait_result(libc::SYS_epoll_wait, &[9, 0, 0, 0, 0, 0], 9),
                Some(-i64::from(libc::EINVAL))
            );
            assert_eq!(
                epoll_wait_result(libc::SYS_epoll_wait, &[8, 0, 8, 0, 0, 0], 9),
                None
            );
        }
        assert_eq!(
            epoll_wait_result(libc::SYS_epoll_pwait, &[8, 0, 8, 0, 0, 0], 9),
            None
        );
        // An invalid or unknown number is not an epoll wait anywhere:
        // absence must never be encoded as a matchable sentinel value.
        for unknown in [-1, i64::MAX] {
            assert_eq!(epoll_wait_result(unknown, &[9, 0, 8, 0, 0, 0], 9), None);
            assert!(!names_descriptor(unknown, &[9, 0, 8, 0, 0, 0], 9));
        }
    }

    #[test]
    fn close_range_sparing_checks_then_closes_each_gap() {
        let mut calls = Vec::new();
        let result = unsafe {
            close_range_sparing(
                &[10, 20, (1 << 1) | (1 << 2), 0, 0, 0],
                &[12, 15],
                |n, a| {
                    calls.push((n, a[0], a[1], a[2]));
                    0
                },
            )
        };
        assert_eq!(result, 0);
        assert_eq!(
            calls,
            vec![
                (libc::SYS_unshare, libc::CLONE_FILES as u64, 0, 0),
                (libc::SYS_close_range, 10, 11, 1 << 2),
                (libc::SYS_close_range, 13, 14, 1 << 2),
                (libc::SYS_close_range, 16, 20, 1 << 2),
            ]
        );
        let refuse = |args: [u64; 6]| unsafe {
            close_range_sparing(&args, &[12], |_, _| panic!("no syscall may run"))
        };
        assert_eq!(refuse([10, 20, 1, 0, 0, 0]), -i64::from(libc::EINVAL));
        assert_eq!(refuse([20, 10, 0, 0, 0, 0]), -i64::from(libc::EINVAL));
    }

    #[test]
    fn close_range_sparing_stops_at_the_first_error() {
        let mut calls = 0;
        let result = unsafe {
            close_range_sparing(&[10, 20, 0, 0, 0, 0], &[12], |_, _| {
                calls += 1;
                -i64::from(libc::EPERM)
            })
        };
        assert_eq!((result, calls), (-i64::from(libc::EPERM), 1));
    }

    #[test]
    fn ficlone_names_its_source() {
        let ioctl = libc::SYS_ioctl;
        let ficlone = u64::from(FICLONE);
        assert!(names_descriptor(ioctl, &[3, ficlone, 7, 0, 0, 0], 7));
        assert!(names_descriptor(
            ioctl,
            &[3, ficlone | 1 << 32, 7 | 1 << 32, 0, 0, 0],
            7
        ));
        assert!(!names_descriptor(ioctl, &[3, ficlone, 8, 0, 0, 0], 7));
        // FICLONERANGE and FIDEDUPERANGE take a pointer in argument 2.
        for command in [0x4020_940du64, 0xc018_9436] {
            assert!(
                !names_descriptor(ioctl, &[3, command, 7, 0, 0, 0], 7),
                "{command:#x}"
            );
        }
    }

    #[test]
    fn dupfd_query_names_its_second_descriptor() {
        let fcntl = libc::SYS_fcntl;
        let query = u64::from(F_DUPFD_QUERY);
        assert!(names_descriptor(fcntl, &[3, query, 7, 0, 0, 0], 7));
        assert!(names_descriptor(
            fcntl,
            &[3, query | 1 << 32, 7 | 1 << 32, 0, 0, 0],
            7
        ));
        assert!(!names_descriptor(fcntl, &[3, query, 8, 0, 0, 0], 7));
        // The fcntl's own descriptor, under any command.
        assert!(names_descriptor(
            fcntl,
            &[7, libc::F_GETFD as u64, 0, 0, 0, 0],
            7
        ));
        // F_DUPFD's argument is a minimum, not a descriptor.
        assert!(!names_descriptor(
            fcntl,
            &[3, libc::F_DUPFD as u64, 7, 0, 0, 0],
            7
        ));
    }

    #[test]
    fn io_uring_numbers_are_unchecked_descriptor_syscalls() {
        for number in 425..=427 {
            assert!(unchecked_descriptor_syscall(number), "{number}");
            // Linux reads the number as 32 bits.
            assert!(unchecked_descriptor_syscall(number | 1 << 32), "{number}");
        }
        #[cfg(target_arch = "x86_64")]
        for number in 0x4000_01a9..=0x4000_01ab {
            assert!(unchecked_descriptor_syscall(number), "{number:#x}");
        }
        for number in [424, 428, -1, i64::MAX, i64::MIN, 0] {
            assert!(!unchecked_descriptor_syscall(number), "{number}");
        }
    }

    #[test]
    fn kcmp_hides_reserved_indices_whatever_task() {
        let reserved = [7u64, 9];
        let closed = u64::from(CLOSED_DESCRIPTOR);
        let file = u64::from(KCMP_FILE);
        let epoll = u64::from(KCMP_EPOLL_TFD);
        assert_eq!(
            kcmp_hidden_indices(&[1, 2, file, 7, 9, 0], &reserved),
            Some([1, 2, file, closed, closed, 0])
        );
        assert_eq!(
            kcmp_hidden_indices(&[1, 2, file, 3, 9 | 1 << 32, 0], &reserved),
            Some([1, 2, file, 3, closed, 0])
        );
        assert_eq!(kcmp_hidden_indices(&[1, 2, file, 3, 4, 0], &reserved), None);
        // Linux reads `type` as an int.
        assert_eq!(
            kcmp_hidden_indices(&[1, 2, file | 1 << 32, 7, 4, 0], &reserved),
            Some([1, 2, file | 1 << 32, closed, 4, 0])
        );
        // Under KCMP_EPOLL_TFD the last argument is a pointer, never an index.
        assert_eq!(
            kcmp_hidden_indices(&[1, 2, epoll, 7, 9, 0], &reserved),
            Some([1, 2, epoll, closed, 9, 0])
        );
        assert_eq!(
            kcmp_hidden_indices(&[1, 2, epoll | 1 << 32, 3, 7, 0], &reserved),
            None
        );
        // The other types compare tasks' resources, not descriptors.
        for kind in 1..=6u64 {
            assert_eq!(
                kcmp_hidden_indices(&[1, 2, kind, 7, 9, 0], &reserved),
                None,
                "{kind}"
            );
        }
    }

    #[test]
    fn dedupe_plan_keeps_requests_within_the_buffer() {
        assert_eq!(dedupe_plan(0, 4096), DedupePlan::Copy { size: 24 });
        assert_eq!(dedupe_plan(127, 4096), DedupePlan::Copy { size: 4088 });
        assert_eq!(dedupe_plan(128, 4096), DedupePlan::OversizeHeader);
        assert_eq!(dedupe_plan(2047, 65536), DedupePlan::Copy { size: 65528 });
        assert_eq!(dedupe_plan(2048, 65536), DedupePlan::OversizeHeader);
        assert_eq!(dedupe_plan(u16::MAX, 65536), DedupePlan::OversizeHeader);
    }

    fn dedupe_copy(header_count: u16, dest_fds: &[i64]) -> Vec<u8> {
        let mut copy = vec![0u8; DEDUPE_HEADER_SIZE + DEDUPE_INFO_SIZE * dest_fds.len()];
        copy[DEDUPE_COUNT_OFFSET..DEDUPE_COUNT_OFFSET + 2]
            .copy_from_slice(&header_count.to_ne_bytes());
        for (index, fd) in dest_fds.iter().enumerate() {
            let at = DEDUPE_HEADER_SIZE + DEDUPE_INFO_SIZE * index;
            copy[at..at + 8].copy_from_slice(&fd.to_ne_bytes());
        }
        copy
    }

    fn dest_fd(copy: &[u8], index: usize) -> i64 {
        read_i64(copy, DEDUPE_HEADER_SIZE + DEDUPE_INFO_SIZE * index).unwrap()
    }

    #[test]
    fn dedupe_pins_the_count_and_hides_only_that_many_entries() {
        let reserved = [7u64];
        // The count read once is 1; the full copy claims 100 and names the
        // reserved descriptor in entry 1, which the pinned count excludes.
        let mut copy = dedupe_copy(100, &[3, 7]);
        let mut originals = [0i64; 2];
        assert_eq!(
            dedupe_hide_reserved(&mut copy, 1, &reserved, &mut originals),
            Some(0)
        );
        assert_eq!(u16::from_ne_bytes([copy[16], copy[17]]), 1);
        assert_eq!(dest_fd(&copy, 1), 7, "entry 1 is beyond the pinned count");
        // Within the count, a reserved low 32 bits is hidden and restored.
        let mut copy = dedupe_copy(2, &[7 | 1 << 32, 3]);
        assert_eq!(
            dedupe_hide_reserved(&mut copy, 2, &reserved, &mut originals),
            Some(1)
        );
        assert_eq!((dest_fd(&copy, 0), dest_fd(&copy, 1)), (-1, 3));
        assert_eq!(dedupe_restore(&mut copy, 2, &originals), Some(()));
        assert_eq!((dest_fd(&copy, 0), dest_fd(&copy, 1)), (7 | 1 << 32, 3));
        // A copy or snapshot shorter than the count changes nothing.
        let mut short = dedupe_copy(2, &[7]);
        let before = short.clone();
        assert_eq!(
            dedupe_hide_reserved(&mut short, 2, &reserved, &mut originals),
            None
        );
        assert_eq!(short, before);
        assert_eq!(
            dedupe_hide_reserved(&mut copy, 2, &reserved, &mut originals[..1]),
            None
        );
    }

    #[test]
    fn clone_range_and_epoll_slot_hide_a_reserved_descriptor() {
        let reserved = [7u64];
        let mut range = [0u8; FILE_CLONE_RANGE_SIZE];
        range[..8].copy_from_slice(&(7i64 | 1 << 32).to_ne_bytes());
        range[8..16].copy_from_slice(&4096u64.to_ne_bytes());
        assert!(clone_range_hide_reserved(&mut range, &reserved));
        assert_eq!(read_i64(&range, 0), Some(-1));
        assert_eq!(read_i64(&range, 8), Some(4096));
        range[..8].copy_from_slice(&3i64.to_ne_bytes());
        assert!(!clone_range_hide_reserved(&mut range, &reserved));
        assert_eq!(read_i64(&range, 0), Some(3));

        let mut slot = [0u8; KCMP_EPOLL_SLOT_SIZE];
        slot[..4].copy_from_slice(&7u32.to_ne_bytes());
        slot[4..8].copy_from_slice(&7u32.to_ne_bytes());
        assert!(kcmp_slot_hide_reserved(&mut slot, &reserved));
        assert_eq!(slot[..4], CLOSED_DESCRIPTOR.to_ne_bytes());
        assert_eq!(
            slot[4..8],
            7u32.to_ne_bytes(),
            "tfd is an epoll key, not a lookup"
        );
    }

    #[test]
    fn a_reserved_descriptor_zero_is_hidden_like_any_other() {
        // A runtime descriptor that could not be moved keeps the number it
        // was created at, 0 included; zero-filled fields are checked too.
        let reserved = [0u64];
        let mut range = [0u8; FILE_CLONE_RANGE_SIZE];
        assert!(clone_range_hide_reserved(&mut range, &reserved));
        assert_eq!(read_i64(&range, 0), Some(-1));
        let mut copy = dedupe_copy(3, &[0, 0, 0]);
        let mut originals = [9i64; 3];
        assert_eq!(
            dedupe_hide_reserved(&mut copy, 3, &reserved, &mut originals),
            Some(3)
        );
        assert_eq!(
            [dest_fd(&copy, 0), dest_fd(&copy, 1), dest_fd(&copy, 2)],
            [-1, -1, -1]
        );
        assert_eq!(originals, [0, 0, 0]);
        let closed = u64::from(CLOSED_DESCRIPTOR);
        let file = u64::from(KCMP_FILE);
        assert_eq!(
            kcmp_hidden_indices(&[1, 2, file, 0, 0, 0], &reserved),
            Some([1, 2, file, closed, closed, 0])
        );
        let mut slot = [0u8; KCMP_EPOLL_SLOT_SIZE];
        assert!(kcmp_slot_hide_reserved(&mut slot, &reserved));
        assert_eq!(slot[..4], CLOSED_DESCRIPTOR.to_ne_bytes());
    }
}
