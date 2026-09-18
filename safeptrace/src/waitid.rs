/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! Provide `waitid` which is based on `SYS_waitid` syscall.
//! `SYS_waitid` provide `WNOWAIT` flag which is absent in `SYS_waitpid`.
//! compare to `waitpid`, flags *must* be explicitly provided.
//! which could be a combination (bitwise-or) of `WEXITED`, `WSTOPPED`,
//! `WCONTINUED`, `WNOHANG` and `WNOWAIT`. see `waitid(2)` for more details.
//! NB: `waitid` here provide a similar interface as `nix`'s `waitpid`.

use std::mem::MaybeUninit;
use std::os::unix::io::RawFd;

use nix::sys::signal::Signal;
use nix::sys::wait::WaitPidFlag;
use nix::sys::wait::WaitStatus;
use nix::unistd::Pid;

use super::Errno;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum IdType {
    #[allow(unused)]
    Pid(Pid),
    Pgid(Pid),
    #[allow(unused)]
    Pidfd(RawFd), // this requires linux kernel >= 5.4
    All,
}

#[inline]
fn exact_status_signal(status: i32) -> Result<Signal, Errno> {
    // This is the exact domain of nix::WaitStatus, not all Linux signal
    // numbers: nix 0.31 has no typed SIGRTMIN..SIGRTMAX variants. Retain those
    // as fail-closed conversion errors until the public wait API grows a raw
    // signal representation; never alias them to a named standard signal.
    Signal::try_from(status).map_err(|_| Errno::EPROTO)
}

#[inline]
fn exact_job_control_stop_signal(status: i32) -> Result<Signal, Errno> {
    let signal = exact_status_signal(status)?;
    matches!(
        signal,
        Signal::SIGSTOP | Signal::SIGTSTP | Signal::SIGTTIN | Signal::SIGTTOU
    )
    .then_some(signal)
    .ok_or(Errno::EPROTO)
}

#[inline]
fn exact_ptrace_stop_signal(status: i32) -> Result<Signal, Errno> {
    let signal = exact_status_signal(status)?;
    (signal != Signal::SIGKILL)
        .then_some(signal)
        .ok_or(Errno::EPROTO)
}

#[inline]
fn exact_terminating_signal(status: i32) -> Result<Signal, Errno> {
    let signal = exact_status_signal(status)?;
    (!matches!(
        signal,
        Signal::SIGCHLD
            | Signal::SIGCONT
            | Signal::SIGSTOP
            | Signal::SIGTSTP
            | Signal::SIGTTIN
            | Signal::SIGTTOU
            | Signal::SIGURG
            | Signal::SIGWINCH
    ))
    .then_some(signal)
    .ok_or(Errno::EPROTO)
}

#[inline]
fn exact_core_dump_signal(status: i32) -> Result<Signal, Errno> {
    let signal = exact_terminating_signal(status)?;
    matches!(
        signal,
        Signal::SIGQUIT
            | Signal::SIGILL
            | Signal::SIGTRAP
            | Signal::SIGABRT
            | Signal::SIGBUS
            | Signal::SIGFPE
            | Signal::SIGSEGV
            | Signal::SIGXCPU
            | Signal::SIGXFSZ
            | Signal::SIGSYS
    )
    .then_some(signal)
    .ok_or(Errno::EPROTO)
}

#[inline]
fn exact_exit_status(status: i32) -> Result<i32, Errno> {
    (0..=u8::MAX as i32)
        .contains(&status)
        .then_some(status)
        .ok_or(Errno::EPROTO)
}

#[inline]
pub(crate) fn physical_trapped_status(status: i32) -> Result<(Signal, i32), Errno> {
    let status = u32::try_from(status).map_err(|_| Errno::EPROTO)?;
    if status > 0x00ff_ffff {
        return Err(Errno::EPROTO);
    }
    let signal = exact_status_signal((status & 0xff) as i32)?;
    let event = (status >> 8) as i32;
    if event == 0 {
        return Ok((exact_ptrace_stop_signal(signal as i32)?, event));
    }
    if event == libc::PTRACE_EVENT_STOP as i32 {
        return matches!(
            signal,
            Signal::SIGTRAP | Signal::SIGSTOP | Signal::SIGTSTP | Signal::SIGTTIN | Signal::SIGTTOU
        )
        .then_some((signal, event))
        .ok_or(Errno::EPROTO);
    }
    let supported = matches!(
        event,
        value if value == libc::PTRACE_EVENT_FORK as i32
            || value == libc::PTRACE_EVENT_VFORK as i32
            || value == libc::PTRACE_EVENT_CLONE as i32
            || value == libc::PTRACE_EVENT_EXEC as i32
            || value == libc::PTRACE_EVENT_VFORK_DONE as i32
            || value == libc::PTRACE_EVENT_EXIT as i32
            || value == libc::PTRACE_EVENT_SECCOMP as i32
    );
    if supported && signal == Signal::SIGTRAP {
        Ok((signal, event))
    } else {
        Err(Errno::EPROTO)
    }
}

/// Validate one compact `waitpid` status against the same exact signal/event
/// vocabulary as retained `waitid` siginfo, then return its typed nix form.
pub(crate) fn physical_raw_wait_status(pid: i32, raw: i32) -> Result<WaitStatus, Errno> {
    if pid <= 0 || raw < 0 {
        return Err(Errno::EPROTO);
    }
    let pid = Pid::from_raw(pid);
    let low_seven = raw & 0x7f;
    let low_byte = raw & 0xff;
    let expected = if low_seven == 0 {
        let code = (raw >> 8) & 0xff;
        if raw != code << 8 {
            return Err(Errno::EPROTO);
        }
        WaitStatus::Exited(pid, code)
    } else if low_byte == 0x7f {
        let signal = (raw >> 8) & 0xff;
        let event = raw >> 16;
        if event == 0 && signal == (libc::SIGTRAP | 0x80) {
            WaitStatus::PtraceSyscall(pid)
        } else if event == 0 {
            WaitStatus::Stopped(pid, exact_ptrace_stop_signal(signal)?)
        } else {
            let si_status = event
                .checked_shl(8)
                .and_then(|event| event.checked_add(signal))
                .ok_or(Errno::EPROTO)?;
            let (signal, event) = physical_trapped_status(si_status)?;
            WaitStatus::PtraceEvent(pid, signal, event)
        }
    } else if low_seven != 0x7f {
        let signal = low_seven;
        let core_dumped = raw & 0x80 != 0;
        if core_dumped {
            exact_core_dump_signal(signal)?;
        } else {
            exact_terminating_signal(signal)?;
        }
        let expected_raw = signal | if core_dumped { 0x80 } else { 0 };
        if raw != expected_raw {
            return Err(Errno::EPROTO);
        }
        WaitStatus::Signaled(pid, exact_status_signal(signal)?, core_dumped)
    } else {
        // nix::WaitStatus::from_raw asserts that this must be the exact
        // continued status. Continued statuses are outside this observer's
        // accepted pre-registration vocabulary, so reject before calling it.
        return Err(Errno::EPROTO);
    };

    let decoded = WaitStatus::from_raw(pid, raw).map_err(|_| Errno::EPROTO)?;
    (decoded == expected)
        .then_some(decoded)
        .ok_or(Errno::EPROTO)
}

/// Returns the raw siginfo from a waitid call.
fn waitid_si(waitid_type: IdType, flags: WaitPidFlag) -> Result<libc::siginfo_t, Errno> {
    let mut siginfo = MaybeUninit::<libc::siginfo_t>::zeroed();
    let siginfo_ptr: *mut libc::siginfo_t = siginfo.as_mut_ptr();

    let (id_type, pid_or_pidfd) = match waitid_type {
        IdType::Pid(pid) => (libc::P_PID, pid.as_raw()),
        IdType::Pgid(pid) => (libc::P_PGID, pid.as_raw()),
        IdType::Pidfd(raw_fd) => (libc::P_PIDFD, raw_fd),
        IdType::All => (libc::P_ALL, -1),
    };

    Errno::result(unsafe {
        libc::waitid(
            id_type,
            pid_or_pidfd as libc::id_t,
            siginfo_ptr,
            flags.bits(),
        )
    })?;

    Ok(unsafe { siginfo.assume_init() })
}

/// `waitid(P_PIDFD)` with the original wait-status bit layout preserved.
///
/// In particular, ptrace event numbers in the high 16 bits of `si_status`
/// must survive so the notifier can decode `PTRACE_EVENT_*` losslessly.
#[cfg(all(feature = "notifier", test))]
pub fn waitpidfd(raw_fd: RawFd, flags: WaitPidFlag) -> Result<Option<i32>, Errno> {
    waitpidfd_raw(raw_fd, flags).and_then(|raw| raw.status())
}

/// A successful raw `waitid(P_PIDFD)` result retained before status
/// conversion. Keeping the `siginfo_t` intact lets the optional physical
/// observer record provenance when later typed conversion rejects an
/// unexpected kernel code.
#[cfg(feature = "notifier")]
pub(crate) struct WaitPidfdRaw(libc::siginfo_t);

#[cfg(feature = "notifier")]
impl WaitPidfdRaw {
    #[cfg(test)]
    pub(crate) fn override_code_for_test(&mut self, code: i32) {
        self.0.si_code = code;
    }

    pub(crate) fn pid(&self) -> i32 {
        unsafe { self.0.si_pid() }
    }

    pub(crate) fn uid(&self) -> u32 {
        unsafe { self.0.si_uid() }
    }

    pub(crate) fn status_value(&self) -> i32 {
        unsafe { self.0.si_status() }
    }

    pub(crate) fn signo(&self) -> i32 {
        self.0.si_signo
    }

    pub(crate) fn errno(&self) -> i32 {
        self.0.si_errno
    }

    pub(crate) fn code(&self) -> i32 {
        self.0.si_code
    }

    pub(crate) fn status(&self) -> Result<Option<i32>, Errno> {
        physical_wait_siginfo_to_status(
            self.signo(),
            self.errno(),
            self.code(),
            self.pid(),
            self.uid(),
            self.status_value(),
        )
    }
}

#[cfg(feature = "notifier")]
pub(crate) fn waitpidfd_raw(raw_fd: RawFd, flags: WaitPidFlag) -> Result<WaitPidfdRaw, Errno> {
    waitid_si(IdType::Pidfd(raw_fd), flags).map(WaitPidfdRaw)
}

/// Exact result domain of [`WaitPidfdRaw::status`] for retained raw fields.
/// A zero PID is the kernel's `WNOHANG` no-status result and bypasses typed
/// conversion exactly as it does on the production path.
pub(crate) fn physical_wait_siginfo_to_status(
    signo: i32,
    errno: i32,
    code: i32,
    pid: i32,
    uid: u32,
    si_status: i32,
) -> Result<Option<i32>, Errno> {
    if pid == 0 {
        if signo == 0 && errno == 0 && code == 0 && uid == 0 && si_status == 0 {
            Ok(None)
        } else {
            Err(Errno::EPROTO)
        }
    } else {
        physical_siginfo_to_status(signo, errno, code, pid, si_status).map(Some)
    }
}

/// Applies the exact production waitid-to-compact-status predicate to retained
/// physical siginfo fields. The observer uses this same function to prove that
/// an `UndecodableStatus` record really was rejected by the typed converter;
/// keeping a single predicate prevents tests from manufacturing EPROTO for a
/// siginfo tuple that production would accept.
pub(crate) fn physical_siginfo_to_status(
    signo: i32,
    errno: i32,
    code: i32,
    pid: i32,
    si_status: i32,
) -> Result<i32, Errno> {
    validate_wait_siginfo_fields(signo, errno, pid)?;

    let status = match code {
        libc::CLD_EXITED => (exact_exit_status(si_status)? as u32).wrapping_shl(8) as i32,
        libc::CLD_KILLED => exact_terminating_signal(si_status)? as i32,
        libc::CLD_DUMPED => exact_core_dump_signal(si_status)? as i32 | 0x80,
        libc::CLD_TRAPPED => {
            if si_status != (0x80 | Signal::SIGTRAP as i32) {
                physical_trapped_status(si_status)?;
            }
            ((si_status as u32).wrapping_shl(8) | 0x7f) as i32
        }
        libc::CLD_STOPPED => (exact_job_control_stop_signal(si_status)? as i32) << 8 | 0x7f,
        libc::CLD_CONTINUED if si_status == libc::SIGCONT => 0xffff,
        libc::CLD_CONTINUED => return Err(Errno::EPROTO),
        _ => return Err(Errno::EPROTO),
    };

    let compact = WaitStatus::from_raw(Pid::from_raw(pid), status).map_err(|_| Errno::EPROTO)?;
    if physical_siginfo_to_waitstatus(signo, errno, code, pid, si_status)? != compact {
        return Err(Errno::EPROTO);
    }

    Ok(status)
}

fn validate_wait_siginfo_fields(signo: i32, errno: i32, pid: i32) -> Result<(), Errno> {
    if signo != libc::SIGCHLD || errno != 0 || pid <= 0 {
        return Err(Errno::EPROTO);
    }
    Ok(())
}

fn siginfo_to_waitstatus(si: libc::siginfo_t) -> Result<WaitStatus, Errno> {
    physical_siginfo_to_waitstatus(
        si.si_signo,
        si.si_errno,
        si.si_code,
        unsafe { si.si_pid() },
        unsafe { si.si_status() },
    )
}

fn physical_siginfo_to_waitstatus(
    signo: i32,
    errno: i32,
    code: i32,
    raw_pid: i32,
    si_status: i32,
) -> Result<WaitStatus, Errno> {
    validate_wait_siginfo_fields(signo, errno, raw_pid)?;
    let pid = Pid::from_raw(raw_pid);
    Ok(match code {
        libc::CLD_EXITED => WaitStatus::Exited(pid, exact_exit_status(si_status)?),
        libc::CLD_KILLED => WaitStatus::Signaled(pid, exact_terminating_signal(si_status)?, false),
        libc::CLD_DUMPED => WaitStatus::Signaled(pid, exact_core_dump_signal(si_status)?, true),
        libc::CLD_STOPPED => WaitStatus::Stopped(pid, exact_job_control_stop_signal(si_status)?),
        libc::CLD_TRAPPED if si_status == (0x80 | Signal::SIGTRAP as i32) => {
            WaitStatus::PtraceSyscall(pid)
        }
        libc::CLD_TRAPPED => {
            let (trap_sig, event) = physical_trapped_status(si_status)?;
            if event == 0 {
                WaitStatus::Stopped(pid, trap_sig)
            } else {
                WaitStatus::PtraceEvent(pid, trap_sig, event)
            }
        }
        libc::CLD_CONTINUED if si_status == libc::SIGCONT => WaitStatus::Continued(pid),
        _ => return Err(Errno::EPROTO),
    })
}

/// waitid as to SYS_waitid.
/// return
///   - Err when syscall returns -1.
///   - OK(WaitStatus::StillAlive) when no state change
///   - OK(WaitStatus::...) when state has changed.
pub fn waitid(waitid_type: IdType, flags: WaitPidFlag) -> Result<WaitStatus, Errno> {
    let siginfo = waitid_si(waitid_type, flags)?;

    if unsafe { siginfo.si_pid() } == 0 {
        Ok(WaitStatus::StillAlive)
    } else {
        siginfo_to_waitstatus(siginfo)
    }
}

#[cfg(test)]
mod tests {
    #[cfg(feature = "notifier")]
    use std::os::fd::AsRawFd;
    #[cfg(feature = "notifier")]
    use std::os::fd::FromRawFd;
    #[cfg(feature = "notifier")]
    use std::os::fd::OwnedFd;

    use nix::sys::signal::Signal;
    use nix::sys::wait::WaitPidFlag;
    use nix::unistd;
    use nix::unistd::ForkResult;

    use super::*;

    #[test]
    fn exact_siginfo_oracle_rejects_malformed_signal_and_continued_statuses() {
        let pid = 101;
        assert_eq!(
            physical_siginfo_to_status(libc::SIGCHLD, 0, libc::CLD_KILLED, pid, libc::SIGTERM,),
            Ok(libc::SIGTERM)
        );
        assert_eq!(
            physical_siginfo_to_status(libc::SIGCHLD, 0, libc::CLD_DUMPED, pid, libc::SIGABRT,),
            Ok(libc::SIGABRT | 0x80)
        );
        for stop_signal in [libc::SIGSTOP, libc::SIGTSTP, libc::SIGTTIN, libc::SIGTTOU] {
            assert_eq!(
                physical_siginfo_to_status(libc::SIGCHLD, 0, libc::CLD_STOPPED, pid, stop_signal,),
                Ok((stop_signal << 8) | 0x7f)
            );
        }
        assert_eq!(
            physical_siginfo_to_status(libc::SIGCHLD, 0, libc::CLD_CONTINUED, pid, libc::SIGCONT,),
            Ok(0xffff)
        );

        for code in [libc::CLD_KILLED, libc::CLD_DUMPED, libc::CLD_STOPPED] {
            for malformed in [0, libc::SIGTERM + 0x100] {
                assert_eq!(
                    physical_siginfo_to_status(libc::SIGCHLD, 0, code, pid, malformed),
                    Err(Errno::EPROTO),
                    "code {code} accepted malformed si_status {malformed:#x}"
                );
            }
        }
        for nonterminating in [
            libc::SIGCHLD,
            libc::SIGCONT,
            libc::SIGSTOP,
            libc::SIGTSTP,
            libc::SIGTTIN,
            libc::SIGTTOU,
            libc::SIGURG,
            libc::SIGWINCH,
        ] {
            for code in [libc::CLD_KILLED, libc::CLD_DUMPED] {
                assert_eq!(
                    physical_siginfo_to_status(libc::SIGCHLD, 0, code, pid, nonterminating),
                    Err(Errno::EPROTO),
                    "terminal code {code} accepted nonterminating signal {nonterminating}"
                );
            }
        }
        for noncore in [libc::SIGTERM, libc::SIGKILL, libc::SIGPIPE] {
            assert_eq!(
                physical_siginfo_to_status(libc::SIGCHLD, 0, libc::CLD_DUMPED, pid, noncore),
                Err(Errno::EPROTO),
                "CLD_DUMPED accepted non-core signal {noncore}"
            );
        }
        assert_eq!(
            physical_siginfo_to_status(libc::SIGCHLD, 0, libc::CLD_STOPPED, pid, libc::SIGTERM),
            Err(Errno::EPROTO),
            "CLD_STOPPED accepted a terminating signal"
        );
        for malformed in [0, libc::SIGTERM, libc::SIGCONT + 0x100] {
            assert_eq!(
                physical_siginfo_to_status(libc::SIGCHLD, 0, libc::CLD_CONTINUED, pid, malformed,),
                Err(Errno::EPROTO),
                "CLD_CONTINUED accepted malformed si_status {malformed:#x}"
            );
        }
    }

    #[test]
    fn exact_siginfo_oracle_rejects_out_of_domain_exit_and_trap_statuses() {
        let pid = 101;

        for exit_status in [0, u8::MAX as i32] {
            assert_eq!(
                physical_siginfo_to_status(libc::SIGCHLD, 0, libc::CLD_EXITED, pid, exit_status,),
                Ok(exit_status << 8)
            );
        }
        for malformed in [-1, u8::MAX as i32 + 1, i32::MIN, i32::MAX] {
            assert_eq!(
                physical_siginfo_to_status(libc::SIGCHLD, 0, libc::CLD_EXITED, pid, malformed,),
                Err(Errno::EPROTO),
                "CLD_EXITED accepted malformed si_status {malformed:#x}"
            );
        }

        let plain_trap = Signal::SIGTRAP as i32;
        let syscall_trap = 0x80 | plain_trap;
        let event_traps = [
            libc::PTRACE_EVENT_FORK,
            libc::PTRACE_EVENT_VFORK,
            libc::PTRACE_EVENT_CLONE,
            libc::PTRACE_EVENT_EXEC,
            libc::PTRACE_EVENT_VFORK_DONE,
            libc::PTRACE_EVENT_EXIT,
            libc::PTRACE_EVENT_SECCOMP,
        ]
        .map(|event| ((event as i32) << 8) | plain_trap);
        let stop_events = [
            libc::SIGTRAP,
            libc::SIGSTOP,
            libc::SIGTSTP,
            libc::SIGTTIN,
            libc::SIGTTOU,
        ]
        .map(|signal| ((libc::PTRACE_EVENT_STOP as i32) << 8) | signal);
        for trapped_status in [plain_trap, syscall_trap]
            .into_iter()
            .chain(event_traps)
            .chain(stop_events)
        {
            assert_eq!(
                physical_siginfo_to_status(
                    libc::SIGCHLD,
                    0,
                    libc::CLD_TRAPPED,
                    pid,
                    trapped_status,
                ),
                Ok((trapped_status << 8) | 0x7f)
            );
        }
        assert_eq!(
            physical_siginfo_to_waitstatus(libc::SIGCHLD, 0, libc::CLD_TRAPPED, pid, plain_trap,),
            Ok(WaitStatus::Stopped(Pid::from_raw(pid), Signal::SIGTRAP))
        );
        assert_eq!(
            physical_siginfo_to_waitstatus(libc::SIGCHLD, 0, libc::CLD_TRAPPED, pid, syscall_trap,),
            Ok(WaitStatus::PtraceSyscall(Pid::from_raw(pid)))
        );
        assert_eq!(
            physical_siginfo_to_waitstatus(
                libc::SIGCHLD,
                0,
                libc::CLD_TRAPPED,
                pid,
                event_traps[5],
            ),
            Ok(WaitStatus::PtraceEvent(
                Pid::from_raw(pid),
                Signal::SIGTRAP,
                libc::PTRACE_EVENT_EXIT as i32,
            ))
        );

        for malformed in [
            0,
            0xff,
            libc::SIGKILL,
            (8 << 8) | plain_trap,
            ((libc::PTRACE_EVENT_FORK as i32) << 8) | libc::SIGSTOP,
            ((libc::PTRACE_EVENT_STOP as i32) << 8) | libc::SIGTERM,
            0x0100_0000 | plain_trap,
            -1,
            i32::MAX,
        ] {
            assert_eq!(
                physical_siginfo_to_status(libc::SIGCHLD, 0, libc::CLD_TRAPPED, pid, malformed,),
                Err(Errno::EPROTO),
                "CLD_TRAPPED accepted malformed si_status {malformed:#x}"
            );
        }
    }

    #[test]
    fn exact_raw_wait_oracle_rejects_impossible_or_noncanonical_shapes() {
        let pid = 101;
        for raw in [
            0,
            libc::SIGTERM,
            libc::SIGABRT | 0x80,
            (libc::SIGSTOP << 8) | 0x7f,
            ((libc::SIGTRAP | 0x80) << 8) | 0x7f,
            (libc::PTRACE_EVENT_FORK << 16) | (libc::SIGTRAP << 8) | 0x7f,
            (libc::PTRACE_EVENT_STOP << 16) | (libc::SIGTSTP << 8) | 0x7f,
        ] {
            assert!(
                physical_raw_wait_status(pid, raw).is_ok(),
                "rejected canonical raw wait status {raw:#x}"
            );
        }
        for malformed in [
            0x01ff, // malformed low byte that makes nix::from_raw assert
            (libc::SIGKILL << 8) | 0x7f,
            (0xff << 8) | 0x7f,
            (8 << 16) | (libc::SIGTRAP << 8) | 0x7f,
            (libc::PTRACE_EVENT_FORK << 16) | (libc::SIGSTOP << 8) | 0x7f,
            (libc::PTRACE_EVENT_STOP << 16) | (libc::SIGTERM << 8) | 0x7f,
            (libc::SIGSTOP | 0x80),
            i32::MAX,
            -1,
        ] {
            assert_eq!(
                physical_raw_wait_status(pid, malformed),
                Err(Errno::EPROTO),
                "accepted impossible or noncanonical raw wait status {malformed:#x}"
            );
        }
    }

    #[test]
    fn exact_wait_oracle_fails_closed_on_valid_unsupported_realtime_signals() {
        let pid = 101;
        // Linux x86-64 SIGRTMIN is a valid termination or ptrace-stop signal,
        // but nix 0.31's typed Signal cannot represent it. These are support
        // refusals, not malformed kernel statuses.
        let sig_rtmin = 34;
        for raw in [sig_rtmin, (sig_rtmin << 8) | 0x7f] {
            assert_eq!(
                physical_raw_wait_status(pid, raw),
                Err(Errno::EPROTO),
                "accepted valid RT signal that the typed observer cannot represent"
            );
        }
        for code in [libc::CLD_KILLED, libc::CLD_TRAPPED] {
            assert_eq!(
                physical_siginfo_to_status(libc::SIGCHLD, 0, code, pid, sig_rtmin),
                Err(Errno::EPROTO),
                "accepted valid RT signal that the typed waitid API cannot represent"
            );
        }
    }

    #[test]
    fn physical_raw_wait_oracle_excludes_valid_continued_status_by_vocabulary() {
        let pid = 101;
        assert_eq!(
            WaitStatus::from_raw(Pid::from_raw(pid), 0xffff),
            Ok(WaitStatus::Continued(Pid::from_raw(pid)))
        );
        assert_eq!(physical_raw_wait_status(pid, 0xffff), Err(Errno::EPROTO));
    }

    #[cfg(feature = "notifier")]
    #[test]
    fn waitpidfd_preserves_direct_child_stop_status() {
        let fork_result = unsafe { unistd::fork() }.expect("fork direct-child stop test");
        match fork_result {
            ForkResult::Parent { child, .. } => {
                let raw_fd =
                    unsafe { libc::syscall(libc::SYS_pidfd_open, child.as_raw(), 0) } as i32;
                assert!(
                    raw_fd >= 0,
                    "pidfd_open failed: {}",
                    std::io::Error::last_os_error()
                );
                let pidfd = unsafe { OwnedFd::from_raw_fd(raw_fd) };
                let status = waitpidfd(pidfd.as_raw_fd(), WaitPidFlag::WSTOPPED)
                    .expect("wait direct-child stop through pidfd")
                    .expect("blocking pidfd wait returned no stop");
                assert!(
                    libc::WIFSTOPPED(status),
                    "raw status {status:#x} is not stopped"
                );
                assert_eq!(libc::WSTOPSIG(status), libc::SIGSTOP);

                nix::sys::signal::kill(child, Signal::SIGCONT).expect("resume direct child");
                assert_eq!(
                    waitid(IdType::Pidfd(pidfd.as_raw_fd()), WaitPidFlag::WEXITED),
                    Ok(WaitStatus::Exited(child, 0))
                );
            }
            ForkResult::Child => {
                nix::sys::signal::raise(Signal::SIGSTOP).expect("raise direct-child SIGSTOP");
                unsafe { libc::_exit(0) };
            }
        }
    }

    #[test]
    fn waitid_w_exited_0() {
        let fork_result = unsafe { unistd::fork() };
        assert!(fork_result.is_ok());
        match fork_result.unwrap() {
            ForkResult::Parent { child, .. } => {
                assert_eq!(
                    waitid(IdType::Pid(child), WaitPidFlag::WEXITED),
                    Ok(WaitStatus::Exited(child, 0))
                );
            }
            ForkResult::Child => {
                let hundred_millies = std::time::Duration::from_millis(100);
                std::thread::sleep(hundred_millies);
                unsafe { libc::syscall(libc::SYS_exit_group, 0) };
            }
        }
    }

    #[test]
    fn waitid_w_exited_1() {
        let fork_result = unsafe { unistd::fork() };
        assert!(fork_result.is_ok());
        match fork_result.unwrap() {
            ForkResult::Parent { child, .. } => {
                assert_eq!(
                    waitid(IdType::Pid(child), WaitPidFlag::WEXITED),
                    Ok(WaitStatus::Exited(child, 1))
                );
            }
            ForkResult::Child => {
                let hundred_millies = std::time::Duration::from_millis(100);
                std::thread::sleep(hundred_millies);
                unsafe { libc::syscall(libc::SYS_exit_group, 1) };
            }
        }
    }

    #[test]
    fn waitid_w_killed_by_signal() {
        let fork_result = unsafe { unistd::fork() };
        assert!(fork_result.is_ok());
        match fork_result.unwrap() {
            ForkResult::Parent { child, .. } => {
                assert!(nix::sys::signal::kill(child, Signal::SIGINT).is_ok());
                assert_eq!(
                    waitid(IdType::Pid(child), WaitPidFlag::WEXITED),
                    Ok(WaitStatus::Signaled(child, Signal::SIGINT, false))
                );
            }
            ForkResult::Child => {
                let one_sec = std::time::Duration::from_millis(1000);
                loop {
                    std::thread::sleep(one_sec);
                }
            }
        }
    }

    #[test]
    fn waitid_w_exited_no_wait_then_wait() {
        let fork_result = unsafe { unistd::fork() };
        assert!(fork_result.is_ok());
        match fork_result.unwrap() {
            ForkResult::Parent { child, .. } => {
                assert_eq!(
                    waitid(
                        IdType::Pid(child),
                        WaitPidFlag::WEXITED | WaitPidFlag::WNOWAIT
                    ),
                    Ok(WaitStatus::Exited(child, 0))
                );
                assert_eq!(
                    waitid(IdType::Pid(child), WaitPidFlag::WEXITED),
                    Ok(WaitStatus::Exited(child, 0))
                );
            }
            ForkResult::Child => {
                let hundred_millies = std::time::Duration::from_millis(100);
                std::thread::sleep(hundred_millies);
                unsafe { libc::syscall(libc::SYS_exit_group, 0) };
            }
        }
    }

    #[test]
    fn waitid_w_exited_then_echild() {
        let fork_result = unsafe { unistd::fork() };
        assert!(fork_result.is_ok());
        match fork_result.unwrap() {
            ForkResult::Parent { child, .. } => {
                assert_eq!(
                    waitid(IdType::Pid(child), WaitPidFlag::WEXITED),
                    Ok(WaitStatus::Exited(child, 0))
                );
                assert_eq!(
                    waitid(IdType::Pid(child), WaitPidFlag::WEXITED),
                    Err(Errno::ECHILD)
                );
            }
            ForkResult::Child => {
                let hundred_millies = std::time::Duration::from_millis(100);
                std::thread::sleep(hundred_millies);
                unsafe { libc::syscall(libc::SYS_exit_group, 0) };
            }
        }
    }

    #[test]
    fn waitid_w_nohang_then_kill() {
        let fork_result = unsafe { unistd::fork() };
        assert!(fork_result.is_ok());
        match fork_result.unwrap() {
            ForkResult::Parent { child, .. } => {
                assert_eq!(
                    waitid(
                        IdType::Pid(child),
                        WaitPidFlag::WEXITED | WaitPidFlag::WNOHANG
                    ),
                    Ok(WaitStatus::StillAlive),
                );
                assert!(nix::sys::signal::kill(child, Signal::SIGINT).is_ok());
                assert_eq!(
                    waitid(IdType::Pid(child), WaitPidFlag::WEXITED),
                    Ok(WaitStatus::Signaled(child, Signal::SIGINT, false))
                );
            }
            ForkResult::Child => {
                let one_sec = std::time::Duration::from_millis(100);
                loop {
                    std::thread::sleep(one_sec);
                }
            }
        }
    }

    #[test]
    fn waitid_w_nohang_kill_nohang_nowait_wait() {
        let fork_result = unsafe { unistd::fork() };
        assert!(fork_result.is_ok());
        match fork_result.unwrap() {
            ForkResult::Parent { child, .. } => {
                assert_eq!(
                    waitid(
                        IdType::Pid(child),
                        WaitPidFlag::WEXITED | WaitPidFlag::WNOHANG
                    ),
                    Ok(WaitStatus::StillAlive),
                );
                assert!(nix::sys::signal::kill(child, Signal::SIGINT).is_ok());
                loop {
                    // this is not very ideal, the loops generally runs 1K - 10K times..
                    let status = waitid(
                        IdType::Pid(child),
                        WaitPidFlag::WEXITED | WaitPidFlag::WNOHANG | WaitPidFlag::WNOWAIT,
                    );
                    assert!(status.is_ok());
                    match status.unwrap() {
                        WaitStatus::StillAlive => {}
                        waitid_nohang_nowait => {
                            assert_eq!(
                                waitid_nohang_nowait,
                                WaitStatus::Signaled(child, Signal::SIGINT, false)
                            );
                            break;
                        }
                    }
                }
                assert_eq!(
                    waitid(IdType::Pid(child), WaitPidFlag::WEXITED),
                    Ok(WaitStatus::Signaled(child, Signal::SIGINT, false))
                );
            }
            ForkResult::Child => {
                let one_sec = std::time::Duration::from_millis(100);
                loop {
                    std::thread::sleep(one_sec);
                }
            }
        }
    }
}
