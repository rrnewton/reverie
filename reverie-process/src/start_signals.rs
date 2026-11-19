/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! The signal dispositions a launched program starts with.
//!
//! A program inherits the dispositions its launcher left it: across `execve`
//! an ignored signal stays ignored, and every other signal returns to its
//! default. The process that launches programs changes its own dispositions
//! as it runs. Rust's runtime ignores SIGPIPE before `main`, and glibc installs
//! its handler for signal 33 when the process creates its first thread (and
//! for signal 32 at its first thread cancellation). A child forked later would
//! therefore start with whatever state that race with its fork had left.
//!
//! So a program that launches others records its launcher's dispositions once,
//! before any thread exists: its executable lists
//! [`record_launch_dispositions`] in its preinit array, which the dynamic
//! loader runs before every constructor. A library must not record. It may be
//! loaded into another program, such as a preload in a guest, and the record's
//! queries would then be that program's own system calls. A process that did
//! not record at start records at its first launch. Every launched program is
//! given the table before it executes, and a backend that emulates a program's
//! dispositions starts from the same table ([`ignored_at_launch`]).
//!
//! The table has three named exceptions:
//!
//! - SIGPIPE always starts at its default. Whether a program dies on a broken
//!   pipe must not depend on who launched the launcher, and a launcher's
//!   ignored SIGPIPE is usually its runtime's default rather than anyone's
//!   intent.
//! - SIGTTIN and SIGTTOU always start ignored. On a background read of its
//!   controlling terminal, or a background write with `TOSTOP` set, Linux
//!   sends the process group SIGTTIN or SIGTTOU and restarts the call. A Tool
//!   that holds such a signal until the interrupted call returns never sees
//!   it return: the restarted call raises the signal again, and the program
//!   spins forever.
//!
//!   Measured on 2026-11-18 under ptrace. A program opened a pseudoterminal,
//!   made it a session's controlling terminal, and had a child in a
//!   background process group read from it. The tracer logged "SIGTTIN
//!   replaces held SIGTTIN in the single pending-signal slot" 457,921 times,
//!   until the run was killed at its output cap.
//!
//!   With the signal ignored, Linux fails the read with EIO and lets the
//!   write through, so the program goes on. Every backend starts from this
//!   table. A backend that delivers a held job-control signal as a stop can
//!   drop the exception. A traced process that never executes a program, so
//!   that the table never reaches it, ignores the two signals through
//!   [`ignore_job_control_signals`].

use core::sync::atomic::AtomicBool;
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

/// The signals the launcher left ignored, once [`RECORDED`] is set.
static IGNORED: AtomicU64 = AtomicU64::new(0);
static RECORDED: AtomicBool = AtomicBool::new(false);

/// Records the signals this process's launcher left ignored, as the table
/// every program it launches starts from. A program that launches others has
/// its executable call this before any thread exists, from its preinit array:
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
    RECORDED.store(true, Ordering::Release);
}

/// The signals a launched program starts with ignored, as a 64-bit set with
/// signal `n` at bit `n - 1`: those this process's launcher left ignored
/// ([`record_launch_dispositions`]), without SIGPIPE and with SIGTTIN and
/// SIGTTOU (the module's named exceptions). Every other signal starts at its
/// default. A process that did not record at start records on its first call.
pub fn ignored_at_launch() -> u64 {
    if !RECORDED.load(Ordering::Acquire) {
        record_launch_dispositions();
    }
    let launcher = IGNORED.load(Ordering::Relaxed);
    (launcher & !bit(libc::SIGPIPE)) | bit(libc::SIGTTIN) | bit(libc::SIGTTOU)
}

/// Ignores SIGTTIN and SIGTTOU in the calling process, the table's job-control
/// exception, for a traced process that keeps running its launcher's code
/// instead of executing a program, which the table never reaches. Every other
/// disposition stays as the process inherited it. It allocates nothing, takes
/// no lock and makes only raw system calls, so it can run between fork and
/// the traced code.
pub fn ignore_job_control_signals() -> Result<(), Errno> {
    for signal in [libc::SIGTTIN, libc::SIGTTOU] {
        let action = KernelSigaction::with_handler(libc::SIG_IGN);
        // SAFETY: `action` is a valid `struct sigaction`; no old action.
        unsafe { rt_sigaction(signal, &action, core::ptr::null_mut()) }?;
    }
    Ok(())
}

/// Gives the calling process the dispositions a launched program starts with:
/// each signal in `ignored`, which the parent took from [`ignored_at_launch`]
/// before the fork, ignored, and every other at its default. SIGKILL and
/// SIGSTOP cannot be changed and are left alone.
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

    /// This test binary records at start, as the executable of a program
    /// that launches others does.
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

    /// A copy of this test binary starts with [`LAUNCHER`] ignored, and its
    /// preinit array records that table. The copy then changes its own
    /// dispositions, as a launcher does while it runs. It installs a handler
    /// for signal 33, as glibc's first `pthread_create` does, and one for
    /// SIGUSR2, and it ignores SIGUSR1. Only after that does it ask for the
    /// table, so a table read at first use instead of at start would be the
    /// changed one. A program it launches still starts with the recorded
    /// table, without SIGPIPE and with SIGTTIN and SIGTTOU.
    #[test]
    fn a_launched_program_starts_with_its_launchers_recorded_table() {
        let name =
            "start_signals::tests::a_launched_program_starts_with_its_launchers_recorded_table";
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
        let expected = bit(libc::SIGUSR2) | bit(33) | bit(libc::SIGTTIN) | bit(libc::SIGTTOU);
        assert_eq!(ignored_at_launch(), expected);

        let output = tempfile::NamedTempFile::new().unwrap();
        let mut child = crate::Command::new("/bin/grep")
            .args(["SigIgn", "/proc/self/status"])
            .stdout(output.reopen().unwrap())
            .spawn()
            .unwrap();
        assert_eq!(child.wait_blocking().unwrap(), crate::ExitStatus::Exited(0));
        let line = std::fs::read_to_string(output.path()).unwrap();
        let shown = line
            .trim()
            .strip_prefix("SigIgn:")
            .map(str::trim)
            .and_then(|hex| u64::from_str_radix(hex, 16).ok());
        assert_eq!(shown, Some(expected), "{line}");
    }
}
