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

/// The signals the calling process ignores now, as a set like
/// [`launch_dispositions`]'s: what a program it executed now would start
/// with ignored. It makes only raw system calls.
pub fn ignored_now() -> u64 {
    let mut ignored = 0;
    for signal in 1..=LAST_SIGNAL {
        let mut old = KernelSigaction::with_handler(libc::SIG_DFL);
        // SAFETY: no new action; `old` is a valid `struct sigaction`.
        let read = unsafe { rt_sigaction(signal, core::ptr::null(), &mut old) };
        if read.is_ok() && old.handler == libc::SIG_IGN {
            ignored |= bit(signal);
        }
    }
    ignored
}

/// The signals a program launched without an input starts with ignored: the
/// calling process's set now ([`ignored_now`]) with SIGPIPE at its default,
/// which a `Command` launch resets because the Rust runtime ignored it, as
/// `std::process` does. An emulating backend starts a program it is given no
/// input for with this set, so every backend agrees.
pub fn ignored_without_input() -> u64 {
    ignored_now() & !bit(libc::SIGPIPE)
}

/// Gives the calling process the dispositions a launched program starts with
/// (`Command::ignored_signals`): each signal in `ignored` ignored, and every
/// other at its default. SIGKILL and SIGSTOP cannot be changed and are left
/// alone.
///
/// # Safety
///
/// Call this in a child between fork and `execve`, before it runs anything
/// that relies on a handler: it replaces every handler. It allocates nothing,
/// takes no lock and makes only raw system calls.
pub(crate) unsafe fn apply_in_child(ignored: u64) -> Result<(), Errno> {
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
        assert_eq!(ignored_without_input(), bit(libc::SIGUSR1));
        assert_eq!(launched_sigign(inherited), ignored_without_input());

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
        assert_eq!(ignored_now(), launched);
        let ignore = KernelSigaction::with_handler(libc::SIG_IGN);
        let mut glibc_33 = KernelSigaction::with_handler(libc::SIG_DFL);
        // SAFETY: valid actions.
        unsafe { rt_sigaction(33, &ignore, &mut glibc_33) }.unwrap();
        // SAFETY: a valid action; no old action.
        unsafe { rt_sigaction(libc::SIGUSR1, &ignore, core::ptr::null_mut()) }.unwrap();
        assert_eq!(ignored_now(), launched | bit(33) | bit(libc::SIGUSR1));
        // SAFETY: the action the kernel returned. glibc's set*id calls rely on
        // its handler for 33.
        unsafe { rt_sigaction(33, &glibc_33, core::ptr::null_mut()) }.unwrap();
    }
}
