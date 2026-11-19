/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! The facts a Tool may ask for when a signal stops an injected call before
//! the call ran (`reverie::InjectionStopFacts`, for
//! `reverie::Tool::decide_injection_stop`), read while the thread stays
//! stopped at the signal's delivery stop.
//!
//! A traced guest thread is a host task, and the tracer shares the guest's
//! `/proc` view, so these are the kernel's own facts for that thread. They
//! are read in the `/proc/<tid>` directory the session retained for the task
//! when it began tracing it (`FatalSession::traced_directories`), never by
//! number: that directory names the task it was opened for until the task is
//! reaped, and no task after. The siginfo comes from the stop itself
//! (`PTRACE_GETSIGINFO`). This module reads facts and decides nothing.

use std::ffi::CStr;
use std::ffi::CString;
use std::os::fd::AsFd;
use std::os::fd::AsRawFd;
use std::os::fd::BorrowedFd;
use std::os::fd::FromRawFd;

use reverie::DescriptorFile;
use reverie::Errno;
use reverie::FactError;
use reverie::SignalSets;
use safeptrace::Error as TraceError;
use safeptrace::Stopped;

use crate::task::FatalSession;

/// The most `stat` text read for one task: its fields are numbers and the
/// command name in parentheses, at most 16 bytes, so 4 KiB is ample.
const STAT_LIMIT: u64 = 4096;

/// The facts of the thread a signal stopped at its delivery stop `stopped`
/// before an injected call ran, read through `session`'s retained `/proc`
/// directories for the thread.
pub(crate) struct Reader<'a> {
    pub(crate) stopped: &'a Stopped,
    pub(crate) session: &'a FatalSession,
}

impl reverie::InjectionStopFacts for Reader<'_> {
    /// The stop's siginfo. It is not asked for where a seccomp filter may
    /// restrict the tracer thread (`thread_may_be_seccomp_filtered`), as such
    /// a filter may refuse `PTRACE_GETSIGINFO` or kill the tracer at it: that
    /// is `NotProvided`. A thread that has left the stop for its exit is
    /// `ESRCH`.
    fn signal_info(&self) -> Result<[u8; reverie::SIGNAL_INFO_SIZE], FactError> {
        if crate::task::thread_may_be_seccomp_filtered() {
            return Err(FactError::NotProvided);
        }
        match self.stopped.getsiginfo_bytes() {
            Ok(bytes) => Ok(bytes),
            Err(TraceError::Died(_)) => Err(FactError::Errno(Errno::ESRCH)),
            Err(TraceError::Errno(errno)) => Err(FactError::Errno(errno)),
        }
    }

    /// The thread's signal lines, read as `Guest::signal_sets` reads them
    /// (`crate::signal_sets`).
    fn signal_sets(&self) -> Result<SignalSets, FactError> {
        crate::signal_sets::read(self.stopped.pid(), |tid| {
            self.session.traced_directories(tid)
        })
    }

    /// `tty_nr` from the thread's `stat` ([`controlling_terminal_in`]).
    fn controlling_terminal(&self) -> Result<u32, FactError> {
        in_live_directory(
            self.session.traced_directories(self.stopped.pid()),
            controlling_terminal_in,
        )
    }

    /// The file the descriptor is open on ([`descriptor_file_in`]).
    fn descriptor_file(&self, fd: i32) -> Result<DescriptorFile, FactError> {
        in_live_directory(
            self.session.traced_directories(self.stopped.pid()),
            |directory| descriptor_file_in(directory, fd),
        )
    }
}

/// `read` in the first of `directories` whose task has not been reaped.
/// `directories` are the retained `/proc/<tid>` directories of the tasks the
/// session traces as one thread number, of which at most one names the live
/// task; a read that answers `ESRCH` found its task reaped, and the next
/// directory is tried. With none left, the thread no longer lives (`ESRCH`).
/// Any other answer is the fact's.
fn in_live_directory<D: AsFd, T>(
    directories: Vec<D>,
    read: impl Fn(BorrowedFd<'_>) -> Result<T, Errno>,
) -> Result<T, FactError> {
    for directory in directories {
        match read(directory.as_fd()) {
            Err(Errno::ESRCH) => continue,
            answer => return answer.map_err(FactError::Errno),
        }
    }
    Err(FactError::Errno(Errno::ESRCH))
}

/// The controlling terminal of the task whose retained `/proc/<tid>`
/// directory is `directory`: the `tty_nr` field of its `stat`
/// ([`controlling_terminal`]), 0 for none. `ESRCH` means the directory's task
/// has been reaped, and `EIO` that the text is not Linux's; any other failure
/// to read is returned as it is.
pub(crate) fn controlling_terminal_in(directory: BorrowedFd<'_>) -> Result<u32, Errno> {
    let text = {
        // The guard ends before anything else runs.
        let _open = crate::launch_window::TransientOpen::begin();
        open_in(directory, c"stat")
            .and_then(|file| crate::signal_sets::read_bounded(file, STAT_LIMIT))
            .map_err(|error| crate::signal_sets::read_errno(&error))?
    };
    text.as_deref()
        .and_then(controlling_terminal)
        .ok_or(Errno::EIO)
}

/// The file descriptor `fd` of the task whose retained `/proc/<tid>`
/// directory is `directory` is open on, as `fstat(2)` of the descriptor
/// would describe it: `fd/<fd>` in the directory is a link to that file,
/// which the call follows. `EBADF` for a descriptor that is not open: its
/// link is missing while the task's `fd` directory is there (a zombie's,
/// which has closed every descriptor, included). `ESRCH` means the
/// directory's task has been reaped, so that its `fd` directory is missing
/// too. Any other failure is returned as it is.
pub(crate) fn descriptor_file_in(
    directory: BorrowedFd<'_>,
    fd: i32,
) -> Result<DescriptorFile, Errno> {
    let name = CString::new(format!("fd/{fd}")).expect("a decimal has no NUL");
    match stat_in(directory, &name, 0) {
        Ok(stat) => Ok(DescriptorFile::new(stat.st_mode, stat.st_rdev)),
        Err(Errno::ENOENT) => match stat_in(directory, c"fd", libc::AT_SYMLINK_NOFOLLOW) {
            Ok(_) => Err(Errno::EBADF),
            Err(Errno::ENOENT | Errno::ESRCH) => Err(Errno::ESRCH),
            Err(errno) => Err(errno),
        },
        Err(errno) => Err(errno),
    }
}

/// `fstatat(2)` of `name` in `directory`, with `flags`.
fn stat_in(
    directory: BorrowedFd<'_>,
    name: &CStr,
    flags: libc::c_int,
) -> Result<libc::stat, Errno> {
    // SAFETY: an all-zero `stat` is a valid buffer for fstatat to fill.
    let mut stat: libc::stat = unsafe { std::mem::zeroed() };
    // SAFETY: the directory descriptor is open for the call, the name is
    // NUL-terminated, and `stat` is a valid buffer.
    let result = unsafe { libc::fstatat(directory.as_raw_fd(), name.as_ptr(), &mut stat, flags) };
    if result != 0 {
        return Err(match std::io::Error::last_os_error().raw_os_error() {
            Some(errno) => Errno::new(errno),
            None => Errno::EIO,
        });
    }
    Ok(stat)
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
    // Printed as a signed int (`seq_put_decimal_ll`). `new_encode_dev` puts
    // bits 8 to 19 of the minor number in bits 20 to 31, so a minor of 2^19
    // or more sets the top bit, and the field is negative.
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
    use std::os::unix::fs::MetadataExt;
    use std::os::unix::fs::OpenOptionsExt;
    use std::os::unix::process::CommandExt;
    use std::process::Stdio;

    use super::*;

    /// A pseudoterminal pair: the master, and the slave's path.
    struct Pseudoterminal {
        master: libc::c_int,
        slave: String,
    }

    impl Pseudoterminal {
        fn open() -> Self {
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
            let slave = unsafe { CStr::from_ptr(name.as_ptr()) }
                .to_str()
                .unwrap()
                .to_owned();
            Self { master, slave }
        }

        /// The slave, opened without making it a controlling terminal.
        fn open_slave(&self) -> std::fs::File {
            std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .custom_flags(libc::O_NOCTTY)
                .open(&self.slave)
                .unwrap()
        }
    }

    impl Drop for Pseudoterminal {
        fn drop(&mut self) {
            // SAFETY: the master is this test's own descriptor.
            unsafe { libc::close(self.master) };
        }
    }

    /// `tty_nr` is the seventh field, counted after the command name's last
    /// `)`, whatever the name holds, and a top bit set reads as the
    /// unsigned device number.
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

    /// This process's own descriptors read as the files they are open on: a
    /// pipe's ends as pipes, `/dev/null` as character device (1, 3), and a
    /// descriptor that is not open as `EBADF`.
    #[test]
    fn a_descriptor_reads_as_the_file_it_is_open_on() {
        let directory = std::fs::File::open("/proc/thread-self").unwrap();
        let (reader, writer) = std::io::pipe().unwrap();
        for fd in [reader.as_fd(), writer.as_fd()] {
            let file = descriptor_file_in(directory.as_fd(), fd.as_raw_fd()).unwrap();
            assert_eq!(file.mode & libc::S_IFMT, libc::S_IFIFO, "{file:?}");
            assert_eq!(file.rdev, 0, "{file:?}");
        }
        let null = std::fs::File::open("/dev/null").unwrap();
        let file = descriptor_file_in(directory.as_fd(), null.as_raw_fd()).unwrap();
        assert_eq!(file.mode & libc::S_IFMT, libc::S_IFCHR, "{file:?}");
        assert_eq!(file.rdev, libc::makedev(1, 3), "{file:?}");
        assert_eq!(
            descriptor_file_in(directory.as_fd(), 1_000_000),
            Err(Errno::EBADF)
        );
    }

    /// A child's controlling terminal reads as the device number of the
    /// terminal it made its controlling terminal, the same number its
    /// descriptor on that terminal reads with; a child that started a session
    /// and took no terminal reads 0. Each child is a session leader whose
    /// standard input is a pseudoterminal's slave, and only the first makes
    /// it its controlling terminal.
    #[test]
    fn a_childs_controlling_terminal_reads_as_its_terminals_device() {
        let pair = Pseudoterminal::open();
        let slave_device = pair.open_slave().metadata().unwrap().rdev();
        let mut children = Vec::new();
        for takes_the_terminal in [true, false] {
            let mut command = std::process::Command::new("sleep");
            command
                .arg("60")
                .stdin(pair.open_slave())
                .stdout(Stdio::null());
            // SAFETY: setsid and ioctl are async-signal-safe, and allocate
            // nothing.
            unsafe {
                command.pre_exec(move || {
                    if libc::setsid() < 0
                        || (takes_the_terminal && libc::ioctl(0, libc::TIOCSCTTY, 0) != 0)
                    {
                        return Err(std::io::Error::last_os_error());
                    }
                    Ok(())
                });
            }
            children.push(command.spawn().unwrap());
        }
        let read = |child: &std::process::Child| {
            let directory = std::fs::File::open(format!("/proc/{}", child.id())).unwrap();
            (
                controlling_terminal_in(directory.as_fd()),
                descriptor_file_in(directory.as_fd(), 0),
                descriptor_file_in(directory.as_fd(), 1),
            )
        };
        let with_terminal = read(&children[0]);
        let without = read(&children[1]);
        for mut child in children {
            child.kill().unwrap();
            child.wait().unwrap();
        }
        let (terminal, input, output) = with_terminal;
        assert_ne!(slave_device, 0);
        assert_eq!(terminal, Ok(slave_device as u32), "{}", pair.slave);
        let input = input.unwrap();
        assert_eq!(input.mode & libc::S_IFMT, libc::S_IFCHR, "{input:?}");
        assert_eq!(input.rdev, slave_device, "{input:?}");
        assert_eq!(output.unwrap().rdev, libc::makedev(1, 3));
        let (terminal, input, _) = without;
        assert_eq!(terminal, Ok(0), "a session leader that took no terminal");
        assert_eq!(input.unwrap().rdev, slave_device);
    }

    /// A directory whose task has been reaped is skipped for the next one,
    /// and with none left the thread no longer lives. Both facts read so.
    #[test]
    fn a_reaped_task_is_skipped() {
        let mut child = std::process::Command::new("true").spawn().unwrap();
        let reaped = std::fs::File::open(format!("/proc/{}", child.id())).unwrap();
        child.wait().unwrap();
        let live = std::fs::File::open("/proc/thread-self").unwrap();
        let (reader, _writer) = std::io::pipe().unwrap();
        let fd = reader.as_raw_fd();
        let gone = FactError::Errno(Errno::ESRCH);
        assert_eq!(
            in_live_directory(vec![&reaped], controlling_terminal_in),
            Err(gone)
        );
        assert_eq!(
            in_live_directory(vec![&reaped], |directory| descriptor_file_in(directory, fd)),
            Err(gone)
        );
        let file = in_live_directory(vec![&reaped, &live], |directory| {
            descriptor_file_in(directory, fd)
        })
        .unwrap();
        assert_eq!(file.mode & libc::S_IFMT, libc::S_IFIFO, "{file:?}");
        assert!(in_live_directory(vec![&reaped, &live], controlling_terminal_in).is_ok());
    }
}
