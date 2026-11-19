/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! The signal dispositions a launching process's own launcher left it.
//!
//! A program inherits the dispositions its launcher left it: across `execve`
//! an ignored signal stays ignored, and every other signal returns to its
//! default. A process that launches others changes its own dispositions as it
//! runs. Rust's runtime ignores SIGPIPE before `main`, and glibc installs its
//! handler for signal 33 when the process creates its first thread (and for
//! signal 32 at its first thread cancellation). A child forked later would
//! therefore start with whatever state that race with its fork had left.
//!
//! This module is only the mechanism for a launching program that wants its
//! programs to start as its own launcher left it. The program records those
//! dispositions once, at its start, with [`record_launch_dispositions`] in its
//! executable's preinit array. It then gives the recorded set
//! ([`launch_dispositions`]) to each launch as an explicit input,
//! `Command::ignored_signals`, and an emulating backend takes the same input.
//! Which set a launch starts from is its caller's choice.
//!
//! The dynamic loader runs the preinit array before any thread exists and
//! before every initializer and constructor, with one exception: the
//! initializer of an object marked `DF_1_INITFIRST` runs first (glibc's
//! `_dl_init`, elf/dl-init.c). glibc before 2.34 marked libpthread so, and its
//! initializer installed the handlers for signals 32 and 33. On such a system
//! a launcher's ignored 32 and 33 are not seen.

use core::sync::atomic::AtomicU8;
use core::sync::atomic::AtomicU64;
use core::sync::atomic::Ordering;

use syscalls::Errno;

/// Linux's highest signal number.
const LAST_SIGNAL: libc::c_int = 64;

/// The kernel's `struct sigaction`, as `rt_sigaction` reads and writes it.
#[repr(C)]
struct KernelSigaction {
    handler: libc::sighandler_t,
    flags: libc::c_ulong,
    restorer: usize,
    mask: u64,
}

impl KernelSigaction {
    const fn with_handler(handler: libc::sighandler_t) -> Self {
        Self {
            handler,
            flags: 0,
            restorer: 0,
            mask: 0,
        }
    }
}

/// `signal`'s bit in a 64-bit signal set.
const fn bit(signal: libc::c_int) -> u64 {
    1 << (signal - 1)
}

/// `rt_sigaction(signal, new, old)` as a raw system call. glibc's
/// `sigaction` refuses its own signals 32 and 33.
///
/// # Safety
///
/// `new` and `old` are null or valid for the kernel's `struct sigaction`.
unsafe fn rt_sigaction(
    signal: libc::c_int,
    new: *const KernelSigaction,
    old: *mut KernelSigaction,
) -> Result<(), Errno> {
    // SAFETY: the caller's pointers; the set size is the kernel's.
    let result = unsafe {
        libc::syscall(
            libc::SYS_rt_sigaction,
            signal,
            new,
            old,
            core::mem::size_of::<u64>(),
        )
    };
    Errno::result(result).map(drop)
}

/// [`RECORD`] before the first [`record_launch_dispositions`] call.
const UNRECORDED: u8 = 0;
/// [`RECORD`] while the first call reads the dispositions.
const RECORDING: u8 = 1;
/// [`RECORD`] once [`IGNORED`] holds the record.
const RECORDED: u8 = 2;

/// Whether the record has been taken ([`UNRECORDED`], [`RECORDING`] or
/// [`RECORDED`]).
static RECORD: AtomicU8 = AtomicU8::new(UNRECORDED);
/// The signals the launcher left ignored, once [`RECORD`] is [`RECORDED`].
static IGNORED: AtomicU64 = AtomicU64::new(0);

/// Records the signals this process's launcher left ignored, once. Only the
/// first call records; a later one records nothing, so the record is the
/// dispositions at the first call, whatever the process changed after it. A
/// launching program has its executable call this before any thread exists,
/// from its preinit array:
///
/// ```ignore
/// #[used]
/// #[unsafe(link_section = ".preinit_array")]
/// static RECORD_LAUNCH_DISPOSITIONS: extern "C" fn() =
///     reverie::process::start_signals::record_launch_dispositions;
/// ```
///
/// Only an executable has a preinit array, so a library loaded into another
/// program cannot record by mistake. This makes only raw system calls, so it
/// can run before the C library and the Rust runtime have started.
pub extern "C" fn record_launch_dispositions() {
    if RECORD
        .compare_exchange(UNRECORDED, RECORDING, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        return;
    }
    let mut ignored = 0;
    for signal in 1..=LAST_SIGNAL {
        let mut old = KernelSigaction::with_handler(libc::SIG_DFL);
        // SAFETY: no new action; `old` is a valid `struct sigaction`.
        let read = unsafe { rt_sigaction(signal, core::ptr::null(), &mut old) };
        if read.is_ok() && old.handler == libc::SIG_IGN {
            ignored |= bit(signal);
        }
    }
    IGNORED.store(ignored, Ordering::Relaxed);
    RECORD.store(RECORDED, Ordering::Release);
}

/// The signals this process's launcher left ignored, as a 64-bit set with
/// signal `n` at bit `n - 1`, as [`record_launch_dispositions`] recorded
/// them; `None` when this process has not recorded.
pub fn launch_dispositions() -> Option<u64> {
    (RECORD.load(Ordering::Acquire) == RECORDED).then(|| IGNORED.load(Ordering::Relaxed))
}

/// The calling thread's own status. The thread-group leader's
/// (`/proc/self/status`) shows no signal sets once the leader has exited
/// while other threads run: Linux prints them only while it can take the
/// leader's signal lock (`lock_task_sighand` in `task_sig`).
const THREAD_STATUS: &str = "/proc/thread-self/status";

/// The signals the calling process ignores now, as a set like
/// [`launch_dispositions`]'s: what a program it executed now would start
/// with ignored. It reads the calling thread's status once. Linux prints its
/// `SigIgn` set from every one of the process's dispositions in one pass,
/// holding the lock that `sigaction` takes to change one (`task_sig` and
/// `collect_sigign_sigcatch`, fs/proc/array.c), so the set is the
/// dispositions at one moment, whatever another thread changes meanwhile.
/// Reading each signal's action in turn would not be. A status that cannot
/// be read, or that has no such set, is an error, never an empty set.
pub fn ignored_now() -> std::io::Result<u64> {
    let status = std::fs::read(THREAD_STATUS)?;
    ignored_set(&status).ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("{THREAD_STATUS} has no single SigIgn line of sixteen hexadecimal digits"),
        )
    })
}

/// The set on the one `SigIgn` line of `status`: sixteen lowercase
/// hexadecimal digits, as Linux prints a set of its 64 signals
/// (`render_sigset_t`). `status` is bytes, not text: the `Name` line holds
/// the thread's name, which may be any bytes.
fn ignored_set(status: &[u8]) -> Option<u64> {
    let mut lines = status
        .split(|&byte| byte == b'\n')
        .filter_map(|line| line.strip_prefix(b"SigIgn:\t"));
    let digits = lines.next()?;
    if lines.next().is_some() || digits.len() != 16 {
        return None;
    }
    digits.iter().try_fold(0u64, |set, &digit| {
        let value = match digit {
            b'0'..=b'9' => digit - b'0',
            b'a'..=b'f' => digit - b'a' + 10,
            _ => return None,
        };
        Some(set << 4 | u64::from(value))
    })
}

/// The signals a program launched without an input starts with ignored: the
/// calling process's set now ([`ignored_now`]) with SIGPIPE at its default,
/// which a `Command` launch resets because the Rust runtime ignored it, as
/// `std::process` does. An emulating backend starts a program it is given no
/// input for with this set, so every backend agrees. It fails as
/// [`ignored_now`] fails.
pub fn ignored_without_input() -> std::io::Result<u64> {
    Ok(ignored_now()? & !bit(libc::SIGPIPE))
}

/// Gives the calling process the dispositions a launched program starts with
/// (`Command::ignored_signals`): each signal in `ignored` ignored, and every
/// other at its default. SIGKILL and SIGSTOP cannot be changed and are left
/// alone. A launcher that forks and executes a program itself, rather than
/// through a `Command`, calls this in its child to take the same input.
///
/// # Safety
///
/// Call this in a child between fork and `execve`, before it runs anything
/// that relies on a handler: it replaces every handler. It allocates nothing,
/// takes no lock and makes only raw system calls.
pub unsafe fn apply_in_child(ignored: u64) -> Result<(), Errno> {
    for signal in 1..=LAST_SIGNAL {
        if signal == libc::SIGKILL || signal == libc::SIGSTOP {
            continue;
        }
        let handler = if ignored & bit(signal) != 0 {
            libc::SIG_IGN
        } else {
            libc::SIG_DFL
        };
        let action = KernelSigaction::with_handler(handler);
        // SAFETY: `action` is a valid `struct sigaction`; no old action.
        unsafe { rt_sigaction(signal, &action, core::ptr::null_mut()) }?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::os::unix::process::CommandExt;

    use super::*;

    /// What the launcher of the test's isolated copy leaves ignored.
    const LAUNCHER: u64 = bit(libc::SIGPIPE) | bit(libc::SIGUSR2) | bit(33);

    /// This test binary records at start, as the executable of a launching
    /// program does.
    #[used]
    #[unsafe(link_section = ".preinit_array")]
    static RECORD_LAUNCH_DISPOSITIONS: extern "C" fn() = record_launch_dispositions;

    /// Leaves exactly `ignored` ignored and every other signal at its default,
    /// as a launcher leaves a program it starts. Runs between fork and exec.
    fn leave_only(ignored: u64) -> std::io::Result<()> {
        for signal in 1..=LAST_SIGNAL {
            if signal == libc::SIGKILL || signal == libc::SIGSTOP {
                continue;
            }
            let handler = if ignored & bit(signal) != 0 {
                libc::SIG_IGN
            } else {
                libc::SIG_DFL
            };
            let action = KernelSigaction::with_handler(handler);
            // SAFETY: a valid action; no old action.
            unsafe { rt_sigaction(signal, &action, core::ptr::null_mut()) }
                .map_err(|errno| std::io::Error::from_raw_os_error(errno.into_raw()))?;
        }
        Ok(())
    }

    extern "C" fn never_runs(_: libc::c_int) {}

    /// The `SigIgn` set `/bin/grep` reports when `command` launches it.
    fn launched_sigign(mut command: crate::Command) -> u64 {
        let output = tempfile::NamedTempFile::new().unwrap();
        let mut child = command
            .args(["SigIgn", "/proc/self/status"])
            .stdout(output.reopen().unwrap())
            .spawn()
            .unwrap();
        assert_eq!(child.wait_blocking().unwrap(), crate::ExitStatus::Exited(0));
        reported_sigign(&output)
    }

    /// The `SigIgn` set `/bin/grep` reports when `command`, converted with
    /// [`crate::Command::try_into_std`], is launched by the standard library.
    fn std_launched_sigign(mut command: crate::Command) -> u64 {
        let output = tempfile::NamedTempFile::new().unwrap();
        command
            .args(["SigIgn", "/proc/self/status"])
            .stdout(output.reopen().unwrap());
        let status = command.try_into_std().unwrap().status().unwrap();
        assert!(status.success(), "/bin/grep: {status}");
        reported_sigign(&output)
    }

    /// The set in the `SigIgn` line `/bin/grep` wrote to `output`.
    fn reported_sigign(output: &tempfile::NamedTempFile) -> u64 {
        let line = std::fs::read_to_string(output.path()).unwrap();
        line.trim()
            .strip_prefix("SigIgn:")
            .map(str::trim)
            .and_then(|hex| u64::from_str_radix(hex, 16).ok())
            .unwrap_or_else(|| panic!("no SigIgn line: {line:?}"))
    }

    /// A copy of this test binary starts with [`LAUNCHER`] ignored, and its
    /// preinit array records that set. The copy then changes its own
    /// dispositions, as a launching program does while it runs: it installs a
    /// handler for signal 33, as glibc's first `pthread_create` does, and one
    /// for SIGUSR2, and it ignores SIGUSR1. A second record changes nothing.
    /// The record is still [`LAUNCHER`], SIGPIPE included. A program launched
    /// with it as its input starts with exactly that set ignored; one launched
    /// without an input starts as the launching process's own dispositions and
    /// the launch's reset leave it (SIGPIPE at its default). The input holds
    /// too when the standard library launches the command, converted with
    /// [`crate::Command::try_into_std`]. Without one, the standard library may
    /// launch through `posix_spawn`, and glibc's spawn child ignores glibc's
    /// internal signals 32 and 33 before it executes the program (glibc 2.34,
    /// measured), so that case is glibc's and is not checked here.
    #[test]
    fn a_launch_given_the_record_starts_with_exactly_the_launchers_set() {
        let name = "start_signals::tests::\
                    a_launch_given_the_record_starts_with_exactly_the_launchers_set";
        if std::env::var_os(crate::ISOLATED_TEST_MARKER).is_none() {
            let mut command = std::process::Command::new(std::env::current_exe().unwrap());
            command
                .args([name, "--exact", "--test-threads=1", "--nocapture"])
                .env(crate::ISOLATED_TEST_MARKER, name)
                .stdin(std::process::Stdio::null());
            // SAFETY: the callback makes only raw rt_sigaction calls.
            unsafe { command.pre_exec(|| leave_only(LAUNCHER)) };
            let output = command.output().unwrap();
            let stdout = String::from_utf8_lossy(&output.stdout);
            assert!(
                output.status.success() && stdout.contains("1 passed"),
                "the isolated copy failed ({}):\n{stdout}\n{}",
                output.status,
                String::from_utf8_lossy(&output.stderr),
            );
            return;
        }
        // No signal is ever sent, so the handler needs no restorer.
        let handler = never_runs as extern "C" fn(libc::c_int) as libc::sighandler_t;
        for (signal, handler) in [
            (33, handler),
            (libc::SIGUSR2, handler),
            (libc::SIGUSR1, libc::SIG_IGN),
        ] {
            let action = KernelSigaction::with_handler(handler);
            // SAFETY: a valid action; no old action.
            unsafe { rt_sigaction(signal, &action, core::ptr::null_mut()) }.unwrap();
        }
        record_launch_dispositions();
        assert_eq!(launch_dispositions(), Some(LAUNCHER));

        let mut given = crate::Command::new("/bin/grep");
        given.ignored_signals(LAUNCHER);
        assert_eq!(given.get_ignored_signals(), Some(LAUNCHER));
        assert_eq!(launched_sigign(given), LAUNCHER);

        let inherited = crate::Command::new("/bin/grep");
        assert_eq!(inherited.get_ignored_signals(), None);
        assert_eq!(ignored_without_input().unwrap(), bit(libc::SIGUSR1));
        assert_eq!(launched_sigign(inherited), ignored_without_input().unwrap());

        let mut given = crate::Command::new("/bin/grep");
        given.ignored_signals(LAUNCHER);
        assert_eq!(std_launched_sigign(given), LAUNCHER, "through std");
    }

    /// The calling process's ignored set is what a program it executed would
    /// start with ignored: the copy's launcher left SIGHUP ignored, and the
    /// Rust runtime ignored SIGPIPE before `main`. A signal the process ignores
    /// later is in the set, signal 33 included, which glibc's `sigaction`
    /// refuses to change.
    #[test]
    fn the_ignored_set_now_is_the_calling_process_set() {
        let name = "start_signals::tests::the_ignored_set_now_is_the_calling_process_set";
        if std::env::var_os(crate::ISOLATED_TEST_MARKER).is_none() {
            let mut command = std::process::Command::new(std::env::current_exe().unwrap());
            command
                .args([name, "--exact", "--test-threads=1", "--nocapture"])
                .env(crate::ISOLATED_TEST_MARKER, name)
                .stdin(std::process::Stdio::null());
            // SAFETY: the callback makes only raw rt_sigaction calls.
            unsafe { command.pre_exec(|| leave_only(bit(libc::SIGHUP))) };
            let output = command.output().unwrap();
            let stdout = String::from_utf8_lossy(&output.stdout);
            assert!(
                output.status.success() && stdout.contains("1 passed"),
                "the isolated copy failed ({}):\n{stdout}\n{}",
                output.status,
                String::from_utf8_lossy(&output.stderr),
            );
            return;
        }
        let launched = bit(libc::SIGHUP) | bit(libc::SIGPIPE);
        assert_eq!(ignored_now().unwrap(), launched);
        let ignore = KernelSigaction::with_handler(libc::SIG_IGN);
        let mut glibc_33 = KernelSigaction::with_handler(libc::SIG_DFL);
        // SAFETY: valid actions.
        unsafe { rt_sigaction(33, &ignore, &mut glibc_33) }.unwrap();
        // SAFETY: a valid action; no old action.
        unsafe { rt_sigaction(libc::SIGUSR1, &ignore, core::ptr::null_mut()) }.unwrap();
        assert_eq!(
            ignored_now().unwrap(),
            launched | bit(33) | bit(libc::SIGUSR1)
        );
        // SAFETY: the action the kernel returned. glibc's set*id calls rely on
        // its handler for 33.
        unsafe { rt_sigaction(33, &glibc_33, core::ptr::null_mut()) }.unwrap();
    }

    /// A status as Linux prints one, with `name` on its `Name` line and
    /// `ignored` on its `SigIgn` line.
    fn status(name: &[u8], ignored: &[u8]) -> Vec<u8> {
        [
            b"Name:\t".as_slice(),
            name,
            b"\nUmask:\t0022\nState:\tR (running)\nSigBlk:\t0000000000000000\nSigIgn:\t",
            ignored,
            b"\nSigCgt:\t0000000180000000\nCapInh:\t0000000000000000\n",
        ]
        .concat()
    }

    /// The set is read from the `SigIgn` line's bytes, whatever bytes the
    /// thread's name is: Linux lets a thread name itself with any bytes but
    /// NUL (`PR_SET_NAME`) and escapes only a newline and a backslash in it.
    #[test]
    fn the_ignored_set_is_read_from_bytes_whatever_the_thread_name() {
        assert_eq!(
            ignored_set(&status(b"\x80\xff-call", b"0000000180001001")),
            Some(bit(1) | bit(13) | bit(32) | bit(33))
        );
        assert_eq!(
            ignored_set(&status(b"main", b"8000000000000000")),
            Some(bit(64))
        );
    }

    /// A status without exactly one `SigIgn` line of sixteen lowercase
    /// hexadecimal digits has no set, so [`ignored_now`] fails rather than
    /// answer with an empty one.
    #[test]
    fn a_status_without_one_well_formed_set_has_none() {
        let missing = b"Name:\tmain\nSigCgt:\t0000000000000000\n";
        assert_eq!(ignored_set(missing), None);
        let mut repeated = status(b"main", b"0000000000001000");
        repeated.extend_from_slice(b"SigIgn:\t0000000000001000\n");
        assert_eq!(ignored_set(&repeated), None);
        for malformed in [
            b"000000000000100".as_slice(),
            b"00000000000010000",
            b"000000000000100A",
            b"+00000000000100a",
            b"000000000000100\t",
            b"",
        ] {
            assert_eq!(
                ignored_set(&status(b"main", malformed)),
                None,
                "{:?}",
                String::from_utf8_lossy(malformed)
            );
        }
    }
}
