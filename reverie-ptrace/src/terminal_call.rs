/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! Whether a traced call can raise a terminal's job-control signal itself.
//!
//! Linux raises `SIGTTIN` and `SIGTTOU` in one place, `__tty_check_change`
//! (drivers/tty/tty_jobctrl.c). It does so only for a call on the caller's
//! controlling terminal (`current->signal->tty == tty`) made from a
//! background process group, while the signal is neither ignored nor
//! blocked: `SIGTTIN` for a read (the line discipline's `job_control`), and
//! `SIGTTOU` for a write with `TOSTOP` set or a change of the terminal's
//! settings or foreground group (`tty_check_change`). It sends the signal to
//! the caller's whole process group (`kill_pgrp`) and fails the call with
//! `-ERESTARTSYS`, so a retry of that call raises it again. Every other
//! member of the group gets the same signal in the middle of whatever call it
//! is making. A retry of that other call does not raise the signal again.

use std::ffi::CStr;
use std::ffi::CString;
use std::os::fd::AsFd;
use std::os::fd::AsRawFd;
use std::os::fd::BorrowedFd;
use std::os::fd::FromRawFd;

use reverie::Errno;
use reverie::syscalls::SyscallArgs;
use reverie::syscalls::Sysno;

/// The most `stat` text read for one task: its fields are numbers and the
/// command name in parentheses, at most 16 bytes, so 4 KiB is ample.
const STAT_LIMIT: u64 = 4096;

/// `/dev/tty` and `/dev/console` (major 5, minors 0 and 1): a descriptor
/// open on either reaches the terminal the file stands for. For `/dev/tty`
/// that is the caller's controlling terminal, whatever its own device
/// number. `/dev/console` is counted more often than Linux stops a call
/// through it: Linux never stops a read or a write through it (n_tty's
/// `job_control` and `n_tty_write` skip `redirected_tty_write`), and stops a
/// change of its settings only where the console is the caller's
/// controlling terminal. Counting it can only refuse a call that did not
/// raise the signal, never hold the signal for a call that did, which would
/// raise it again on every retry.
const TERMINAL_ALIASES: [(u32, u32); 2] = [(5, 0), (5, 1)];

/// The descriptor through which `nr`, called with `args`, can reach a
/// terminal call that raises `signal`:
/// - for `SIGTTIN`, a read: `read`, `readv` and `preadv2`, and the input of
///   `sendfile` and `splice`;
/// - for `SIGTTOU`, a write or a change of settings: `write`, `writev`,
///   `pwritev2` and `ioctl`, and the output of `sendfile` and `splice`.
///
/// A positioned `pread64`, `preadv`, `pwrite64` or `pwritev` of a terminal
/// fails with `ESPIPE` before it reaches the terminal, because Linux opens a
/// terminal unseekable (`nonseekable_open` in `tty_open`). `preadv2` and
/// `pwritev2` with offset -1 use the file position instead, so they are
/// included whatever their offset. `None` for any other call or signal.
pub(crate) fn descriptor(nr: Sysno, args: &SyscallArgs, signal: libc::c_int) -> Option<u32> {
    // The kernel takes each of these descriptors as a 32-bit int.
    let fd = |arg: usize| Some(arg as u32);
    match (signal, nr) {
        (libc::SIGTTIN, Sysno::read | Sysno::readv | Sysno::preadv2 | Sysno::splice) => {
            fd(args.arg0)
        }
        (libc::SIGTTIN, Sysno::sendfile) => fd(args.arg1),
        (libc::SIGTTOU, Sysno::write | Sysno::writev | Sysno::pwritev2 | Sysno::ioctl) => {
            fd(args.arg0)
        }
        (libc::SIGTTOU, Sysno::sendfile) => fd(args.arg0),
        (libc::SIGTTOU, Sysno::splice) => fd(args.arg2),
        _ => None,
    }
}

/// Whether `call`, the number and arguments of the call that thread `tid`
/// was about to make when `signal` (a terminal's job-control signal) stopped
/// it, could have raised that signal itself: a call [`descriptor`] names, on
/// a descriptor that reaches the controlling terminal of the thread's task
/// ([`reaches_controlling_terminal`]). `directories` are the retained
/// `/proc/<tid>` directories of the tasks traced as `tid`, of which at most
/// one names the live task; a directory whose task has been reaped is
/// skipped. A call that is not known, or whose answer cannot be read, could
/// have: the decision then falls to the refusal, which fails closed rather
/// than hold a signal that each retry of the call would raise again.
pub(crate) fn call_may_raise<D: AsFd>(
    tid: libc::pid_t,
    call: Option<(Sysno, SyscallArgs)>,
    signal: libc::c_int,
    directories: impl IntoIterator<Item = D>,
) -> bool {
    let Some((nr, args)) = call else {
        return true;
    };
    let Some(fd) = descriptor(nr, &args, signal) else {
        return false;
    };
    for directory in directories {
        match reaches_controlling_terminal(directory.as_fd(), fd) {
            Ok(reaches) => return reaches,
            // That task has been reaped; another directory may hold the
            // number's live task.
            Err(Errno::ESRCH) => continue,
            Err(errno) => {
                tracing::warn!(
                    "(pid = {tid}) signal {signal} stopped an injected {nr} before it ran, and \
                     whether its descriptor {fd} is the controlling terminal could not be read: \
                     {errno}"
                );
                return true;
            }
        }
    }
    true
}

/// Whether descriptor `fd` of the task whose retained `/proc/<tid>`
/// directory is `directory` reaches its controlling terminal, as
/// `__tty_check_change` asks. That is so when the descriptor is open on a
/// character device that is either the terminal the task's `stat` names
/// (`tty_nr`, encoded as `stat` encodes a device number: both use
/// `new_encode_dev`) or one of [`TERMINAL_ALIASES`]. It is not so for a task
/// with no controlling terminal (`tty_nr` 0) or for a descriptor that is not
/// open (`ENOENT`). `ESRCH` means the directory's task has been reaped.
/// Any other failure to read is returned as it is.
pub(crate) fn reaches_controlling_terminal(
    directory: BorrowedFd<'_>,
    fd: u32,
) -> Result<bool, Errno> {
    let text = {
        // The guard ends before anything else runs.
        let _open = crate::launch_window::TransientOpen::begin();
        open_in(directory, c"stat")
            .and_then(|file| crate::signal_sets::read_bounded(file, STAT_LIMIT))
            .map_err(|error| crate::signal_sets::read_errno(&error))?
    };
    let terminal = text
        .as_deref()
        .and_then(controlling_terminal)
        .ok_or(Errno::EIO)?;
    if terminal == 0 {
        return Ok(false);
    }
    let name = CString::new(format!("fd/{fd}")).expect("a decimal has no NUL");
    // SAFETY: an all-zero `stat` is a valid buffer for fstatat to fill.
    let mut stat: libc::stat = unsafe { std::mem::zeroed() };
    // SAFETY: the directory descriptor is open for the call, the name is
    // NUL-terminated, and `stat` is a valid buffer. Following the link
    // stats the file the descriptor is open on.
    let result = unsafe { libc::fstatat(directory.as_raw_fd(), name.as_ptr(), &mut stat, 0) };
    if result != 0 {
        return match std::io::Error::last_os_error().raw_os_error() {
            Some(libc::ENOENT) => Ok(false),
            Some(libc::ESRCH) => Err(Errno::ESRCH),
            Some(errno) => Err(Errno::new(errno)),
            None => Err(Errno::EIO),
        };
    }
    Ok(is_terminal_device(stat.st_mode, stat.st_rdev, terminal))
}

/// Whether a file of mode `mode` and device `rdev` (as `stat` encodes it) is
/// the controlling terminal `terminal` (as `stat`'s `tty_nr` encodes it) or
/// one of [`TERMINAL_ALIASES`].
fn is_terminal_device(mode: libc::mode_t, rdev: libc::dev_t, terminal: u32) -> bool {
    mode & libc::S_IFMT == libc::S_IFCHR
        && (rdev == libc::dev_t::from(terminal)
            || TERMINAL_ALIASES
                .iter()
                .any(|&(major, minor)| rdev == libc::makedev(major, minor)))
}

/// The `tty_nr` field of a task's `stat` text, the seventh: the device
/// number of its controlling terminal, 0 for none. The command name, the
/// second field, is in parentheses and may hold any byte but NUL, so the
/// fields are counted after its last `)`. `None` for text that is not
/// Linux's.
fn controlling_terminal(text: &[u8]) -> Option<u32> {
    let close = text.iter().rposition(|&byte| byte == b')')?;
    // After the name: state, ppid, pgrp, session, tty_nr.
    let field = text[close + 1..]
        .split(|&byte| byte == b' ')
        .filter(|field| !field.is_empty())
        .nth(4)?;
    // Printed as a signed int (`seq_put_decimal_ll`); a minor of 2048 or
    // more sets its top bit.
    let number: i32 = std::str::from_utf8(field).ok()?.parse().ok()?;
    Some(number as u32)
}

/// Opens `name` read-only in `directory`.
fn open_in(directory: BorrowedFd<'_>, name: &CStr) -> std::io::Result<std::fs::File> {
    // SAFETY: the directory descriptor is open for the call, and the name is
    // a NUL-terminated string.
    let fd = unsafe {
        libc::openat(
            directory.as_raw_fd(),
            name.as_ptr(),
            libc::O_RDONLY | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: openat returned a new descriptor that nothing else owns.
    Ok(unsafe { std::fs::File::from_raw_fd(fd) })
}

#[cfg(test)]
mod tests {
    use std::os::fd::AsFd;
    use std::os::unix::fs::OpenOptionsExt;
    use std::os::unix::process::CommandExt;
    use std::process::Stdio;

    use super::*;

    fn args(arg0: usize, arg1: usize, arg2: usize) -> SyscallArgs {
        SyscallArgs::new(arg0, arg1, arg2, 0, 0, 0)
    }

    /// A call without a terminal descriptor for the signal could not have
    /// raised it, and needs no read; a call that is not known could have, as
    /// could a terminal call whose descriptor there is no directory to read
    /// in.
    #[test]
    fn a_call_could_have_raised_it_only_through_a_terminal_descriptor() {
        let none: [&std::fs::File; 0] = [];
        let three = args(3, 4, 5);
        assert!(call_may_raise(0, None, libc::SIGTTIN, none));
        for nr in [
            Sysno::poll,
            Sysno::ppoll,
            Sysno::wait4,
            Sysno::futex,
            Sysno::pread64,
        ] {
            assert!(
                !call_may_raise(0, Some((nr, three)), libc::SIGTTIN, none),
                "{nr}"
            );
            assert!(
                !call_may_raise(0, Some((nr, three)), libc::SIGTTOU, none),
                "{nr}"
            );
        }
        assert!(!call_may_raise(
            0,
            Some((Sysno::read, three)),
            libc::SIGTTOU,
            none
        ));
        assert!(!call_may_raise(
            0,
            Some((Sysno::write, three)),
            libc::SIGTTIN,
            none
        ));
        assert!(call_may_raise(
            0,
            Some((Sysno::read, three)),
            libc::SIGTTIN,
            none
        ));
    }

    /// A read or a write of a pipe could not have raised the signal: the
    /// descriptor, read through the thread's directory, is not its
    /// controlling terminal.
    #[test]
    fn a_call_on_a_pipe_could_not_have_raised_it() {
        let directory = std::fs::File::open("/proc/thread-self").unwrap();
        let (reader, writer) = std::io::pipe().unwrap();
        let read = args(reader.as_raw_fd() as usize, 0, 16);
        let write = args(writer.as_raw_fd() as usize, 0, 1);
        assert!(!call_may_raise(
            0,
            Some((Sysno::read, read)),
            libc::SIGTTIN,
            [&directory]
        ));
        assert!(!call_may_raise(
            0,
            Some((Sysno::write, write)),
            libc::SIGTTOU,
            [&directory]
        ));
    }

    /// A directory whose task has been reaped is skipped for the next one,
    /// and with none left the call could have raised the signal.
    #[test]
    fn a_reaped_task_is_skipped() {
        let mut child = std::process::Command::new("true").spawn().unwrap();
        let reaped = std::fs::File::open(format!("/proc/{}", child.id())).unwrap();
        child.wait().unwrap();
        let live = std::fs::File::open("/proc/thread-self").unwrap();
        let (reader, _writer) = std::io::pipe().unwrap();
        let read = Some((Sysno::read, args(reader.as_raw_fd() as usize, 0, 16)));
        assert!(call_may_raise(0, read, libc::SIGTTIN, [&reaped]));
        assert!(!call_may_raise(0, read, libc::SIGTTIN, [&reaped, &live]));
    }

    /// A read or a write of a task's controlling terminal could have raised
    /// the signal, and a call on its other descriptors could not. The task is
    /// a child that makes a pseudoterminal its controlling terminal on its
    /// standard input, with its standard output on `/dev/null`.
    #[test]
    fn a_call_on_the_controlling_terminal_could_have_raised_it() {
        // SAFETY: plain calls on a descriptor this test owns.
        let master = unsafe { libc::posix_openpt(libc::O_RDWR | libc::O_NOCTTY) };
        assert!(master >= 0, "{}", std::io::Error::last_os_error());
        // SAFETY: as above.
        unsafe {
            assert_eq!(libc::grantpt(master), 0);
            assert_eq!(libc::unlockpt(master), 0);
        }
        let mut name = [0 as libc::c_char; 64];
        // SAFETY: the buffer is writable for its whole length.
        assert_eq!(
            unsafe { libc::ptsname_r(master, name.as_mut_ptr(), name.len()) },
            0
        );
        // SAFETY: ptsname_r wrote a NUL-terminated name into the buffer.
        let path = unsafe { CStr::from_ptr(name.as_ptr()) }
            .to_str()
            .unwrap()
            .to_owned();
        let terminal = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(libc::O_NOCTTY)
            .open(&path)
            .unwrap();
        let mut command = std::process::Command::new("sleep");
        command.arg("60").stdin(terminal).stdout(Stdio::null());
        // SAFETY: setsid and ioctl are async-signal-safe, and allocate nothing.
        unsafe {
            command.pre_exec(|| {
                if libc::setsid() < 0 || libc::ioctl(0, libc::TIOCSCTTY, 0) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let mut child = command.spawn().unwrap();
        let pid = child.id() as libc::pid_t;
        let directory = std::fs::File::open(format!("/proc/{pid}")).unwrap();
        let on = |nr, fd| Some((nr, args(fd, 0, 1)));
        let read_terminal = call_may_raise(pid, on(Sysno::read, 0), libc::SIGTTIN, [&directory]);
        let write_terminal = call_may_raise(pid, on(Sysno::write, 0), libc::SIGTTOU, [&directory]);
        let read_null = call_may_raise(pid, on(Sysno::read, 1), libc::SIGTTIN, [&directory]);
        child.kill().unwrap();
        child.wait().unwrap();
        // SAFETY: the master is this test's own descriptor.
        unsafe { libc::close(master) };
        assert!(read_terminal, "a read of {path}, the controlling terminal");
        assert!(
            write_terminal,
            "a write of {path}, the controlling terminal"
        );
        assert!(!read_null, "a read of /dev/null");
    }

    /// A read raises SIGTTIN and a write or an ioctl raises SIGTTOU, each
    /// through its own descriptor argument; nothing else does.
    #[test]
    fn each_signal_has_its_calls_and_descriptor() {
        let three = args(3, 4, 5);
        for nr in [Sysno::read, Sysno::readv, Sysno::preadv2, Sysno::splice] {
            assert_eq!(descriptor(nr, &three, libc::SIGTTIN), Some(3), "{nr}");
            assert_eq!(
                descriptor(nr, &three, libc::SIGTTOU),
                (nr == Sysno::splice).then_some(5),
                "{nr}"
            );
        }
        assert_eq!(descriptor(Sysno::sendfile, &three, libc::SIGTTIN), Some(4));
        assert_eq!(descriptor(Sysno::sendfile, &three, libc::SIGTTOU), Some(3));
        for nr in [Sysno::write, Sysno::writev, Sysno::pwritev2, Sysno::ioctl] {
            assert_eq!(descriptor(nr, &three, libc::SIGTTOU), Some(3), "{nr}");
            assert_eq!(descriptor(nr, &three, libc::SIGTTIN), None, "{nr}");
        }
        for nr in [
            Sysno::pread64,
            Sysno::preadv,
            Sysno::pwrite64,
            Sysno::pwritev,
            Sysno::wait4,
            Sysno::ppoll,
            Sysno::rt_sigsuspend,
            Sysno::futex,
        ] {
            assert_eq!(descriptor(nr, &three, libc::SIGTTIN), None, "{nr}");
            assert_eq!(descriptor(nr, &three, libc::SIGTTOU), None, "{nr}");
        }
        assert_eq!(descriptor(Sysno::read, &three, libc::SIGTSTP), None);
        // A descriptor argument is the kernel's 32-bit int.
        let wide = args(0x1_0000_0007, 0, 0);
        assert_eq!(descriptor(Sysno::read, &wide, libc::SIGTTIN), Some(7));
    }

    /// `tty_nr` is the seventh field, counted after the command name's last
    /// `)`, whatever the name holds.
    #[test]
    fn the_controlling_terminal_is_read_after_the_name() {
        assert_eq!(
            controlling_terminal(b"42 (cat) S 1 42 42 34816 42 4194560 0\n"),
            Some(34816)
        );
        assert_eq!(
            controlling_terminal(b"42 (a) b) S\xff) R 1 42 42 0 -1 4194560\n"),
            Some(0)
        );
        assert_eq!(
            controlling_terminal(b"42 (big) S 1 42 42 -2146924544 42\n"),
            Some(-2_146_924_544i32 as u32)
        );
        assert_eq!(controlling_terminal(b"42 (short) S 1 42"), None);
        assert_eq!(controlling_terminal(b"42 no name S 1 2 3 4 5"), None);
        assert_eq!(controlling_terminal(b"42 (cat) S 1 42 42 x 42"), None);
    }

    /// The device must be a character device: the controlling terminal
    /// itself, or `/dev/tty` or `/dev/console`.
    #[test]
    fn only_the_terminal_and_its_aliases_count() {
        let pts = libc::makedev(136, 3);
        let terminal = pts as u32;
        assert!(is_terminal_device(libc::S_IFCHR | 0o620, pts, terminal));
        assert!(is_terminal_device(
            libc::S_IFCHR | 0o666,
            libc::makedev(5, 0),
            terminal
        ));
        assert!(is_terminal_device(
            libc::S_IFCHR | 0o600,
            libc::makedev(5, 1),
            terminal
        ));
        // Another terminal, the pseudoterminal master, a pipe or a block
        // device with the same number.
        assert!(!is_terminal_device(
            libc::S_IFCHR | 0o620,
            libc::makedev(136, 4),
            terminal
        ));
        assert!(!is_terminal_device(
            libc::S_IFCHR | 0o666,
            libc::makedev(5, 2),
            terminal
        ));
        assert!(!is_terminal_device(libc::S_IFIFO | 0o600, 0, terminal));
        assert!(!is_terminal_device(libc::S_IFBLK | 0o660, pts, terminal));
    }

    /// This test's own process reads as Linux shows it: a pipe is not its
    /// controlling terminal, a descriptor that is not open is not either, and
    /// with no controlling terminal nothing is (a test runner without a
    /// terminal), or else only a descriptor open on it is.
    #[test]
    fn a_pipe_or_a_closed_descriptor_never_reaches_the_terminal() {
        let directory = std::fs::File::open("/proc/thread-self").unwrap();
        let (reader, writer) = std::io::pipe().unwrap();
        for fd in [reader.as_fd(), writer.as_fd()] {
            assert_eq!(
                reaches_controlling_terminal(directory.as_fd(), fd.as_raw_fd() as u32),
                Ok(false)
            );
        }
        assert_eq!(
            reaches_controlling_terminal(directory.as_fd(), 1_000_000),
            Ok(false)
        );
    }
}
