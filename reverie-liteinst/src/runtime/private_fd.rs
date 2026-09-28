/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! Private-descriptor safeguards on the admitted original/injected routes.
//!
//! Direct scalar references are rejected by descriptor number. Operations
//! which can move a descriptor through pointed-to memory or asynchronous work
//! are categorically unavailable while any private slot exists. Existing
//! io_uring instances are refused before the first private slot is acquired.

use std::io;
use std::os::unix::ffi::OsStrExt;

use reverie_preload::trap::raw_syscall6;

const UNSHARE: u32 = 1 << 1;
const CLOEXEC: u32 = 1 << 2;
// Linux _IO('$', 5). Its argument is a scalar output-event descriptor.
const PERF_EVENT_IOC_SET_OUTPUT: u32 = 0x2405;
// Linux _IOW('!', 3, struct seccomp_notif_addfd). The descriptor to copy is
// inside pointed-to memory, so inspecting only the listener in argument zero
// cannot protect a private source descriptor.
const SECCOMP_IOCTL_NOTIF_ADDFD: u32 = 0x4018_2103;
const IORING_REGISTER_PROBE: u64 = 8;
const IORING_REGISTER_USE_REGISTERED_RING: u64 = 1_u64 << 31;
// include/linux/io_uring_types.h fixes the per-task registered-ring table at
// sixteen entries. io_uring_register with USE_REGISTERED_RING returns EBADF
// only when current has no table or this exact slot is empty; every other
// result means a ring exists or the slot cannot be proved empty.
const IO_RINGFD_REG_MAX: u64 = 16;
const IO_URING_ANON_INODE: &[u8] = b"anon_inode:[io_uring]";

#[derive(Default)]
#[repr(C)]
struct IoUringProbeOp {
    op: u8,
    reserved: u8,
    flags: u16,
    reserved2: u32,
}

#[derive(Default)]
#[repr(C)]
struct IoUringProbe {
    last_op: u8,
    ops_len: u8,
    reserved: u16,
    reserved2: [u32; 3],
    op: IoUringProbeOp,
}

const _: () = assert!(std::mem::size_of::<IoUringProbe>() == 24);

fn refuse_registered_only_io_uring_with(
    mut raw: impl FnMut(i64, [u64; 6]) -> i64,
) -> io::Result<()> {
    for index in 0..IO_RINGFD_REG_MAX {
        let mut probe = IoUringProbe::default();
        let result = raw(
            libc::SYS_io_uring_register,
            [
                index,
                IORING_REGISTER_PROBE | IORING_REGISTER_USE_REGISTERED_RING,
                (&raw mut probe) as u64,
                1,
                0,
                0,
            ],
        );
        if result != -i64::from(libc::EBADF) {
            return Err(io::Error::from_raw_os_error(libc::ENOTSUP));
        }
    }
    Ok(())
}

/// Reject every inherited io_uring before acquisition can create a private
/// descriptor. This includes IORING_SETUP_SQPOLL rings, whose kernel worker can
/// consume already-published SQEs without another admitted guest syscall.
///
/// Installation is single-threaded and signals are fenced by the caller. Any
/// unreadable or unstable `/proc/self/fd` entry therefore fails closed instead
/// of being treated as proof that no ring exists.
pub(super) fn refuse_inherited_io_uring() -> io::Result<()> {
    for entry in std::fs::read_dir("/proc/self/fd")? {
        let entry = entry?;
        let target = std::fs::read_link(entry.path())?;
        if target.as_os_str().as_bytes() == IO_URING_ANON_INODE {
            return Err(io::Error::from_raw_os_error(libc::ENOTSUP));
        }
    }
    // IORING_SETUP_REGISTERED_FD_ONLY deliberately has no numeric descriptor
    // and is absent from /proc/self/fd. Enumerate the complete kernel table by
    // registered index. The one-entry probe buffer is valid even though a
    // missing slot is resolved before the probe operation itself.
    refuse_registered_only_io_uring_with(|number, args| unsafe {
        raw_syscall6(number, args)
    })
}

/// `protected` is a prepared fixed-capacity set of coordinator, setup and
/// counter descriptors. A negative value denotes absence. The raw closure is the trusted
/// scalar gate in production; tests observe the exact operation sequence.
pub(super) fn apply<const N: usize>(
    number: i64,
    args: [u64; 6],
    mut protected: [i32; N],
    mut raw: impl FnMut(i64, [u64; 6]) -> i64,
) -> Option<i64> {
    protected.sort_unstable();
    let active = protected.iter().any(|fd| *fd >= 0);
    if active
        && matches!(
            number,
            libc::SYS_sendmsg
                | libc::SYS_sendmmsg
                | libc::SYS_recvmsg
                | libc::SYS_recvmmsg
                | libc::SYS_pidfd_getfd
                | libc::SYS_io_uring_setup
                | libc::SYS_io_uring_register
                | libc::SYS_io_uring_enter
        )
    {
        // These ABIs can acquire, publish or consume descriptor aliases through
        // pointed-to structures or an asynchronous ring. No argument-level
        // inspection is sufficient at this common original/injected boundary.
        return Some(-i64::from(libc::ENOTSUP));
    }
    if active
        && number == libc::SYS_ioctl
        && args[1] as u32 == SECCOMP_IOCTL_NOTIF_ADDFD
    {
        // The addfd source is a u32 field in struct seccomp_notif_addfd. Refuse
        // the operation before dereferencing guest memory or entering Linux;
        // trusted runtime notification control uses the raw syscall gate.
        return Some(-i64::from(libc::ENOTSUP));
    }
    let contains = |value: u64| protected.iter().any(|fd| *fd >= 0 && *fd == value as i32);
    if number == libc::SYS_close && contains(args[0]) {
        // Preserve the existing coordinator-FD convention: the private slot
        // remains open and absent from admitted guest lifecycle operations.
        return Some(0);
    }
    if number == libc::SYS_close_range {
        // The Linux ABI truncates these three unsigned-int arguments. Do not
        // reject meaningful calls only because their unused high bits are set.
        let first = args[0] as u32;
        let last = args[1] as u32;
        let mut flags = args[2] as u32;
        let overlaps = protected
            .iter()
            .any(|fd| *fd >= 0 && first <= *fd as u32 && *fd as u32 <= last);
        if !overlaps {
            return None;
        }
        if first > last || flags & !(UNSHARE | CLOEXEC) != 0 {
            return Some(-i64::from(libc::EINVAL));
        }
        // The existing single-coordinator helper uses this same operation.
        // Do it ONCE, before any close/cloexec side effect, even when every
        // descriptor in the requested interval is private. Passing UNSHARE
        // again to each partition would repeat the requested table operation.
        if flags & UNSHARE != 0 {
            let result = raw(libc::SYS_unshare, [libc::CLONE_FILES as u64, 0, 0, 0, 0, 0]);
            if result != 0 {
                return Some(result);
            }
            flags &= !UNSHARE;
        }
        let mut cursor = u64::from(first);
        for fd in protected {
            if fd < 0 || (fd as u32) < first || (fd as u32) > last {
                continue;
            }
            let fd = fd as u64;
            // Also deduplicates the protected set after sorting.
            if fd < cursor {
                continue;
            }
            if cursor < fd {
                let result = raw(
                    libc::SYS_close_range,
                    [cursor, fd - 1, u64::from(flags), 0, 0, 0],
                );
                if result != 0 {
                    return Some(result);
                }
            }
            cursor = fd + 1;
        }
        if cursor <= u64::from(last) {
            return Some(raw(
                libc::SYS_close_range,
                [cursor, u64::from(last), u64::from(flags), 0, 0, 0],
            ));
        }
        return Some(0);
    }
    let targets = match number {
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
        | libc::SYS_fcntl
        | libc::SYS_dup
        | libc::SYS_sendto
        | libc::SYS_recvfrom
        | libc::SYS_shutdown
        | libc::SYS_setsockopt
        | libc::SYS_getsockopt
        | libc::SYS_connect
        | libc::SYS_bind
        | libc::SYS_listen
        | libc::SYS_accept
        | libc::SYS_accept4
        | libc::SYS_getsockname
        | libc::SYS_getpeername => contains(args[0]),
        // A guest event must not join the private event's group or redirect
        // output to it. Both operations name another event by descriptor;
        // checking only the ioctl's source fd would miss that relationship.
        libc::SYS_perf_event_open => contains(args[3]),
        libc::SYS_ioctl => {
            contains(args[0]) || (args[1] as u32 == PERF_EVENT_IOC_SET_OUTPUT && contains(args[2]))
        }
        libc::SYS_dup2 | libc::SYS_dup3 | libc::SYS_sendfile | libc::SYS_tee => {
            contains(args[0]) || contains(args[1])
        }
        libc::SYS_splice | libc::SYS_copy_file_range => contains(args[0]) || contains(args[2]),
        // Anonymous mappings ignore fd; rejecting them would change a valid
        // unrelated mapping solely because the ignored argument names a slot.
        libc::SYS_mmap => args[3] & libc::MAP_ANONYMOUS as u64 == 0 && contains(args[4]),
        _ => false,
    };
    targets.then_some(-i64::from(libc::EBADF))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn private_descriptor_indirection_is_native_only_before_protection() {
        for number in [
            libc::SYS_sendmsg,
            libc::SYS_recvmsg,
            libc::SYS_sendmmsg,
            libc::SYS_recvmmsg,
            libc::SYS_pidfd_getfd,
            libc::SYS_io_uring_setup,
            libc::SYS_io_uring_register,
            libc::SYS_io_uring_enter,
        ] {
            for fd in [5, 7, (1_u64 << 32) | 5, u64::MAX] {
                assert_eq!(
                    apply(number, [fd, 0, 0, 0, 0, 0], [5, 11, 17], |_, _| panic!(
                        "indirect descriptor operation reached kernel"
                    )),
                    Some(-i64::from(libc::ENOTSUP))
                );
            }
            assert_eq!(
                apply(number, [7, 0, 0, 0, 0, 0], [-1, -1, -1], |_, _| panic!(
                    "preactivation operation must remain forwarded"
                )),
                None
            );
        }
        for request in [
            u64::from(SECCOMP_IOCTL_NOTIF_ADDFD),
            (1_u64 << 32) | u64::from(SECCOMP_IOCTL_NOTIF_ADDFD),
        ] {
            assert_eq!(
                apply(
                    libc::SYS_ioctl,
                    [7, request, 0xdead_beef, 0, 0, 0],
                    [5, 11, 17],
                    |_, _| panic!("seccomp addfd reached Linux"),
                ),
                Some(-i64::from(libc::ENOTSUP)),
            );
            assert_eq!(
                apply(
                    libc::SYS_ioctl,
                    [7, request, 0xdead_beef, 0, 0, 0],
                    [-1, -1, -1],
                    |_, _| panic!("preactivation operation remains caller-owned"),
                ),
                None,
            );
        }
        for number in [
            libc::SYS_sendto,
            libc::SYS_recvfrom,
            libc::SYS_shutdown,
            libc::SYS_setsockopt,
            libc::SYS_getsockopt,
            libc::SYS_connect,
            libc::SYS_bind,
            libc::SYS_listen,
            libc::SYS_accept,
            libc::SYS_accept4,
            libc::SYS_getsockname,
            libc::SYS_getpeername,
        ] {
            for fd in [5, (1_u64 << 32) | 5] {
                assert_eq!(
                    apply(number, [fd, 0, 0, 0, 0, 0], [5, 11, 17], |_, _| panic!(
                        "private call reached kernel"
                    )),
                    Some(-i64::from(libc::EBADF))
                );
            }
            assert_eq!(
                apply(number, [7, 0, 0, 0, 0, 0], [5, 11, 17], |_, _| panic!(
                    "ordinary call must remain forwarded"
                )),
                None
            );
        }
    }

    #[test]
    fn registered_only_ring_inventory_requires_every_kernel_slot_to_be_empty() {
        let mut indices = Vec::new();
        refuse_registered_only_io_uring_with(|number, args| {
            assert_eq!(number, libc::SYS_io_uring_register);
            assert_eq!(args[1], IORING_REGISTER_PROBE | IORING_REGISTER_USE_REGISTERED_RING);
            assert_ne!(args[2], 0, "valid probe buffer");
            assert_eq!(args[3], 1, "one probe operation");
            indices.push(args[0]);
            -i64::from(libc::EBADF)
        })
        .unwrap();
        assert_eq!(indices, (0..IO_RINGFD_REG_MAX).collect::<Vec<_>>());

        let error = refuse_registered_only_io_uring_with(|_, args| {
            if args[0] == 7 {
                0
            } else {
                -i64::from(libc::EBADF)
            }
        })
        .unwrap_err();
        assert_eq!(error.raw_os_error(), Some(libc::ENOTSUP));

        let error = refuse_registered_only_io_uring_with(|_, args| {
            if args[0] == 3 {
                -i64::from(libc::EOPNOTSUPP)
            } else {
                -i64::from(libc::EBADF)
            }
        })
        .unwrap_err();
        assert_eq!(error.raw_os_error(), Some(libc::ENOTSUP));
    }

    #[test]
    fn perf_event_group_references_protect_only_owned_descriptors() {
        let raw = |_, _| panic!("guard must not execute the physical operation");
        for flags in [0, 1, 2, 3, 8] {
            for fd in [5, 11, (1_u64 << 32) | 5, (1_u64 << 32) | 11] {
                assert_eq!(
                    apply(
                        libc::SYS_perf_event_open,
                        [0, 0, u64::MAX, fd, flags, 0],
                        [5, 11],
                        raw,
                    ),
                    Some(-i64::from(libc::EBADF)),
                );
            }
            for fd in [7, (1_u64 << 32) | 7, u64::MAX] {
                assert_eq!(
                    apply(
                        libc::SYS_perf_event_open,
                        [0, 0, u64::MAX, fd, flags, 0],
                        [5, 11],
                        raw,
                    ),
                    None,
                );
            }
        }
    }

    #[test]
    fn perf_event_output_references_protect_only_owned_descriptors() {
        let raw = |_, _| panic!("guard must not execute the physical operation");
        for request in [
            u64::from(PERF_EVENT_IOC_SET_OUTPUT),
            (1_u64 << 32) | u64::from(PERF_EVENT_IOC_SET_OUTPUT),
        ] {
            for fd in [5, 11, (1_u64 << 32) | 5, (1_u64 << 32) | 11] {
                assert_eq!(
                    apply(libc::SYS_ioctl, [7, request, fd, 0, 0, 0], [5, 11], raw),
                    Some(-i64::from(libc::EBADF)),
                );
            }
            for fd in [9, (1_u64 << 32) | 9, u64::MAX] {
                assert_eq!(
                    apply(libc::SYS_ioctl, [7, request, fd, 0, 0, 0], [5, 11], raw),
                    None,
                );
            }
        }
        // Other ioctl arguments retain their command-specific meaning.
        assert_eq!(
            apply(libc::SYS_ioctl, [7, 0x2400, 5, 0, 0, 0], [5, 11], raw),
            None,
        );
    }

    #[test]
    fn two_private_fds_unshare_once_and_preserve_cloexec() {
        let mut calls = Vec::new();
        let result = apply(
            libc::SYS_close_range,
            [3, 20, u64::from(UNSHARE | CLOEXEC), 0, 0, 0],
            [10, 5],
            |n, a| {
                calls.push((n, a));
                0
            },
        );
        assert_eq!(result, Some(0));
        assert_eq!(
            calls,
            vec![
                (libc::SYS_unshare, [libc::CLONE_FILES as u64, 0, 0, 0, 0, 0]),
                (libc::SYS_close_range, [3, 4, u64::from(CLOEXEC), 0, 0, 0]),
                (libc::SYS_close_range, [6, 9, u64::from(CLOEXEC), 0, 0, 0]),
                (libc::SYS_close_range, [11, 20, u64::from(CLOEXEC), 0, 0, 0]),
            ]
        );
    }

    #[test]
    fn wholly_private_range_still_unshares_and_deduplicates() {
        for (last, protected) in [(5, [5, 5]), (6, [6, 5])] {
            let mut calls = Vec::new();
            let result = apply(
                libc::SYS_close_range,
                [5, last, u64::from(UNSHARE), 0, 0, 0],
                protected,
                |n, a| {
                    calls.push((n, a));
                    0
                },
            );
            assert_eq!(result, Some(0));
            assert_eq!(
                calls,
                vec![(libc::SYS_unshare, [libc::CLONE_FILES as u64, 0, 0, 0, 0, 0])]
            );
        }
    }

    #[test]
    fn error_stops_later_partitions_without_undoing_earlier_effects() {
        let mut calls = Vec::new();
        let result = apply(
            libc::SYS_close_range,
            [3, 20, u64::from(UNSHARE), 0, 0, 0],
            [5, 10],
            |n, a| {
                calls.push((n, a));
                if calls.len() == 3 {
                    -i64::from(libc::EINTR)
                } else {
                    0
                }
            },
        );
        assert_eq!(result, Some(-i64::from(libc::EINTR)));
        assert_eq!(
            calls,
            vec![
                (libc::SYS_unshare, [libc::CLONE_FILES as u64, 0, 0, 0, 0, 0]),
                (libc::SYS_close_range, [3, 4, 0, 0, 0, 0]),
                (libc::SYS_close_range, [6, 9, 0, 0, 0, 0]),
            ]
        );
        let mut calls = Vec::new();
        let result = apply(
            libc::SYS_close_range,
            [3, 20, u64::from(UNSHARE), 0, 0, 0],
            [5, 10],
            |n, a| {
                calls.push((n, a));
                -i64::from(libc::ENOMEM)
            },
        );
        assert_eq!(result, Some(-i64::from(libc::ENOMEM)));
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, libc::SYS_unshare);
    }

    #[test]
    fn fd_integer_abi_and_ignored_anonymous_mmap_argument() {
        let raw = |_, _| panic!("no physical operation expected");
        assert_eq!(
            apply(
                libc::SYS_ioctl,
                [(1_u64 << 32) | 5, 0, 0, 0, 0, 0],
                [5, -1],
                raw
            ),
            Some(-i64::from(libc::EBADF))
        );
        assert_eq!(
            apply(
                libc::SYS_mmap,
                [0, 4096, 0, libc::MAP_ANONYMOUS as u64, 5, 0],
                [5, -1],
                raw
            ),
            None
        );
        assert_eq!(
            apply(libc::SYS_mmap, [0, 4096, 0, 0, 5, 0], [5, -1], raw),
            Some(-i64::from(libc::EBADF))
        );
        assert_eq!(
            apply(libc::SYS_sendfile, [8, 5, 0, 0, 0, 0], [5, -1], raw),
            Some(-i64::from(libc::EBADF))
        );
        assert_eq!(
            apply(libc::SYS_dup2, [8, 5, 0, 0, 0, 0], [5, -1], raw),
            Some(-i64::from(libc::EBADF))
        );
        assert_eq!(
            apply(libc::SYS_dup2, [8, 9, 0, 0, 0, 0], [5, -1], raw),
            None
        );
    }

    #[test]
    fn native_unsigned_range_arguments_and_invalid_flags() {
        let mut calls = Vec::new();
        let result = apply(
            libc::SYS_close_range,
            [(1_u64 << 32) | 4, (1_u64 << 32) | 6, 1_u64 << 32, 0, 0, 0],
            [5, -1],
            |n, a| {
                calls.push((n, a));
                0
            },
        );
        assert_eq!(result, Some(0));
        assert_eq!(
            calls,
            vec![
                (libc::SYS_close_range, [4, 4, 0, 0, 0, 0]),
                (libc::SYS_close_range, [6, 6, 0, 0, 0, 0]),
            ]
        );
        assert_eq!(
            apply(
                libc::SYS_close_range,
                [4, 6, 1, 0, 0, 0],
                [5, -1],
                |_, _| panic!("invalid flags must have no effects")
            ),
            Some(-i64::from(libc::EINVAL))
        );
        // An invalid range has no protected overlap and goes to Linux unchanged.
        assert_eq!(
            apply(
                libc::SYS_close_range,
                [6, 4, 0, 0, 0, 0],
                [5, -1],
                |_, _| panic!("native operation remains caller-owned")
            ),
            None
        );
    }
}
