/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! Facts a backend reports about the kernel objects of a guest that no
//! syscall reads without changing them, and the errors that come with them.
//!
//! A fact is a [`Guest`](crate::Guest) method that returns typed state, such
//! as [`Guest::descriptor_status`](crate::Guest::descriptor_status). Linux
//! shows the same state as text under `/proc`; a fact carries it typed, so a
//! tool can use it without reading, or trusting, a backend's `/proc`. A
//! backend whose guest threads are host tasks reads the host kernel's state
//! for them; a backend that runs the guest's syscalls itself answers from its
//! own records; a backend that can do neither declines with
//! [`FactError::NotProvided`].

use serde::Deserialize;
use serde::Serialize;

use crate::Errno;

/// Why a backend did not report a fact.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
#[non_exhaustive]
pub enum FactError {
    /// The backend does not report this fact, for any object or for this
    /// one: it can neither read the kernel's state for the guest nor answer
    /// from records of its own, or the state is one the fact's type cannot
    /// describe. This is the default of every fact method.
    NotProvided,
    /// The fact could not be read; the errno says why. Each fact method names
    /// the values it returns, such as `EBADF` for a descriptor that is not
    /// open.
    Errno(Errno),
}

/// The class of a lock held through an open file description, as Linux
/// names it in `/proc/locks` and in a descriptor's `fdinfo` `lock:` lines
/// (`lock_get_status` in Linux's `fs/locks.c`).
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
#[non_exhaustive]
pub enum LockClass {
    /// A process-associated record lock (`F_SETLK`), which Linux prints as
    /// `POSIX`.
    Posix,
    /// An open file description lock (`F_OFD_SETLK`), which Linux prints as
    /// `OFDLCK`.
    OpenFileDescription,
    /// A `flock(2)` lock, which Linux prints as `FLOCK`.
    Flock,
    /// A lease (`F_SETLEASE`), which Linux prints as `LEASE`.
    Lease,
}

/// One lock held through a descriptor's open file description, as Linux
/// prints it on one `lock:` line of the descriptor's `fdinfo`.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
#[non_exhaustive]
pub struct HeldLockFact {
    /// The lock's class.
    pub class: LockClass,
    /// Whether Linux enforces the lock: it printed `MANDATORY` for a `POSIX`
    /// or `OFDLCK` lock on a file with mandatory locking enabled. Linux 5.15
    /// removed mandatory locking, and later kernels print `ADVISORY` for
    /// every lock, so this is false there.
    pub mandatory: bool,
    /// A write (exclusive) lock; false for a read (shared) one. For a lease,
    /// a write lease.
    pub write: bool,
    /// The device of the locked file's file system, as Linux prints it on
    /// the line (the superblock's device number, `i_sb->s_dev`), encoded as
    /// `libc::makedev` encodes it. On btrfs this need not be the file's
    /// `st_dev`, which names its subvolume.
    pub raw_device: u64,
    /// The locked file's inode number.
    pub raw_inode: u64,
    /// The first byte the lock covers. Zero for a `flock(2)` lock or a
    /// lease, which cover the whole file.
    pub start: u64,
    /// The last byte the lock covers, inclusive, or `None` when it extends to
    /// the end of the file, however long it grows (Linux prints `EOF`).
    /// `None` for a `flock(2)` lock or a lease.
    pub end: Option<u64>,
}

/// What Linux reports about one open descriptor that no syscall reads without
/// changing it. Linux shows it in the descriptor's `/proc/<pid>/fdinfo/<fd>`
/// (`seq_show` in Linux's `fs/proc/fd.c`); see
/// [`Guest::descriptor_status`](crate::Guest::descriptor_status).
#[derive(Clone, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
#[non_exhaustive]
pub struct DescriptorStatus {
    /// An eventfd's counter (`eventfd-count`, from `eventfd_show_fdinfo` in
    /// Linux's `fs/eventfd.c`): reading it with `read(2)` consumes it. `None`
    /// for any other descriptor.
    pub eventfd_count: Option<u64>,
    /// For a Unix domain socket, how many descriptors are queued for it in
    /// `SCM_RIGHTS` messages it has not received yet (`scm_fds`, from
    /// `unix_show_fdinfo` in Linux's `net/unix/af_unix.c`); for a listening
    /// socket, in the messages queued for the connections it has not
    /// accepted. `None` for any other descriptor.
    pub scm_fds: Option<u32>,
    /// The locks held through the descriptor's open file description, in the
    /// order Linux lists them (`show_fd_locks` in Linux's `fs/locks.c`):
    /// `flock(2)` locks, then record locks, then leases. A `POSIX` record
    /// lock is listed only when it belongs to the descriptor table that the
    /// descriptor is read through, the thread's own.
    pub locks: Vec<HeldLockFact>,
}

/// What [`DescriptorStatus::from_fdinfo`] answers for text that is not
/// Linux's `fdinfo` format.
const MALFORMED: FactError = FactError::Errno(Errno::EIO);

/// Why one `lock:` line was not accepted.
enum Refusal {
    /// Not a line Linux prints in `fdinfo`.
    Malformed,
    /// A line Linux prints that [`HeldLockFact`] cannot describe.
    Unrepresentable,
}

impl DescriptorStatus {
    /// Parses the text Linux prints in a descriptor's
    /// `/proc/<pid>/fdinfo/<fd>`: its first line is the `pos:` line, every
    /// line ends with a newline, and of the rest only the `lock:` lines, the
    /// `eventfd-count:` line and the `scm_fds:` line are read. Lines that
    /// describe other state (`flags:`, `ino:`, an epoll's `tfd:` lines, and
    /// so on) are skipped.
    ///
    /// Returns `Err(FactError::NotProvided)` when the text shows a lock that
    /// [`HeldLockFact`] cannot describe: a delegation (`DELEG`), a lease
    /// being broken (`BREAKING`, whose type Linux prints as the one the lease
    /// is to become), or a share-mode `flock(2)` lock (`LOCK_MAND`, printed
    /// as `MSNFS`) of Linux 5.15 and earlier. Returns
    /// `Err(FactError::Errno(EIO))` when the text is not Linux's format: no
    /// `pos:` line first, an unterminated last line, a `lock:` line Linux
    /// does not print in `fdinfo`, or a missing, repeated or out-of-range
    /// value. A malformed line outweighs an undescribable one.
    pub fn from_fdinfo(text: &[u8]) -> Result<Self, FactError> {
        let body = text.strip_suffix(b"\n").ok_or(MALFORMED)?;
        let mut lines = body.split(|byte| *byte == b'\n');
        if !lines.next().is_some_and(|line| line.starts_with(b"pos:\t")) {
            return Err(MALFORMED);
        }
        let mut status = Self {
            eventfd_count: None,
            scm_fds: None,
            locks: Vec::new(),
        };
        let mut unrepresentable = false;
        for line in lines {
            if let Some(row) = line.strip_prefix(b"lock:\t") {
                match parse_lock(row) {
                    Ok(lock) => status.locks.push(lock),
                    Err(Refusal::Unrepresentable) => unrepresentable = true,
                    Err(Refusal::Malformed) => return Err(MALFORMED),
                }
            } else if let Some(value) = line.strip_prefix(b"eventfd-count:") {
                // `%16llx`: hexadecimal, right-aligned in 16 columns.
                let count = parse_number(value, 16).ok_or(MALFORMED)?;
                if status.eventfd_count.replace(count).is_some() {
                    return Err(MALFORMED);
                }
            } else if let Some(value) = line.strip_prefix(b"scm_fds:") {
                let count = parse_number(value, 10)
                    .and_then(|count| u32::try_from(count).ok())
                    .ok_or(MALFORMED)?;
                if status.scm_fds.replace(count).is_some() {
                    return Err(MALFORMED);
                }
            }
        }
        if unrepresentable {
            return Err(FactError::NotProvided);
        }
        Ok(status)
    }
}

/// A number Linux printed with `%u`, `%llu` (radix 10) or `%llx` (radix
/// 16), after the spaces that pad it: digits of the radix only, with no sign,
/// and no more than a `u64` holds.
fn parse_number(field: &[u8], radix: u32) -> Option<u64> {
    let start = field.iter().position(|byte| *byte != b' ')?;
    let digits = std::str::from_utf8(&field[start..]).ok()?;
    if !digits.chars().all(|digit| digit.is_digit(radix)) {
        return None;
    }
    u64::from_str_radix(digits, radix).ok()
}

/// One `lock:` line after its `lock:\t` prefix, as `lock_get_status` prints
/// it for `fdinfo`: `<n>: <class> <mode> <type> <pid> <major>:<minor>:<inode>
/// <start> <end>`, with no waiter prefix (`->`), since `fdinfo` lists held
/// locks only.
fn parse_lock(row: &[u8]) -> Result<HeldLockFact, Refusal> {
    let row = std::str::from_utf8(row).map_err(|_| Refusal::Malformed)?;
    let fields: Vec<&str> = row.split(' ').filter(|field| !field.is_empty()).collect();
    let &[sequence, class, mode, access, owner, object, start, end] = fields.as_slice() else {
        return Err(Refusal::Malformed);
    };
    if !sequence
        .strip_suffix(':')
        .is_some_and(|number| parse_number(number.as_bytes(), 10).is_some())
    {
        return Err(Refusal::Malformed);
    }
    let (class, mandatory) = match (class, mode) {
        ("POSIX", "ADVISORY") => (LockClass::Posix, false),
        ("POSIX", "MANDATORY") => (LockClass::Posix, true),
        ("OFDLCK", "ADVISORY") => (LockClass::OpenFileDescription, false),
        ("OFDLCK", "MANDATORY") => (LockClass::OpenFileDescription, true),
        ("FLOCK", "ADVISORY") => (LockClass::Flock, false),
        ("LEASE", "ACTIVE") => (LockClass::Lease, false),
        ("DELEG", _) | ("LEASE", "BREAKING") | ("FLOCK", "MSNFS") => {
            return Err(Refusal::Unrepresentable);
        }
        // `ACCESS` and `*NOINODE*` are never a held lock of a descriptor,
        // and a lease `BREAKER` has no file, so `fdinfo` never lists one.
        _ => return Err(Refusal::Malformed),
    };
    let write = match access {
        "WRITE" => true,
        "READ" => false,
        _ => return Err(Refusal::Malformed),
    };
    // The owner's process ID, or -1 for an OFD lock; not part of the fact.
    if parse_number(owner.strip_prefix('-').unwrap_or(owner).as_bytes(), 10).is_none() {
        return Err(Refusal::Malformed);
    }
    let (raw_device, raw_inode) = parse_object(object).ok_or(Refusal::Malformed)?;
    let start = parse_number(start.as_bytes(), 10).ok_or(Refusal::Malformed)?;
    let end = match end {
        "EOF" => None,
        end => Some(
            parse_number(end.as_bytes(), 10)
                .filter(|end| *end >= start)
                .ok_or(Refusal::Malformed)?,
        ),
    };
    // Linux prints `0 EOF` for every lock that is not a record lock.
    if matches!(class, LockClass::Flock | LockClass::Lease) && (start != 0 || end.is_some()) {
        return Err(Refusal::Malformed);
    }
    Ok(HeldLockFact {
        class,
        mandatory,
        write,
        raw_device,
        raw_inode,
        start,
        end,
    })
}

/// The `<major>:<minor>:<inode>` field: hexadecimal device numbers (`%02x`)
/// and a decimal inode number. A lock with no inode (`<none>:0`) is never
/// a descriptor's.
fn parse_object(object: &str) -> Option<(u64, u64)> {
    let mut parts = object.split(':');
    let (Some(major), Some(minor), Some(inode), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return None;
    };
    let major = u32::try_from(parse_number(major.as_bytes(), 16)?).ok()?;
    let minor = u32::try_from(parse_number(minor.as_bytes(), 16)?).ok()?;
    let inode = parse_number(inode.as_bytes(), 10)?;
    Some((libc::makedev(major, minor), inode))
}

/// The five signal sets Linux reports for one thread in its `status` file
/// (`/proc/<pid>/task/<tid>/status`; `task_sig` in Linux's
/// `fs/proc/array.c`), in the order Linux prints them. A tool gets them for
/// any live guest thread, whatever its process; see
/// [`Guest::signal_sets`](crate::Guest::signal_sets).
///
/// Each set holds signal `n`, from 1 to 64, at bit `n - 1`, as the `status`
/// line shows it: sixteen hexadecimal digits, highest-numbered signals
/// first, so the line's digits read as a hexadecimal number are the set. A
/// real-time signal queued more than once is one bit, as in Linux's sets.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
#[non_exhaustive]
pub struct SignalSets {
    /// `SigPnd`: the signals pending for this thread alone
    /// (`task->pending`), such as one sent to it with `tgkill(2)`, that it
    /// has not taken yet.
    pub pending: u64,
    /// `ShdPnd`: the signals pending for its whole thread group
    /// (`signal->shared_pending`), such as one sent with `kill(2)`, that no
    /// thread has taken yet. Every thread of a process has the same set.
    pub shared_pending: u64,
    /// `SigBlk`: the thread's signal mask (`task->blocked`). While a call
    /// that replaces the mask for its duration runs (`rt_sigsuspend`, or
    /// `ppoll`, `pselect6` or `epoll_pwait` given a mask), it is that call's
    /// mask; while a handler runs, it includes the handler's `sa_mask` and,
    /// without `SA_NODEFER`, the handler's own signal. Never `SIGKILL` or
    /// `SIGSTOP`.
    pub blocked: u64,
    /// `SigIgn`: the signals whose action is `SIG_IGN`. A signal that is
    /// ignored only by default, such as `SIGCHLD` under `SIG_DFL`, is not in
    /// it. Never `SIGKILL` or `SIGSTOP`, whose action cannot be changed.
    pub ignored: u64,
    /// `SigCgt`: the signals with a handler, an action that is neither
    /// `SIG_DFL` nor `SIG_IGN`. Never `SIGKILL` or `SIGSTOP`, and never a
    /// signal in [`ignored`](Self::ignored).
    pub caught: u64,
}

impl SignalSets {
    /// The sets in the order Linux prints them: `SigPnd`, `ShdPnd`, `SigBlk`,
    /// `SigIgn` and `SigCgt`.
    pub fn new(pending: u64, shared_pending: u64, blocked: u64, ignored: u64, caught: u64) -> Self {
        Self {
            pending,
            shared_pending,
            blocked,
            ignored,
            caught,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The fdinfo header Linux prints first for every descriptor.
    const HEADER: &str = "pos:\t0\nflags:\t02\nmnt_id:\t15\nino:\t1057\n";

    fn parse(rest: &str) -> Result<DescriptorStatus, FactError> {
        DescriptorStatus::from_fdinfo(format!("{HEADER}{rest}").as_bytes())
    }

    fn lock(row: &str) -> Result<HeldLockFact, FactError> {
        let status = parse(&format!("lock:\t{row}\n"))?;
        assert_eq!(status.locks.len(), 1, "{status:?}");
        assert_eq!((status.eventfd_count, status.scm_fds), (None, None));
        Ok(status.locks[0])
    }

    fn held(
        class: LockClass,
        mandatory: bool,
        write: bool,
        device: (u32, u32),
        raw_inode: u64,
        start: u64,
        end: Option<u64>,
    ) -> HeldLockFact {
        HeldLockFact {
            class,
            mandatory,
            write,
            raw_device: libc::makedev(device.0, device.1),
            raw_inode,
            start,
            end,
        }
    }

    #[test]
    fn a_header_alone_reports_nothing() {
        let status = parse("").unwrap();
        assert_eq!(status.eventfd_count, None);
        assert_eq!(status.scm_fds, None);
        assert!(status.locks.is_empty());
    }

    #[test]
    fn an_eventfd_reports_its_count() {
        // `eventfd_show_fdinfo`: `%16llx`, padded with spaces.
        let status =
            parse("eventfd-count:                7\neventfd-id: 3\neventfd-semaphore: 0\n")
                .unwrap();
        assert_eq!(status.eventfd_count, Some(7));
        assert_eq!(status.scm_fds, None);
        assert!(status.locks.is_empty());
        assert_eq!(
            parse("eventfd-count: fffffffffffffffe\n")
                .unwrap()
                .eventfd_count,
            Some(u64::MAX - 1)
        );
        assert_eq!(
            parse("eventfd-count:                0\n")
                .unwrap()
                .eventfd_count,
            Some(0)
        );
    }

    #[test]
    fn a_unix_socket_reports_its_queued_descriptors() {
        let status = parse("scm_fds: 2\n").unwrap();
        assert_eq!(status.scm_fds, Some(2));
        assert_eq!(status.eventfd_count, None);
        assert_eq!(parse("scm_fds: 0\n").unwrap().scm_fds, Some(0));
        assert_eq!(
            parse("scm_fds: 4294967295\n").unwrap().scm_fds,
            Some(u32::MAX)
        );
    }

    #[test]
    fn each_lock_class_and_mode_is_read() {
        assert_eq!(
            lock("1: POSIX  ADVISORY  WRITE 4242 00:2a:2856949021 0 99"),
            Ok(held(
                LockClass::Posix,
                false,
                true,
                (0, 0x2a),
                2856949021,
                0,
                Some(99)
            ))
        );
        assert_eq!(
            lock("1: POSIX  ADVISORY  READ 4242 08:03:555 100 EOF"),
            Ok(held(LockClass::Posix, false, false, (8, 3), 555, 100, None))
        );
        // Linux 5.14 and earlier, on a file with mandatory locking enabled.
        assert_eq!(
            lock("1: POSIX  MANDATORY WRITE 4242 08:03:555 0 EOF"),
            Ok(held(LockClass::Posix, true, true, (8, 3), 555, 0, None))
        );
        assert_eq!(
            lock("1: OFDLCK ADVISORY  READ -1 00:2a:1234 10 19"),
            Ok(held(
                LockClass::OpenFileDescription,
                false,
                false,
                (0, 0x2a),
                1234,
                10,
                Some(19)
            ))
        );
        assert_eq!(
            lock("1: OFDLCK MANDATORY WRITE -1 00:2a:1234 0 EOF"),
            Ok(held(
                LockClass::OpenFileDescription,
                true,
                true,
                (0, 0x2a),
                1234,
                0,
                None
            ))
        );
        assert_eq!(
            lock("1: FLOCK  ADVISORY  WRITE 4242 00:2a:1234 0 EOF"),
            Ok(held(
                LockClass::Flock,
                false,
                true,
                (0, 0x2a),
                1234,
                0,
                None
            ))
        );
        assert_eq!(
            lock("1: FLOCK  ADVISORY  READ 4242 00:2a:1234 0 EOF"),
            Ok(held(
                LockClass::Flock,
                false,
                false,
                (0, 0x2a),
                1234,
                0,
                None
            ))
        );
        assert_eq!(
            lock("1: LEASE  ACTIVE    READ 4242 00:2a:1234 0 EOF"),
            Ok(held(
                LockClass::Lease,
                false,
                false,
                (0, 0x2a),
                1234,
                0,
                None
            ))
        );
        assert_eq!(
            lock("1: LEASE  ACTIVE    WRITE 4242 00:2a:1234 0 EOF"),
            Ok(held(
                LockClass::Lease,
                false,
                true,
                (0, 0x2a),
                1234,
                0,
                None
            ))
        );
    }

    #[test]
    fn device_numbers_wider_than_a_byte_are_read() {
        // `%02x` prints at least two digits; a major is 12 bits and a minor
        // 20 bits wide.
        assert_eq!(
            lock("1: OFDLCK ADVISORY  WRITE -1 103:fffff:7 0 EOF").map(|lock| lock.raw_device),
            Ok(libc::makedev(0x103, 0xfffff))
        );
    }

    #[test]
    fn locks_keep_their_order_beside_other_lines() {
        let status = parse(concat!(
            "lock:\t1: FLOCK  ADVISORY  WRITE 4242 00:2a:9 0 EOF\n",
            "lock:\t2: OFDLCK ADVISORY  READ -1 00:2a:9 0 9\n",
            "lock:\t3: POSIX  ADVISORY  WRITE 4242 00:2a:9 10 EOF\n",
            "lock:\t4: LEASE  ACTIVE    READ 4242 00:2a:9 0 EOF\n",
            "tfd:        5 events:       19 data:                5  pos:0 ino:4e5 sdev:f\n",
        ))
        .unwrap();
        let classes: Vec<_> = status.locks.iter().map(|lock| lock.class).collect();
        assert_eq!(
            classes,
            [
                LockClass::Flock,
                LockClass::OpenFileDescription,
                LockClass::Posix,
                LockClass::Lease
            ]
        );
        assert_eq!(status.locks[1].end, Some(9));
        assert_eq!(status.locks[2].start, 10);
    }

    #[test]
    fn locks_this_type_cannot_describe_are_not_provided() {
        for row in [
            "1: DELEG  ACTIVE    READ 4242 00:2a:1234 0 EOF",
            "1: LEASE  BREAKING  UNLCK 4242 00:2a:1234 0 EOF",
            "1: LEASE  BREAKING  READ 4242 00:2a:1234 0 EOF",
            "1: FLOCK  MSNFS     RW    4242 00:2a:1234 0 EOF",
        ] {
            assert_eq!(lock(row), Err(FactError::NotProvided), "{row}");
        }
        // A malformed line outweighs an undescribable one.
        assert_eq!(
            parse(concat!(
                "lock:\t1: DELEG  ACTIVE    READ 4242 00:2a:1234 0 EOF\n",
                "lock:\t2: POSIX  ADVISORY  WRITE 4242 00:2a:1234 9 3\n",
            )),
            Err(MALFORMED)
        );
    }

    #[test]
    fn malformed_lock_lines_are_refused() {
        for row in [
            "",
            "POSIX  ADVISORY  WRITE 4242 00:2a:1234 0 EOF",
            "x: POSIX  ADVISORY  WRITE 4242 00:2a:1234 0 EOF",
            "1 POSIX  ADVISORY  WRITE 4242 00:2a:1234 0 EOF",
            "1: POSIX  ADVISORY  WRITE 4242 00:2a:1234 0",
            "1: POSIX  ADVISORY  WRITE 4242 00:2a:1234 0 EOF extra",
            "1: ACCESS ADVISORY  WRITE 4242 00:2a:1234 0 EOF",
            "1: POSIX  *NOINODE* WRITE 4242 00:2a:1234 0 EOF",
            "1: UNKNOWN UNKNOWN  WRITE 4242 00:2a:1234 0 EOF",
            "1: LEASE  BREAKER   READ 4242 00:2a:1234 0 EOF",
            "1: FLOCK  MANDATORY WRITE 4242 00:2a:1234 0 EOF",
            "1: POSIX  ADVISORY  UNLCK 4242 00:2a:1234 0 EOF",
            "1: POSIX  ADVISORY  WRITE 4242 <none>:0 0 EOF",
            "1: POSIX  ADVISORY  WRITE 4242 00:2a 0 EOF",
            "1: POSIX  ADVISORY  WRITE 4242 00:2a:1234:5 0 EOF",
            "1: POSIX  ADVISORY  WRITE 4242 0g:2a:1234 0 EOF",
            "1: POSIX  ADVISORY  WRITE 4242 00:2a:12x4 0 EOF",
            "1: POSIX  ADVISORY  WRITE 4242 100000000:2a:1234 0 EOF",
            "1: POSIX  ADVISORY  WRITE pid 00:2a:1234 0 EOF",
            "1: POSIX  ADVISORY  WRITE 4242 00:2a:1234 -1 EOF",
            "1: POSIX  ADVISORY  WRITE 4242 00:2a:1234 9 3",
            "1: POSIX  ADVISORY  WRITE 4242 00:2a:1234 0 18446744073709551616",
            "1: FLOCK  ADVISORY  WRITE 4242 00:2a:1234 1 EOF",
            "1: LEASE  ACTIVE    READ 4242 00:2a:1234 0 0",
            "1: -> POSIX  ADVISORY  WRITE 4242 00:2a:1234 0 EOF",
        ] {
            assert_eq!(lock(row), Err(MALFORMED), "{row}");
        }
    }

    #[test]
    fn malformed_records_are_refused() {
        for text in [
            "".as_bytes(),
            b"\n",
            b"flags:\t02\npos:\t0\n",
            b"pos:\t0\nflags:\t02",
            b"pos:\t0\neventfd-count: \n",
            b"pos:\t0\neventfd-count: 7g\n",
            b"pos:\t0\neventfd-count: +7\n",
            b"pos:\t0\neventfd-count: 7 \n",
            b"pos:\t0\neventfd-count: 10000000000000000\n",
            b"pos:\t0\neventfd-count: 1\neventfd-count: 1\n",
            b"pos:\t0\nscm_fds: -1\n",
            b"pos:\t0\nscm_fds: 4294967296\n",
            b"pos:\t0\nscm_fds: 1\nscm_fds: 1\n",
            b"pos:\t0\nlock:\t1: POSIX  ADVISORY  WRITE 4242 00:2a:1234 0 \xff\n",
        ] {
            assert_eq!(
                DescriptorStatus::from_fdinfo(text),
                Err(MALFORMED),
                "{}",
                String::from_utf8_lossy(text)
            );
        }
    }

    #[test]
    fn lines_of_other_state_are_skipped() {
        // An inotify descriptor's watch line, an unknown line, an empty line,
        // and bytes that are not UTF-8 on a line the parser does not read.
        let mut text = HEADER.as_bytes().to_vec();
        text.extend_from_slice(
            b"inotify wd:1 ino:4e5 sdev:2a mask:fff ignored_mask:0 fhandle-bytes:8\n",
        );
        text.extend_from_slice(b"new-line: x\n\nopaque: \xff\xfe\n");
        let status = DescriptorStatus::from_fdinfo(&text).unwrap();
        assert_eq!(status.eventfd_count, None);
        assert_eq!(status.scm_fds, None);
        assert!(status.locks.is_empty());
    }
}

#[cfg(test)]
mod signal_sets_tests {
    use super::*;

    /// The bit of signal `signal` in a set.
    fn bit(signal: i32) -> u64 {
        1 << (signal - 1)
    }

    /// The constructor takes the sets in the order Linux prints them, and a
    /// tool passing the fact through its RPC gets the same value back.
    #[test]
    fn the_sets_keep_their_order_and_their_value() {
        let sets = SignalSets::new(
            bit(libc::SIGUSR1),
            bit(libc::SIGUSR2),
            bit(libc::SIGINT) | bit(64),
            bit(libc::SIGHUP),
            bit(libc::SIGTERM) | bit(libc::SIGRTMIN()),
        );
        assert_eq!(sets.pending, 0x200);
        assert_eq!(sets.shared_pending, 0x800);
        assert_eq!(sets.blocked, 0x8000_0000_0000_0002);
        assert_eq!(sets.ignored, 0x1);
        assert_eq!(sets.caught, bit(libc::SIGTERM) | bit(libc::SIGRTMIN()));
        let text = serde_json::to_string(&sets).unwrap();
        assert_eq!(serde_json::from_str::<SignalSets>(&text).unwrap(), sets);
    }
}
