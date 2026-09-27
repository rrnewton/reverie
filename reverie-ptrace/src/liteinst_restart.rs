/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! Pure decisions for restarting a LiteInst host-hybrid syscall trap.
//!
//! A host-hybrid syscall is served while the controller thread is stopped at
//! the runtime's `int3`, with `orig_rax == -1`. Linux therefore never applies
//! its own syscall-restart rule to that stop, and writing a private
//! `-ERESTART*` code into the guest frame would leak it to the guest. Instead
//! the tracer rewinds the controller to the `int3`: signal work happens on the
//! resume, and the re-executed `int3` re-traps and re-dispatches the syscall.
//! The helpers here decide when that is allowed, without touching a tracee.

use nix::sys::signal::Signal;
use reverie::Errno;

/// How a restartable syscall result rewrites the frame before the re-trap.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RestartAction {
    /// Re-dispatch the same syscall number with the same arguments.
    Same,
    /// Re-dispatch as `restart_syscall`, keeping the argument registers, as
    /// Linux does for `-ERESTART_RESTARTBLOCK`.
    RestartSyscall,
}

/// Classifies a final syscall result as a Linux-private restart request.
///
/// Only the four codes Linux itself restarts return an action. Every other
/// result, including an ordinary errno such as `EINTR`, is delivered as is.
pub(crate) fn liteinst_restart_action(result: Result<i64, Errno>) -> Option<RestartAction> {
    match result {
        Err(Errno::ERESTARTSYS) | Err(Errno::ERESTARTNOINTR) | Err(Errno::ERESTARTNOHAND) => {
            Some(RestartAction::Same)
        }
        Err(Errno::ERESTART_RESTARTBLOCK) => Some(RestartAction::RestartSyscall),
        _ => None,
    }
}

/// What a single step of the private-page `syscall` instruction observed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PrivateStep {
    /// The stop is still at the `syscall` instruction: a signal was already
    /// pending when the step began, so the syscall never executed.
    NotRun,
    /// The single-step report after the `syscall` instruction. Its `rax` is
    /// the raw kernel result, which may itself be a restart code whose
    /// interrupting signal is still kernel-pending.
    Ran,
    /// Any other stop. A non-SIGTRAP stop after the instruction can only be a
    /// kernel-generated synchronous signal dequeued ahead of the single-step
    /// report. That report is still queued: resuming would surface it at the
    /// restored controller registers, where it is indistinguishable from a
    /// fresh syscall trap and would re-execute the syscall.
    Unexpected,
}

/// Classifies the signal-delivery stop that ended a private-page step.
pub(crate) fn classify_private_step(
    ip: u64,
    signal: Signal,
    private_syscall: u64,
    syscall_len: u64,
) -> PrivateStep {
    if ip == private_syscall {
        PrivateStep::NotRun
    } else if signal == Signal::SIGTRAP && Some(ip) == private_syscall.checked_add(syscall_len) {
        PrivateStep::Ran
    } else {
        PrivateStep::Unexpected
    }
}

/// The single-byte x86 `int3` opcode.
pub(crate) const INT3: u8 = 0xcc;

/// Checks that the controller is stopped immediately after the runtime `int3`
/// with the syscall marker still in `rax`, so rewinding one byte re-executes
/// exactly that `int3` and nothing else.
pub(crate) fn check_rewind_preconditions(
    ip: u64,
    rax: u64,
    restart_rip: u64,
    syscall_marker: u64,
    restart_byte: u8,
) -> Result<(), String> {
    if Some(ip) != restart_rip.checked_add(1) {
        return Err(format!(
            "controller RIP {ip:#x} is not immediately after the runtime int3 at {restart_rip:#x}"
        ));
    }
    if rax != syscall_marker {
        return Err(format!(
            "controller RAX {rax:#x} no longer holds the syscall marker {syscall_marker:#x}"
        ));
    }
    if restart_byte != INT3 {
        return Err(format!(
            "byte {restart_byte:#04x} at {restart_rip:#x} is not the runtime int3"
        ));
    }
    Ok(())
}

/// Signal masks read from `/proc/<tid>/status`.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct SignalStatus {
    /// `SigPnd`: signals pending for this thread.
    pub(crate) thread_pending: u64,
    /// `ShdPnd`: signals pending for the whole thread group.
    pub(crate) shared_pending: u64,
    /// `SigBlk`: signals this thread blocks.
    pub(crate) blocked: u64,
    /// `SigCgt`: signals with an installed handler (process-wide).
    pub(crate) caught: u64,
}

/// Parses the four signal masks from the text of `/proc/<tid>/status`.
pub(crate) fn parse_signal_status(text: &str) -> Option<SignalStatus> {
    let mut thread_pending = None;
    let mut shared_pending = None;
    let mut blocked = None;
    let mut caught = None;
    for line in text.lines() {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let slot = match key {
            "SigPnd" => &mut thread_pending,
            "ShdPnd" => &mut shared_pending,
            "SigBlk" => &mut blocked,
            "SigCgt" => &mut caught,
            _ => continue,
        };
        *slot = Some(u64::from_str_radix(value.trim(), 16).ok()?);
    }
    Some(SignalStatus {
        thread_pending: thread_pending?,
        shared_pending: shared_pending?,
        blocked: blocked?,
        caught: caught?,
    })
}

/// The `/proc` mask bit for one signal number.
pub(crate) const fn signal_bit(signal: i32) -> u64 {
    1u64 << (signal - 1)
}

/// Handlers the host-hybrid runtime itself installs.
///
/// `initialize_host_runtime` calls `liteinst2::patcher::prepare_live_patching`,
/// which installs the SIGTRAP guard router. A plain ptrace run has no such
/// handler, so for ptrace equivalence it is not a guest handler.
pub(crate) const RUNTIME_OWNED_HANDLERS: u64 = signal_bit(libc::SIGTRAP);

/// Returns the signals that make the no-handler restart rule unsound.
///
/// Linux restarts `-ERESTARTSYS`, `-ERESTARTNOHAND` and
/// `-ERESTART_RESTARTBLOCK` unconditionally only when the delivered signal has
/// no handler; with a guest handler the result depends on the handler's
/// `SA_RESTART` flag or becomes `-EINTR`, and the tracer cannot see
/// `SA_RESTART`. `-ERESTARTNOINTR` restarts even with a handler, so it never
/// conflicts. The deliverable set is the tracer-held `pending_signal` plus
/// every unblocked pending signal; a non-empty intersection with guest
/// handlers is returned as a mask.
pub(crate) fn restart_handler_conflict(
    errno: Errno,
    status: SignalStatus,
    pending_signal: Option<Signal>,
    runtime_owned: u64,
) -> Option<u64> {
    if errno == Errno::ERESTARTNOINTR {
        return None;
    }
    let held = pending_signal.map_or(0, |signal| signal_bit(signal as i32));
    let deliverable = held | ((status.thread_pending | status.shared_pending) & !status.blocked);
    let conflict = deliverable & status.caught & !runtime_owned;
    (conflict != 0).then_some(conflict)
}

#[cfg(test)]
mod tests {
    use super::*;

    const PRIVATE: u64 = 0x7000_0000;

    #[test]
    fn restart_action_covers_every_linux_restart_code() {
        assert_eq!(
            liteinst_restart_action(Err(Errno::ERESTARTSYS)),
            Some(RestartAction::Same)
        );
        assert_eq!(
            liteinst_restart_action(Err(Errno::ERESTARTNOINTR)),
            Some(RestartAction::Same)
        );
        assert_eq!(
            liteinst_restart_action(Err(Errno::ERESTARTNOHAND)),
            Some(RestartAction::Same)
        );
        assert_eq!(
            liteinst_restart_action(Err(Errno::ERESTART_RESTARTBLOCK)),
            Some(RestartAction::RestartSyscall)
        );
    }

    #[test]
    fn restart_action_rejects_ordinary_results() {
        for errno in [Errno::EINTR, Errno::EAGAIN, Errno::EBADF, Errno::ENOSYS] {
            assert_eq!(liteinst_restart_action(Err(errno)), None, "{errno:?}");
        }
        for value in [0, 1, 4243, -1, -512, i64::MAX] {
            assert_eq!(liteinst_restart_action(Ok(value)), None, "{value}");
        }
    }

    #[test]
    fn private_step_distinguishes_not_run_ran_and_unexpected() {
        assert_eq!(
            classify_private_step(PRIVATE, Signal::SIGURG, PRIVATE, 2),
            PrivateStep::NotRun
        );
        // An external SIGTRAP pending before the step still stops before the
        // instruction; it is not a single-step report.
        assert_eq!(
            classify_private_step(PRIVATE, Signal::SIGTRAP, PRIVATE, 2),
            PrivateStep::NotRun
        );
        assert_eq!(
            classify_private_step(PRIVATE + 2, Signal::SIGTRAP, PRIVATE, 2),
            PrivateStep::Ran
        );
        assert_eq!(
            classify_private_step(PRIVATE + 2, Signal::SIGSYS, PRIVATE, 2),
            PrivateStep::Unexpected
        );
        assert_eq!(
            classify_private_step(PRIVATE + 7, Signal::SIGTRAP, PRIVATE, 2),
            PrivateStep::Unexpected
        );
        assert_eq!(
            classify_private_step(u64::MAX, Signal::SIGTRAP, u64::MAX - 1, 2),
            PrivateStep::Unexpected
        );
    }

    #[test]
    fn rewind_preconditions_require_the_exact_int3_stop() {
        let marker = 0x7265_766c_6900_0004;
        assert_eq!(
            check_rewind_preconditions(0x1001, marker, 0x1000, marker, INT3),
            Ok(())
        );
        assert!(check_rewind_preconditions(0x1000, marker, 0x1000, marker, INT3).is_err());
        assert!(check_rewind_preconditions(0x1002, marker, 0x1000, marker, INT3).is_err());
        assert!(check_rewind_preconditions(0x1001, 0, 0x1000, marker, INT3).is_err());
        assert!(check_rewind_preconditions(0x1001, marker, 0x1000, marker, 0x90).is_err());
        assert!(check_rewind_preconditions(0, marker, u64::MAX, marker, INT3).is_err());
    }

    const STATUS: &str = "Name:\tguest\nState:\tt (tracing stop)\nSigQ:\t1/63477\n\
SigPnd:\t0000000000000400\nShdPnd:\t0000000000000001\nSigBlk:\t0000000000000001\n\
SigIgn:\t0000000000001000\nSigCgt:\t0000000000000404\nCapInh:\t0000000000000000\n";

    #[test]
    fn signal_status_parses_the_four_masks() {
        assert_eq!(
            parse_signal_status(STATUS),
            Some(SignalStatus {
                thread_pending: 0x400,
                shared_pending: 0x1,
                blocked: 0x1,
                caught: 0x404,
            })
        );
        assert_eq!(
            parse_signal_status("SigPnd:\t0\nShdPnd:\t0\nSigBlk:\t0\n"),
            None
        );
        assert_eq!(
            parse_signal_status("SigPnd:\tzz\nShdPnd:\t0\nSigBlk:\t0\nSigCgt:\t0\n"),
            None
        );
    }

    #[test]
    fn handler_conflict_is_limited_to_deliverable_guest_handlers() {
        let usr1 = signal_bit(libc::SIGUSR1);
        let urg = signal_bit(libc::SIGURG);
        let trap = signal_bit(libc::SIGTRAP);
        let handled_usr1 = SignalStatus {
            caught: usr1,
            ..Default::default()
        };

        // A handler whose signal is not pending does not decide this restart.
        assert_eq!(
            restart_handler_conflict(Errno::ERESTARTSYS, handled_usr1, None, 0),
            None
        );
        // Pending for the thread, the group, or held by the tracer: refused.
        for status in [
            SignalStatus {
                thread_pending: usr1,
                ..handled_usr1
            },
            SignalStatus {
                shared_pending: usr1,
                ..handled_usr1
            },
        ] {
            assert_eq!(
                restart_handler_conflict(Errno::ERESTARTSYS, status, None, 0),
                Some(usr1)
            );
        }
        assert_eq!(
            restart_handler_conflict(
                Errno::ERESTART_RESTARTBLOCK,
                handled_usr1,
                Some(Signal::SIGUSR1),
                0
            ),
            Some(usr1)
        );
        // A blocked pending signal is not delivered by this resume.
        assert_eq!(
            restart_handler_conflict(
                Errno::ERESTARTNOHAND,
                SignalStatus {
                    thread_pending: usr1,
                    blocked: usr1,
                    ..handled_usr1
                },
                None,
                0
            ),
            None
        );
        // A pending signal with no handler restarts under the no-handler rule.
        assert_eq!(
            restart_handler_conflict(
                Errno::ERESTARTSYS,
                SignalStatus {
                    thread_pending: urg,
                    ..handled_usr1
                },
                None,
                0
            ),
            None
        );
        // ERESTARTNOINTR restarts even with a handler.
        assert_eq!(
            restart_handler_conflict(
                Errno::ERESTARTNOINTR,
                SignalStatus {
                    thread_pending: usr1,
                    ..handled_usr1
                },
                Some(Signal::SIGUSR1),
                0
            ),
            None
        );
        // The runtime's own SIGTRAP router is not a guest handler.
        assert_eq!(
            restart_handler_conflict(
                Errno::ERESTARTSYS,
                SignalStatus {
                    thread_pending: trap,
                    caught: trap,
                    ..Default::default()
                },
                None,
                RUNTIME_OWNED_HANDLERS
            ),
            None
        );
    }
}
