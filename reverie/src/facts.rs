/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! Facts a backend reports about the kernel state of a guest's threads,
//! which Linux shows as text under `/proc`, and the errors that come with
//! them.
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
    /// `/proc/<pid>/fdinfo/<fd>`, accepting exactly what Linux prints. The
    /// first line is the `pos:` line (`seq_show` in Linux's `fs/proc/fd.c`)
    /// and every line ends with a newline. Of the rest, the `lock:` lines
    /// (`show_fd_locks` and `lock_get_status` in `fs/locks.c`), the
    /// `eventfd-count:` line (`eventfd_show_fdinfo` in `fs/eventfd.c`) and the
    /// `scm_fds:` line (`unix_show_fdinfo` in `net/unix/af_unix.c`) are read.
    /// Each must be exactly as Linux renders it: its separators, column
    /// padding, digits, case and numeric range, with the lock lines numbered
    /// from 1 and printed before either of the other two. Lines that describe
    /// other state (`flags:`, `ino:`, an epoll's `tfd:` lines, and so on) are
    /// skipped.
    ///
    /// Returns `Err(FactError::NotProvided)` when a well-formed line shows a
    /// lock that [`HeldLockFact`] cannot describe: a delegation (`DELEG`), a
    /// lease or delegation being broken (`BREAKING`, whose type Linux prints
    /// as the one it is to become), or a share-mode `flock(2)` lock
    /// (`LOCK_MAND`, printed as `MSNFS`) of Linux 5.15 and earlier. Returns
    /// `Err(FactError::Errno(EIO))` for any other text that is not Linux's
    /// format, a malformed line of one of those kinds included: a malformed
    /// line outweighs an undescribable one.
    pub fn from_fdinfo(text: &[u8]) -> Result<Self, FactError> {
        let body = text.strip_suffix(b"\n").ok_or(MALFORMED)?;
        let mut lines = body.split(|byte| *byte == b'\n');
        // `pos:\t%lli`.
        let position = lines.next().and_then(|line| line.strip_prefix(b"pos:\t"));
        if position.and_then(exact_decimal::<i64>).is_none() {
            return Err(MALFORMED);
        }
        let mut status = Self {
            eventfd_count: None,
            scm_fds: None,
            locks: Vec::new(),
        };
        let mut lock_lines = 0;
        let mut unrepresentable = false;
        for line in lines {
            if let Some(row) = line.strip_prefix(b"lock:") {
                // `lock:\t`; the locks come before the file's own lines.
                let row = row.strip_prefix(b"\t").ok_or(MALFORMED)?;
                if status.eventfd_count.is_some() || status.scm_fds.is_some() {
                    return Err(MALFORMED);
                }
                lock_lines += 1;
                match parse_lock(row, lock_lines) {
                    Ok(lock) => status.locks.push(lock),
                    Err(Refusal::Unrepresentable) => unrepresentable = true,
                    Err(Refusal::Malformed) => return Err(MALFORMED),
                }
            } else if let Some(value) = line.strip_prefix(b"eventfd-count:") {
                // `eventfd-count: %16llx`.
                let count = value
                    .strip_prefix(b" ")
                    .and_then(exact_eventfd_count)
                    .ok_or(MALFORMED)?;
                if status.eventfd_count.replace(count).is_some() || status.scm_fds.is_some() {
                    return Err(MALFORMED);
                }
            } else if let Some(value) = line.strip_prefix(b"scm_fds:") {
                // `scm_fds: %u`.
                let count = value
                    .strip_prefix(b" ")
                    .and_then(exact_decimal::<u32>)
                    .ok_or(MALFORMED)?;
                if status.scm_fds.replace(count).is_some() || status.eventfd_count.is_some() {
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

/// `text` as Linux prints an integer of type `T` with `%d`, `%u`, `%lli`,
/// `%llu` or `%Ld`: the value's own decimal digits, with a minus sign only
/// for a negative value, and no padding, plus sign or leading zero.
fn exact_decimal<T: std::str::FromStr + ToString>(text: &[u8]) -> Option<T> {
    let text = std::str::from_utf8(text).ok()?;
    let value = text.parse::<T>().ok()?;
    (value.to_string() == text).then_some(value)
}

/// The `%16llx` of `eventfd-count`: lowercase hexadecimal digits,
/// right-aligned in 16 columns of spaces, which every `u64` fills exactly.
fn exact_eventfd_count(text: &[u8]) -> Option<u64> {
    let text = std::str::from_utf8(text).ok()?;
    let digits = text.trim_start_matches(' ');
    if !digits
        .bytes()
        .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
    {
        return None;
    }
    let count = u64::from_str_radix(digits, 16).ok()?;
    (format!("{count:16x}") == text).then_some(count)
}

/// What the class columns of a `lock:` row say.
#[derive(Clone, Copy)]
enum RowKind {
    /// A lock [`HeldLockFact`] describes, of this class, mandatory or not.
    Held(LockClass, bool),
    /// An active delegation (`DELEG  ACTIVE`).
    Delegation,
    /// A lease or delegation being broken (`BREAKING`).
    Breaking,
    /// A share-mode `flock(2)` lock (`LOCK_MAND`) of Linux 5.15 and earlier.
    ShareMode,
}

/// The class columns of every row `lock_get_status` prints for a lock held
/// through a descriptor, with the spaces it prints after them. `ACCESS` and
/// `*NOINODE*` are not held locks, and a lease `BREAKER` has no file, so
/// `fdinfo` never lists one.
const ROW_KINDS: [(&str, RowKind); 10] = [
    ("POSIX  ADVISORY  ", RowKind::Held(LockClass::Posix, false)),
    ("POSIX  MANDATORY ", RowKind::Held(LockClass::Posix, true)),
    (
        "OFDLCK ADVISORY  ",
        RowKind::Held(LockClass::OpenFileDescription, false),
    ),
    (
        "OFDLCK MANDATORY ",
        RowKind::Held(LockClass::OpenFileDescription, true),
    ),
    ("FLOCK  ADVISORY  ", RowKind::Held(LockClass::Flock, false)),
    ("LEASE  ACTIVE    ", RowKind::Held(LockClass::Lease, false)),
    ("DELEG  ACTIVE    ", RowKind::Delegation),
    ("LEASE  BREAKING  ", RowKind::Breaking),
    ("DELEG  BREAKING  ", RowKind::Breaking),
    ("FLOCK  MSNFS     ", RowKind::ShareMode),
];

impl RowKind {
    /// The type columns Linux prints after this class, with the space after
    /// them, and whether each is a write lock.
    fn types(self) -> &'static [(&'static str, bool)] {
        match self {
            // A held lock, or an active lease or delegation, reads or writes.
            Self::Held(..) | Self::Delegation => &[("WRITE ", true), ("READ ", false)],
            // `target_leasetype`: the type the lease is being broken to.
            Self::Breaking => &[("UNLCK ", false), ("READ ", false)],
            // The `LOCK_MAND` access column: `%s ` of a five-column word.
            Self::ShareMode => &[
                ("RW    ", true),
                ("READ  ", false),
                ("WRITE ", true),
                ("NONE  ", false),
            ],
        }
    }
}

/// One `lock:` row after its `lock:\t` prefix, exactly as `lock_get_status`
/// prints it for the `number`th lock of a descriptor's `fdinfo`: `%lld: `,
/// the class and type columns, `%d %02x:%02x:%llu ` for the owner's process
/// ID, the device and the inode, then the range: `%Ld %Ld` or `%Ld EOF` for
/// a record lock and `0 EOF` for any other. `fdinfo` prints no waiter rows
/// (`->`).
fn parse_lock(row: &[u8], number: u64) -> Result<HeldLockFact, Refusal> {
    let row = std::str::from_utf8(row).map_err(|_| Refusal::Malformed)?;
    let (id, columns) = row.split_once(": ").ok_or(Refusal::Malformed)?;
    if exact_decimal::<u64>(id.as_bytes()) != Some(number) {
        return Err(Refusal::Malformed);
    }
    let (kind, columns) = ROW_KINDS
        .iter()
        .find_map(|(text, kind)| columns.strip_prefix(text).map(|rest| (*kind, rest)))
        .ok_or(Refusal::Malformed)?;
    let (write, columns) = kind
        .types()
        .iter()
        .find_map(|(text, write)| columns.strip_prefix(text).map(|rest| (*write, rest)))
        .ok_or(Refusal::Malformed)?;
    let mut fields = columns.split(' ');
    let (Some(owner), Some(object), Some(start), Some(end), None) = (
        fields.next(),
        fields.next(),
        fields.next(),
        fields.next(),
        fields.next(),
    ) else {
        return Err(Refusal::Malformed);
    };
    // The owner's process ID: -1 for an OFD lock and 0 for an owner the
    // reader cannot see. It is not part of the fact.
    exact_decimal::<i32>(owner.as_bytes()).ok_or(Refusal::Malformed)?;
    let (raw_device, raw_inode) = parse_object(object).ok_or(Refusal::Malformed)?;
    let (start, end) = match kind {
        RowKind::Held(LockClass::Posix | LockClass::OpenFileDescription, _) => {
            parse_range(start, end).ok_or(Refusal::Malformed)?
        }
        // Linux prints `0 EOF` for every lock that is not a record lock.
        _ if (start, end) == ("0", "EOF") => (0, None),
        _ => return Err(Refusal::Malformed),
    };
    let RowKind::Held(class, mandatory) = kind else {
        return Err(Refusal::Unrepresentable);
    };
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

/// A record lock's `%Ld %Ld` or `%Ld EOF`: offsets of a `loff_t` that Linux
/// keeps from 0 to `OFFSET_MAX`, the end not before the start, with an end of
/// `OFFSET_MAX` printed as `EOF`.
fn parse_range(start: &str, end: &str) -> Option<(u64, Option<u64>)> {
    let start = u64::try_from(exact_decimal::<i64>(start.as_bytes())?).ok()?;
    if end == "EOF" {
        return Some((start, None));
    }
    let end = exact_decimal::<i64>(end.as_bytes())?;
    let end = u64::try_from(end).ok().filter(|_| end != i64::MAX)?;
    (start <= end).then_some((start, Some(end)))
}

/// The `%02x:%02x:%llu` object column: the superblock device's major and
/// minor numbers, at most 12 and 20 bits as Linux splits a device number,
/// and the inode number. A lock with no inode (`<none>:0`) is never a
/// descriptor's.
fn parse_object(object: &str) -> Option<(u64, u64)> {
    let mut parts = object.split(':');
    let (Some(major), Some(minor), Some(inode), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return None;
    };
    let major = exact_hex_02(major, 0xfff)?;
    let minor = exact_hex_02(minor, 0xf_ffff)?;
    let inode = exact_decimal::<u64>(inode.as_bytes())?;
    Some((libc::makedev(major, minor), inode))
}

/// `%02x` of a number no larger than `max`: lowercase hexadecimal,
/// zero-padded to two digits and no further.
fn exact_hex_02(text: &str, max: u32) -> Option<u32> {
    if !text
        .bytes()
        .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
    {
        return None;
    }
    let value = u32::from_str_radix(text, 16)
        .ok()
        .filter(|value| *value <= max)?;
    (format!("{value:02x}") == text).then_some(value)
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
    /// mask, and while `rt_sigtimedwait` waits, the mask without the signals
    /// it waits for; while a handler runs, it includes the handler's
    /// `sa_mask` and, without `SA_NODEFER`, the handler's own signal. Never
    /// `SIGKILL` or `SIGSTOP`.
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

/// One of the links to a file that Linux shows in a thread's `/proc`
/// directory, `/proc/<pid>/task/<tid>`, and whose `readlink(2)` names the
/// file: see [`Guest::link_path`](crate::Guest::link_path). The thread's
/// `root` link is not one of them.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
#[non_exhaustive]
pub enum PathLink {
    /// `fd/<fd>`: the file that descriptor `fd` of the thread's descriptor
    /// table refers to (`proc_fd_link` in Linux's `fs/proc/fd.c`).
    Descriptor(i32),
    /// `exe`: the file the thread's process executes, its address space's
    /// executable (`proc_exe_link` in Linux's `fs/proc/base.c`).
    Executable,
    /// `cwd`: the thread's working directory (`proc_cwd_link` in Linux's
    /// `fs/proc/base.c`).
    WorkingDirectory,
}

/// What Linux's `readlink(2)` gives for a [`PathLink`]: the name `d_path`
/// renders for the file the link refers to (`do_proc_readlink` in Linux's
/// `fs/proc/base.c`; `d_path` in `fs/d_path.c`). Linux names a file in one of
/// two ways, which the text's first byte tells apart, so the kind is decided
/// by the text alone. The text is never empty, has no NUL byte and is at most
/// `PATH_MAX - 1` (4,095) bytes long, ` (deleted)` included: `d_path` renders
/// into a buffer of `PATH_MAX` bytes and fails with `ENAMETOOLONG` for a
/// name that does not fit, which a backend reports as that errno.
#[derive(Clone, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
#[non_exhaustive]
pub enum LinkPath {
    /// A text that begins with `/`: the path of the file from the root of
    /// the task that reads the link, with ` (deleted)` appended once the file
    /// has been unlinked, as `d_path` renders a file of the tree of mounted
    /// file systems (`prepend_path`, which always begins the path with `/`).
    /// Some kernel-internal file systems give their files a name of the same
    /// shape, which Linux renders as the path of a deleted file, such as a
    /// memfd's `/memfd:<name> (deleted)` (`simple_dname`). A file whose own
    /// name ends in ` (deleted)` reads as an unlinked one: Linux's text does
    /// not tell them apart, and neither does this one.
    Path(Vec<u8>),
    /// A text that does not begin with `/`: the name a kernel-internal file
    /// system gives an object that has no path, through the file system's
    /// `d_dname` and `dynamic_dname`, such as `pipe:[<inode>]`,
    /// `socket:[<inode>]`, `anon_inode:[eventfd]` or `anon_inode:inotify`.
    /// `dynamic_dname` refuses a name longer than `NAME_MAX - 1` (254) bytes,
    /// so this text is never longer.
    Anonymous(Vec<u8>),
}

/// Linux's `PATH_MAX` (`include/uapi/linux/limits.h`): `d_path`'s buffer in
/// `do_proc_readlink`, whose last byte is the name's NUL.
const PATH_MAX: usize = 4096;

/// Linux's `NAME_MAX` (`include/uapi/linux/limits.h`): `dynamic_dname`'s
/// bound on a name and its NUL.
const NAME_MAX: usize = 255;

impl LinkPath {
    /// Classifies `text`, the bytes Linux's `readlink(2)` of one of a
    /// thread's `/proc` links returns, as [`LinkPath`] describes: a text that
    /// begins with `/` is a [`LinkPath::Path`], any other a
    /// [`LinkPath::Anonymous`]. Returns `Err(FactError::Errno(EIO))` for
    /// text that Linux never returns for such a link: an empty text, one
    /// longer than `PATH_MAX - 1` bytes, one with a NUL byte, or an anonymous
    /// name longer than `NAME_MAX - 1` bytes.
    pub fn from_readlink(text: &[u8]) -> Result<Self, FactError> {
        let malformed = Err(FactError::Errno(Errno::EIO));
        if text.is_empty() || text.len() >= PATH_MAX || text.contains(&0) {
            return malformed;
        }
        if text[0] == b'/' {
            Ok(Self::Path(text.to_vec()))
        } else if text.len() < NAME_MAX {
            Ok(Self::Anonymous(text.to_vec()))
        } else {
            malformed
        }
    }

    /// The link's text, whichever kind it is.
    pub fn as_bytes(&self) -> &[u8] {
        match self {
            Self::Path(text) | Self::Anonymous(text) => text,
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
    fn every_value_linux_prints_is_read() {
        // The owner's process ID is a C `int`: -1 for an OFD lock, 0 for an
        // owner the reader cannot see, and any other value Linux prints.
        for owner in ["-1", "0", "1", "2147483647", "-2147483648"] {
            assert_eq!(
                lock(&format!(
                    "1: OFDLCK ADVISORY  READ {owner} 00:2a:1234 0 EOF"
                ))
                .map(|lock| lock.raw_inode),
                Ok(1234),
                "{owner}"
            );
        }
        // The widest device, inode and offsets: a 12-bit major, a 20-bit
        // minor, a 64-bit inode and offsets up to `OFFSET_MAX`.
        assert_eq!(
            lock(
                "1: POSIX  ADVISORY  WRITE 1 fff:fffff:18446744073709551615 9223372036854775807 EOF"
            ),
            Ok(held(
                LockClass::Posix,
                false,
                true,
                (0xfff, 0xfffff),
                u64::MAX,
                i64::MAX as u64,
                None
            ))
        );
        assert_eq!(
            lock("1: POSIX  ADVISORY  READ 1 00:00:0 0 9223372036854775806"),
            Ok(held(
                LockClass::Posix,
                false,
                false,
                (0, 0),
                0,
                0,
                Some(i64::MAX as u64 - 1)
            ))
        );
        // A one-byte lock.
        assert_eq!(
            lock("1: POSIX  ADVISORY  READ 1 00:2a:1234 5 5").map(|lock| (lock.start, lock.end)),
            Ok((5, Some(5)))
        );
    }

    #[test]
    fn locks_this_type_cannot_describe_are_not_provided() {
        for row in [
            "1: DELEG  ACTIVE    READ 4242 00:2a:1234 0 EOF",
            "1: DELEG  ACTIVE    WRITE 4242 00:2a:1234 0 EOF",
            "1: LEASE  BREAKING  UNLCK 4242 00:2a:1234 0 EOF",
            "1: LEASE  BREAKING  READ 4242 00:2a:1234 0 EOF",
            "1: DELEG  BREAKING  UNLCK 4242 00:2a:1234 0 EOF",
            "1: DELEG  BREAKING  READ 4242 00:2a:1234 0 EOF",
            // Linux 5.15 and earlier: `LOCK_MAND`'s access column.
            "1: FLOCK  MSNFS     RW    4242 00:2a:1234 0 EOF",
            "1: FLOCK  MSNFS     READ  4242 00:2a:1234 0 EOF",
            "1: FLOCK  MSNFS     WRITE 4242 00:2a:1234 0 EOF",
            "1: FLOCK  MSNFS     NONE  4242 00:2a:1234 0 EOF",
        ] {
            assert_eq!(lock(row), Err(FactError::NotProvided), "{row}");
        }
        // One undescribable lock withholds the others too.
        assert_eq!(
            parse(concat!(
                "lock:\t1: FLOCK  ADVISORY  WRITE 4242 00:2a:1234 0 EOF\n",
                "lock:\t2: LEASE  BREAKING  UNLCK 4242 00:2a:1234 0 EOF\n",
            )),
            Err(FactError::NotProvided)
        );
        // A malformed line outweighs an undescribable one, before or after it.
        assert_eq!(
            parse(concat!(
                "lock:\t1: DELEG  ACTIVE    READ 4242 00:2a:1234 0 EOF\n",
                "lock:\t2: POSIX  ADVISORY  WRITE 4242 00:2a:1234 9 3\n",
            )),
            Err(MALFORMED)
        );
        assert_eq!(
            parse(concat!(
                "lock:\t1: POSIX  ADVISORY  WRITE 4242 00:2a:1234 9 3\n",
                "lock:\t2: DELEG  ACTIVE    READ 4242 00:2a:1234 0 EOF\n",
            )),
            Err(MALFORMED)
        );
    }

    #[test]
    fn malformed_rows_of_undescribable_locks_are_refused() {
        // The whole row is checked before its class withholds the fact.
        for row in [
            "2: DELEG  ACTIVE    READ 4242 00:2a:1234 0 EOF",
            "1: DELEG ACTIVE READ 4242 00:2a:1234 0 EOF",
            "1: DELEG  ACTIVE    UNLCK 4242 00:2a:1234 0 EOF",
            "1: DELEG  ACTIVE    READ x 00:2a:1234 0 EOF",
            "1: DELEG  ACTIVE    READ 4242 00:2a:1234 1 EOF",
            "1: DELEG  ACTIVE    READ 4242 00:2a:1234 0 EOF extra",
            "1: LEASE  BREAKING  WRITE 4242 00:2a:1234 0 EOF",
            "1: LEASE  BREAKING  UNLCK 4242 1000:2a:1234 0 EOF",
            "1: DELEG  BREAKING  READ 4242 0:2a:1234 0 EOF",
            "1: FLOCK  MSNFS     RW 4242 00:2a:1234 0 EOF",
            "1: FLOCK  MSNFS     READ 4242 00:2a:1234 0 EOF",
            "1: FLOCK  MSNFS     UNLCK 4242 00:2a:1234 0 EOF",
            "1: FLOCK  MSNFS     NONE  4242 00:2a:1234 0 99",
            "1: FLOCK  MSNFS     NONE  4242 <none>:0 0 EOF",
        ] {
            assert_eq!(lock(row), Err(MALFORMED), "{row}");
        }
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
    fn lock_lines_need_linux_spacing_digits_and_ranges() {
        for row in [
            // The id: `%lld: `, counting from 1.
            "2: POSIX  ADVISORY  WRITE 4242 00:2a:1234 0 EOF",
            "01: POSIX  ADVISORY  WRITE 4242 00:2a:1234 0 EOF",
            "+1: POSIX  ADVISORY  WRITE 4242 00:2a:1234 0 EOF",
            "1:  POSIX  ADVISORY  WRITE 4242 00:2a:1234 0 EOF",
            "1:\tPOSIX  ADVISORY  WRITE 4242 00:2a:1234 0 EOF",
            // The class and type columns, padded with spaces as Linux pads
            // them, in capitals.
            "1: POSIX ADVISORY WRITE 4242 00:2a:1234 0 EOF",
            "1: POSIX  ADVISORY WRITE 4242 00:2a:1234 0 EOF",
            "1: POSIX\tADVISORY\tWRITE 4242 00:2a:1234 0 EOF",
            "1: posix  advisory  write 4242 00:2a:1234 0 EOF",
            "1: POSIX  ADVISORY  write 4242 00:2a:1234 0 EOF",
            "1: POSIX  ADVISORY  WRITE  4242 00:2a:1234 0 EOF",
            "1: OFDLCK ADVISORY  READ  -1 00:2a:1234 0 EOF",
            // One space between the remaining fields, and none after them.
            "1: POSIX  ADVISORY  WRITE 4242  00:2a:1234 0 EOF",
            "1: POSIX  ADVISORY  WRITE 4242 00:2a:1234  0 EOF",
            "1: POSIX  ADVISORY  WRITE 4242 00:2a:1234 0  EOF",
            "1: POSIX  ADVISORY  WRITE 4242 00:2a:1234 0 EOF ",
            "1: POSIX  ADVISORY  WRITE 4242\t00:2a:1234 0 EOF",
            // The owner: `%d` of a C `int`.
            "1: POSIX  ADVISORY  WRITE +4242 00:2a:1234 0 EOF",
            "1: POSIX  ADVISORY  WRITE 04242 00:2a:1234 0 EOF",
            "1: POSIX  ADVISORY  WRITE -0 00:2a:1234 0 EOF",
            "1: POSIX  ADVISORY  WRITE 2147483648 00:2a:1234 0 EOF",
            "1: POSIX  ADVISORY  WRITE -2147483649 00:2a:1234 0 EOF",
            // The device: `%02x:%02x` of a 12-bit major and a 20-bit minor.
            "1: POSIX  ADVISORY  WRITE 4242 00:2A:1234 0 EOF",
            "1: POSIX  ADVISORY  WRITE 4242 0:2a:1234 0 EOF",
            "1: POSIX  ADVISORY  WRITE 4242 00:2:1234 0 EOF",
            "1: POSIX  ADVISORY  WRITE 4242 000:2a:1234 0 EOF",
            "1: POSIX  ADVISORY  WRITE 4242 00:002a:1234 0 EOF",
            "1: POSIX  ADVISORY  WRITE 4242 +0:2a:1234 0 EOF",
            "1: POSIX  ADVISORY  WRITE 4242 1000:2a:1234 0 EOF",
            "1: POSIX  ADVISORY  WRITE 4242 00:100000:1234 0 EOF",
            // The inode: `%llu`.
            "1: POSIX  ADVISORY  WRITE 4242 00:2a:01234 0 EOF",
            "1: POSIX  ADVISORY  WRITE 4242 00:2a:+1234 0 EOF",
            "1: POSIX  ADVISORY  WRITE 4242 00:2a:-1 0 EOF",
            "1: POSIX  ADVISORY  WRITE 4242 00:2a:18446744073709551616 0 EOF",
            // The range: `%Ld %Ld` or `%Ld EOF`, from 0 to `OFFSET_MAX`, with
            // an end of `OFFSET_MAX` printed as `EOF`.
            "1: POSIX  ADVISORY  WRITE 4242 00:2a:1234 00 EOF",
            "1: POSIX  ADVISORY  WRITE 4242 00:2a:1234 +0 EOF",
            "1: POSIX  ADVISORY  WRITE 4242 00:2a:1234 0 eof",
            "1: POSIX  ADVISORY  WRITE 4242 00:2a:1234 0 -1",
            "1: POSIX  ADVISORY  WRITE 4242 00:2a:1234 0 09",
            "1: POSIX  ADVISORY  WRITE 4242 00:2a:1234 0 9223372036854775807",
            "1: POSIX  ADVISORY  WRITE 4242 00:2a:1234 9223372036854775808 EOF",
            "1: OFDLCK ADVISORY  WRITE -1 00:2a:1234 10 9",
            "1: FLOCK  ADVISORY  WRITE 4242 00:2a:1234 0 99",
            "1: LEASE  ACTIVE    READ 4242 00:2a:1234 00 EOF",
        ] {
            assert_eq!(lock(row), Err(MALFORMED), "{row}");
        }
    }

    #[test]
    fn malformed_records_are_refused() {
        // `eventfd-count: %16llx` of 7.
        const COUNT: &str = "eventfd-count:                7\n";
        const POSIX: &str = "lock:\t1: POSIX  ADVISORY  WRITE 4242 00:2a:1234 0 EOF\n";
        for text in [
            "".to_string(),
            "\n".to_string(),
            "flags:\t02\npos:\t0\n".to_string(),
            "pos:\t0\nflags:\t02".to_string(),
            // `pos:\t%lli`.
            "pos: 0\n".to_string(),
            "pos:0\n".to_string(),
            "pos:\t\n".to_string(),
            "pos:\t00\n".to_string(),
            "pos:\t+1\n".to_string(),
            "pos:\t1 \n".to_string(),
            "pos:\t9223372036854775808\n".to_string(),
            // `lock:\t`.
            format!("{HEADER}lock: 1: POSIX  ADVISORY  WRITE 4242 00:2a:1234 0 EOF\n"),
            format!("{HEADER}lock:1: POSIX  ADVISORY  WRITE 4242 00:2a:1234 0 EOF\n"),
            format!("{HEADER}lock:\t\t1: POSIX  ADVISORY  WRITE 4242 00:2a:1234 0 EOF\n"),
            format!("{HEADER}lock:\n"),
            // The locks are numbered 1, 2, 3, ... in order.
            format!("{HEADER}{POSIX}{POSIX}"),
            format!("{HEADER}{POSIX}lock:\t3: OFDLCK ADVISORY  READ -1 00:2a:1234 0 EOF\n"),
            // `eventfd-count: %16llx`: lowercase digits right-aligned in 16
            // columns of spaces.
            format!("{HEADER}eventfd-count: \n"),
            format!("{HEADER}eventfd-count:\t               7\n"),
            format!("{HEADER}eventfd-count:               7\n"),
            format!("{HEADER}eventfd-count:                 7\n"),
            format!("{HEADER}eventfd-count: 7\n"),
            format!("{HEADER}eventfd-count: 0000000000000007\n"),
            format!("{HEADER}eventfd-count:                A\n"),
            format!("{HEADER}eventfd-count: FFFFFFFFFFFFFFFE\n"),
            format!("{HEADER}eventfd-count:               +7\n"),
            format!("{HEADER}eventfd-count:               7g\n"),
            format!("{HEADER}eventfd-count:               7 \n"),
            format!("{HEADER}eventfd-count: 10000000000000000\n"),
            format!("{HEADER}{COUNT}{COUNT}"),
            // `scm_fds: %u`.
            format!("{HEADER}scm_fds:2\n"),
            format!("{HEADER}scm_fds:\t2\n"),
            format!("{HEADER}scm_fds:  2\n"),
            format!("{HEADER}scm_fds: 02\n"),
            format!("{HEADER}scm_fds: +2\n"),
            format!("{HEADER}scm_fds: 2 \n"),
            format!("{HEADER}scm_fds: -1\n"),
            format!("{HEADER}scm_fds: 4294967296\n"),
            format!("{HEADER}scm_fds: 1\nscm_fds: 1\n"),
            // An eventfd is not a socket, and the locks come first.
            format!("{HEADER}{COUNT}scm_fds: 1\n"),
            format!("{HEADER}scm_fds: 1\n{COUNT}"),
            format!("{HEADER}{COUNT}{POSIX}"),
            format!("{HEADER}scm_fds: 1\n{POSIX}"),
        ] {
            assert_eq!(
                DescriptorStatus::from_fdinfo(text.as_bytes()),
                Err(MALFORMED),
                "{text:?}"
            );
        }
        let text = b"pos:\t0\nlock:\t1: POSIX  ADVISORY  WRITE 4242 00:2a:1234 0 \xff\n";
        assert_eq!(DescriptorStatus::from_fdinfo(text), Err(MALFORMED));
    }

    #[test]
    fn a_record_reads_every_header_position_linux_prints() {
        // `f_pos` is a `loff_t`, which Linux prints signed.
        for position in ["0", "-1", "9223372036854775807", "-9223372036854775808"] {
            assert_eq!(
                DescriptorStatus::from_fdinfo(format!("pos:\t{position}\n").as_bytes())
                    .map(|status| status.locks.len()),
                Ok(0),
                "{position}"
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
        text.extend_from_slice(b"locks: 1\neventfd-id: 3\nscm_fds_total: 1\n");
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

#[cfg(test)]
mod link_path_tests {
    use super::*;

    const MALFORMED: Result<LinkPath, FactError> = Err(FactError::Errno(Errno::EIO));

    /// A text that begins with `/` is a path, whatever follows: a file's
    /// path, an unlinked file's, a memfd's and the root's.
    #[test]
    fn a_text_that_begins_with_a_slash_is_a_path() {
        for text in [
            &b"/tmp/work/regular"[..],
            b"/tmp/work/deleted (deleted)",
            b"/memfd:link-path (deleted)",
            b"/dev/null",
            b"/",
            b"/ (deleted)",
            b"/proc/4242/maps",
            b"/dir with spaces/\xff\xfe not UTF-8",
        ] {
            assert_eq!(
                LinkPath::from_readlink(text),
                Ok(LinkPath::Path(text.to_vec())),
                "{}",
                String::from_utf8_lossy(text)
            );
        }
    }

    /// A text that does not begin with `/` is an object's anonymous name.
    #[test]
    fn a_text_that_does_not_begin_with_a_slash_is_anonymous() {
        for text in [
            &b"pipe:[4242]"[..],
            b"socket:[18446744073709551615]",
            b"anon_inode:[eventfd]",
            b"anon_inode:[signalfd]",
            b"anon_inode:inotify",
            b"anon_inode:[pidfd]",
            b"net:[4026531840]",
        ] {
            assert_eq!(
                LinkPath::from_readlink(text),
                Ok(LinkPath::Anonymous(text.to_vec())),
                "{}",
                String::from_utf8_lossy(text)
            );
        }
    }

    /// `d_path`'s whole text, ` (deleted)` included, fits in `PATH_MAX - 1`
    /// bytes, and `dynamic_dname`'s in `NAME_MAX - 1`; a byte more is not
    /// Linux's.
    #[test]
    fn the_bounds_are_linuxs() {
        let longest = [b"/".as_slice(), &vec![b'a'; PATH_MAX - 2]].concat();
        assert_eq!(longest.len(), 4095);
        assert_eq!(
            LinkPath::from_readlink(&longest),
            Ok(LinkPath::Path(longest.clone()))
        );
        assert_eq!(
            LinkPath::from_readlink(&[longest.as_slice(), b"a"].concat()),
            MALFORMED
        );
        let deleted = [&vec![b'/'; PATH_MAX - 11], b" (deleted)".as_slice()].concat();
        assert_eq!(deleted.len(), 4095);
        assert!(matches!(
            LinkPath::from_readlink(&deleted),
            Ok(LinkPath::Path(_))
        ));
        let anonymous = [b"anon_inode:".as_slice(), &vec![b'x'; NAME_MAX - 12]].concat();
        assert_eq!(anonymous.len(), 254);
        assert_eq!(
            LinkPath::from_readlink(&anonymous),
            Ok(LinkPath::Anonymous(anonymous.clone()))
        );
        assert_eq!(
            LinkPath::from_readlink(&[anonymous.as_slice(), b"x"].concat()),
            MALFORMED
        );
    }

    /// An empty text, and one with a NUL byte anywhere, are not Linux's.
    #[test]
    fn an_empty_text_or_a_nul_byte_is_malformed() {
        for text in [
            &b""[..],
            b"\0",
            b"/tmp\0/x",
            b"/tmp/x\0",
            b"pipe:[1]\0",
            b"\0/tmp",
        ] {
            assert_eq!(LinkPath::from_readlink(text), MALFORMED, "{text:?}");
        }
    }

    /// The text comes back whichever kind it is, and the fact survives a
    /// tool's RPC, as the other facts do.
    #[test]
    fn the_text_comes_back_and_survives_a_round_trip() {
        for link in [
            LinkPath::from_readlink(b"/tmp/work/regular").unwrap(),
            LinkPath::from_readlink(b"pipe:[7]").unwrap(),
        ] {
            let text = serde_json::to_string(&link).unwrap();
            assert_eq!(serde_json::from_str::<LinkPath>(&text).unwrap(), link);
        }
        assert_eq!(
            LinkPath::from_readlink(b"/tmp/work/regular")
                .unwrap()
                .as_bytes(),
            b"/tmp/work/regular"
        );
        assert_eq!(
            LinkPath::from_readlink(b"socket:[9]").unwrap().as_bytes(),
            b"socket:[9]"
        );
        for link in [
            PathLink::Descriptor(-1),
            PathLink::Descriptor(3),
            PathLink::Executable,
            PathLink::WorkingDirectory,
        ] {
            let text = serde_json::to_string(&link).unwrap();
            assert_eq!(serde_json::from_str::<PathLink>(&text).unwrap(), link);
        }
    }
}
