/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! Keeping a guest's SIGSEGV action and mask as the guest set them across
//! Reverie's rdtsc and cpuid traps (<https://github.com/rrnewton/hermit/issues/4485>).
//!
//! A Tool that subscribes to rdtsc or cpuid makes either instruction fault
//! (`PR_SET_TSC`, CPUID faulting), and Reverie emulates the instruction and
//! suppresses the SIGSEGV. Natively neither faults. The fault is a general
//! protection fault, which Linux reports with `force_sig(SIGSEGV)`
//! (`gp_user_force_sig_segv`). If SIGSEGV is blocked or ignored then,
//! `force_sig_info_to_task` sets its handler to `SIG_DFL`, keeping its flags,
//! mask and restorer, and unblocks it for the thread, all before the ptrace
//! stop. Unlike a single-step's SIGTRAP, nothing can be shielded: the fault
//! comes from the guest's own instruction, at whatever state the guest is in.
//!
//! So the task repairs at the trap what it knows was reset
//! (`TracedTask::restore_segv_after_trap`), before the Tool sees the event:
//! - SIGSEGV's bit in the mask the guest code ran under, recorded as each
//!   `PTRACE_CONT` resumed it ([`SegvRun`]);
//! - SIGSEGV's whole action, recorded per signal-action table from every
//!   change Reverie sees.
//!
//! What is not known stays as the reset left it, as before.
//!
//! This module holds the parts that need no tracee.

use crate::sigtrap_state::KernelSigaction;

/// SIGSEGV's bit in a kernel signal mask.
pub(crate) const SIGSEGV_BIT: u64 = 1 << (libc::SIGSEGV as u64 - 1);

/// `CLONE_CLEAR_SIGHAND`, a `clone3` flag the libc crate does not name.
pub(crate) const CLONE_CLEAR_SIGHAND: u64 = 0x1_0000_0000;

/// `SA_RESETHAND` and `SA_NODEFER` as `sa_flags` bits (the libc crate's
/// are a signed `int`, and `SA_RESETHAND` is its sign bit).
const SA_RESETHAND: u64 = libc::SA_RESETHAND as u32 as u64;
const SA_NODEFER: u64 = libc::SA_NODEFER as u32 as u64;

/// The `sa_flags` bits an `rt_sigaction` keeps on x86-64 (`UAPI_SA_FLAGS`,
/// `include/linux/signal_types.h`): `SA_NOCLDSTOP`, `SA_NOCLDWAIT`,
/// `SA_SIGINFO`, `SA_EXPOSE_TAGBITS`, `SA_RESTORER`, `SA_ONSTACK`,
/// `SA_RESTART`, `SA_NODEFER` and `SA_RESETHAND`. `do_sigaction` clears the
/// others.
const UAPI_SA_FLAGS: u64 =
    0x1 | 0x2 | 0x4 | 0x800 | 0x0400_0000 | 0x0800_0000 | 0x1000_0000 | 0x4000_0000 | 0x8000_0000;

impl KernelSigaction {
    /// Whether the action runs a handler: it is neither `SIG_DFL` nor
    /// `SIG_IGN`.
    pub(crate) fn caught(&self) -> bool {
        self.handler != libc::SIG_DFL as u64 && !self.ignored()
    }

    /// The action an `rt_sigaction` that succeeds installs from `act`:
    /// `do_sigaction` clears the flags it does not know and drops SIGKILL and
    /// SIGSTOP from the mask.
    pub(crate) fn installed(act: KernelSigaction) -> KernelSigaction {
        KernelSigaction {
            flags: act.flags & UAPI_SA_FLAGS,
            mask: act.mask & crate::sigtrap_state::BLOCKABLE_SIGNAL_BITS,
            ..act
        }
    }

    /// The action `flush_signal_handlers(t, 0)` leaves, as exec and a
    /// `CLONE_CLEAR_SIGHAND` clone do: a handler becomes `SIG_DFL`, `SIG_IGN`
    /// stays, and the flags, restorer and mask are cleared.
    pub(crate) fn flushed(self) -> KernelSigaction {
        KernelSigaction {
            handler: if self.ignored() {
                libc::SIG_IGN as u64
            } else {
                libc::SIG_DFL as u64
            },
            ..KernelSigaction::default()
        }
    }

    /// The action as a delivery leaves it: `SA_RESETHAND` resets a handler
    /// to `SIG_DFL`, keeping the rest (`get_signal`).
    pub(crate) fn after_delivery(self) -> KernelSigaction {
        if self.caught() && self.flags & SA_RESETHAND != 0 {
            KernelSigaction {
                handler: libc::SIG_DFL as u64,
                ..self
            }
        } else {
            self
        }
    }
}

/// SIGSEGV's bit in the mask a thread's guest code runs under since its last
/// `PTRACE_CONT`.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum SegvRun {
    /// Not known.
    #[default]
    Unknown,
    /// Blocked (`true`) or not.
    Known(bool),
    /// The resume ran a syscall that sets the mask (`rt_sigprocmask`,
    /// `rt_sigreturn`), predicted to leave this one from the arguments it
    /// reads from guest memory. The kernel reads them under the guest's
    /// protections, which a tracer's read does not meet (a protection key),
    /// so the call may have failed and left the mask: the prediction holds
    /// only if the mask at the trap is the predicted one without SIGSEGV,
    /// which the trap clears where it is blocked.
    Predicted(u64),
    /// The resume delivered `signal`, not SIGSEGV, and SIGSEGV's bit
    /// depends on whether it ran a handler. If it did, the handler runs under
    /// the mask the kernel blocked under (`blocked`, procfs `SigBlk`) plus
    /// the handler's mask (`signal_delivered`). If it did not (`SIG_IGN`, or
    /// a default action that ignores it), the return to user mode restores a
    /// pending temporary mask's saved mask (`saved`, `PTRACE_GETSIGMASK`).
    /// A delivered signal's action can change only at a stop, and every stop
    /// is followed by a `PTRACE_CONT` that records anew, so the action read
    /// at the trap is the delivery's ([`SegvRun::resolve`]).
    Delivered {
        signal: i32,
        blocked: bool,
        saved: bool,
    },
}

impl SegvRun {
    /// SIGSEGV's bit, given the mask at the trap (`now`) for
    /// [`SegvRun::Predicted`], and the delivered signal's action as read at
    /// the trap (`None` if it could not be read) for [`SegvRun::Delivered`].
    /// The trap leaves SIGSEGV unblocked either way, so a mask that blocks
    /// it at the trap contradicts every record, and nothing is known.
    pub(crate) fn resolve(self, now: u64, action: Option<KernelSigaction>) -> Option<bool> {
        if now & SIGSEGV_BIT != 0 {
            return None;
        }
        match self {
            SegvRun::Unknown => None,
            SegvRun::Known(blocked) => Some(blocked),
            SegvRun::Predicted(mask) => {
                (now == mask & !SIGSEGV_BIT).then_some(mask & SIGSEGV_BIT != 0)
            }
            SegvRun::Delivered { blocked, saved, .. } => {
                let handled = |action: KernelSigaction| blocked || action.mask & SIGSEGV_BIT != 0;
                match action {
                    Some(action) if action.caught() => Some(handled(action)),
                    Some(action) if action.ignored() => Some(saved),
                    // `SIG_DFL` with `SA_RESETHAND` is either a handler the
                    // delivery reset, which keeps its mask, or a default
                    // action that ran no handler.
                    Some(action) if action.flags & SA_RESETHAND != 0 => {
                        (handled(action) == saved).then_some(saved)
                    }
                    Some(_) => Some(saved),
                    // A handler's mask holds `blocked`, so a blocked bit is
                    // blocked either way only when `saved` is too.
                    None => (blocked && saved).then_some(true),
                }
            }
        }
    }

    /// SIGSEGV's bit as a resume that delivers SIGSEGV itself leaves it,
    /// with `before` SIGSEGV's action before the delivery: a handler runs
    /// under `blocked` plus its mask and, unless `SA_NODEFER`, SIGSEGV; an
    /// ignored SIGSEGV is discarded, and the return restores `saved`. A
    /// default SIGSEGV ends the process.
    pub(crate) fn after_segv_delivery(
        before: Option<KernelSigaction>,
        blocked: bool,
        saved: bool,
    ) -> SegvRun {
        match before {
            Some(action) if action.caught() => SegvRun::Known(
                blocked || action.mask & SIGSEGV_BIT != 0 || action.flags & SA_NODEFER == 0,
            ),
            Some(_) => SegvRun::Known(saved),
            None => SegvRun::Unknown,
        }
    }
}

/// What a trap's repair does, decided from what was known before the trap
/// and what SIGSEGV's action reads at it.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct SegvRepair {
    /// Whether SIGSEGV is blocked again.
    pub(crate) block: bool,
    /// The action written back.
    pub(crate) write: Option<KernelSigaction>,
    /// SIGSEGV's action after the repair, if known.
    pub(crate) after: Option<KernelSigaction>,
    /// Whether everything the trap may have reset is known.
    pub(crate) known: bool,
}

/// Decides a trap's repair. `blocked` is SIGSEGV's bit before the trap,
/// `before` its action, each `None` if not known; `now` is the action read
/// at the trap, `None` if it was not or could not be read.
///
/// The trap reset the action (to `SIG_DFL`, keeping the flags, mask and
/// restorer) if SIGSEGV was blocked or ignored, and unblocked it if it was
/// blocked. Where the bit is not known, the reset itself tells: a caught
/// action that now reads `SIG_DFL` was reset, so SIGSEGV was blocked; one
/// that reads as before was not. The action is written back only if `now`
/// is what the reset left of `before`.
pub(crate) fn segv_repair(
    blocked: Option<bool>,
    before: Option<KernelSigaction>,
    now: Option<KernelSigaction>,
) -> SegvRepair {
    let reset_of = |before: KernelSigaction| KernelSigaction {
        handler: libc::SIG_DFL as u64,
        ..before
    };
    let blocked = match (blocked, before, now) {
        (Some(blocked), ..) => Some(blocked),
        (None, Some(before), Some(now)) if before.caught() => {
            if now == reset_of(before) {
                Some(true)
            } else if now == before {
                Some(false)
            } else {
                None
            }
        }
        _ => None,
    };
    let reset = match (blocked, before) {
        (_, Some(before)) if before.ignored() => Some(true),
        (Some(blocked), Some(_)) => Some(blocked),
        _ => None,
    };
    let write = match (reset, before, now) {
        (Some(true), Some(before), Some(now))
            if before.handler != now.handler && now == reset_of(before) =>
        {
            Some(before)
        }
        _ => None,
    };
    let after = match before {
        // The reset leaves a `SIG_DFL` action as it was.
        Some(before) if before.handler == libc::SIG_DFL as u64 => Some(before),
        Some(before) => (reset == Some(false) || write.is_some()).then_some(before),
        // An action that was not known and that reads as no reset could
        // leave (not `SIG_DFL`) was not reset.
        None => now.filter(|now| now.handler != libc::SIG_DFL as u64),
    };
    SegvRepair {
        block: blocked == Some(true),
        write,
        after,
        known: blocked.is_some() && after.is_some(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HANDLER: u64 = 0x401000;

    fn action(handler: u64, flags: u64, mask: u64) -> KernelSigaction {
        KernelSigaction {
            handler,
            flags,
            restorer: 0x402000,
            mask,
        }
    }

    fn dfl() -> u64 {
        libc::SIG_DFL as u64
    }

    fn ign() -> u64 {
        libc::SIG_IGN as u64
    }

    #[test]
    fn an_installed_action_is_what_do_sigaction_keeps() {
        let kill_stop = 1 << (libc::SIGKILL - 1) | 1 << (libc::SIGSTOP - 1);
        let act = action(HANDLER, SA_RESETHAND | 0x0200_0000, SIGSEGV_BIT | kill_stop);
        assert_eq!(
            KernelSigaction::installed(act),
            action(HANDLER, SA_RESETHAND, SIGSEGV_BIT)
        );
    }

    #[test]
    fn a_flush_keeps_only_sig_ign() {
        assert_eq!(
            action(HANDLER, SA_NODEFER, SIGSEGV_BIT).flushed(),
            KernelSigaction::default()
        );
        assert_eq!(action(ign(), SA_NODEFER, 1).flushed().handler, ign());
        assert_eq!(action(ign(), SA_NODEFER, 1).flushed().flags, 0);
    }

    #[test]
    fn a_delivery_resets_only_a_resethand_handler() {
        let oneshot = action(HANDLER, SA_RESETHAND, 0);
        assert_eq!(oneshot.after_delivery(), action(dfl(), SA_RESETHAND, 0));
        let kept = action(HANDLER, 0, 0);
        assert_eq!(kept.after_delivery(), kept);
        let ignored = action(ign(), SA_RESETHAND, 0);
        assert_eq!(ignored.after_delivery(), ignored);
    }

    #[test]
    fn a_delivered_signal_resolves_by_its_action() {
        let run = SegvRun::Delivered {
            signal: libc::SIGUSR1,
            blocked: false,
            saved: false,
        };
        assert_eq!(
            run.resolve(0, Some(action(HANDLER, 0, SIGSEGV_BIT))),
            Some(true)
        );
        assert_eq!(run.resolve(0, Some(action(HANDLER, 0, 0))), Some(false));
        assert_eq!(
            run.resolve(0, Some(action(ign(), 0, SIGSEGV_BIT))),
            Some(false)
        );
        assert_eq!(
            run.resolve(0, Some(action(dfl(), 0, SIGSEGV_BIT))),
            Some(false)
        );
        // A reset one-shot handler or a default action: undecided when the
        // two differ.
        assert_eq!(
            run.resolve(0, Some(action(dfl(), SA_RESETHAND, SIGSEGV_BIT))),
            None
        );
        assert_eq!(
            run.resolve(0, Some(action(dfl(), SA_RESETHAND, 0))),
            Some(false)
        );
        assert_eq!(run.resolve(0, None), None);
        let blocked = SegvRun::Delivered {
            signal: libc::SIGUSR1,
            blocked: true,
            saved: true,
        };
        assert_eq!(blocked.resolve(0, None), Some(true));
        let swapped = SegvRun::Delivered {
            signal: libc::SIGUSR1,
            blocked: true,
            saved: false,
        };
        assert_eq!(swapped.resolve(0, Some(action(HANDLER, 0, 0))), Some(true));
        assert_eq!(swapped.resolve(0, Some(action(ign(), 0, 0))), Some(false));
        assert_eq!(swapped.resolve(0, None), None);
    }

    #[test]
    fn a_prediction_holds_only_if_the_rest_of_the_mask_agrees() {
        let usr1 = 1 << (libc::SIGUSR1 - 1);
        let run = SegvRun::Predicted(usr1 | SIGSEGV_BIT);
        // The trap cleared SIGSEGV; the rest is as predicted.
        assert_eq!(run.resolve(usr1, None), Some(true));
        // A blocked SIGSEGV that the trap did not clear: not the prediction.
        assert_eq!(run.resolve(usr1 | SIGSEGV_BIT, None), None);
        // The call failed and left the old mask.
        assert_eq!(run.resolve(0, None), None);
        assert_eq!(SegvRun::Predicted(usr1).resolve(usr1, None), Some(false));
    }

    #[test]
    fn a_delivered_sigsegv_blocks_itself_unless_nodefer() {
        assert_eq!(
            SegvRun::after_segv_delivery(Some(action(HANDLER, 0, 0)), false, false),
            SegvRun::Known(true)
        );
        assert_eq!(
            SegvRun::after_segv_delivery(Some(action(HANDLER, SA_NODEFER, 0)), false, false),
            SegvRun::Known(false)
        );
        assert_eq!(
            SegvRun::after_segv_delivery(
                Some(action(HANDLER, SA_NODEFER, SIGSEGV_BIT)),
                false,
                false
            ),
            SegvRun::Known(true)
        );
        assert_eq!(
            SegvRun::after_segv_delivery(Some(action(ign(), 0, 0)), true, false),
            SegvRun::Known(false)
        );
        assert_eq!(
            SegvRun::after_segv_delivery(None, false, false),
            SegvRun::Unknown
        );
    }

    #[test]
    fn a_trap_repairs_what_was_known_to_be_reset() {
        let caught = action(HANDLER, SA_NODEFER, SIGSEGV_BIT);
        let reset = action(dfl(), SA_NODEFER, SIGSEGV_BIT);
        let ignored = action(ign(), 0, 0);
        let ignored_reset = action(dfl(), 0, 0);
        let default = action(dfl(), 0, 0);

        // Unblocked and caught: nothing was reset.
        assert_eq!(
            segv_repair(Some(false), Some(caught), None),
            SegvRepair {
                block: false,
                write: None,
                after: Some(caught),
                known: true
            }
        );
        // Blocked and caught: both go back.
        assert_eq!(
            segv_repair(Some(true), Some(caught), Some(reset)),
            SegvRepair {
                block: true,
                write: Some(caught),
                after: Some(caught),
                known: true
            }
        );
        // Ignored: the action goes back whatever the mask, and the mask bit
        // only where it is known.
        assert_eq!(
            segv_repair(None, Some(ignored), Some(ignored_reset)),
            SegvRepair {
                block: false,
                write: Some(ignored),
                after: Some(ignored),
                known: false
            }
        );
        // Blocked and default: only the bit.
        assert_eq!(
            segv_repair(Some(true), Some(default), Some(default)),
            SegvRepair {
                block: true,
                write: None,
                after: Some(default),
                known: true
            }
        );
        // An unknown bit with a caught action: the reset tells.
        assert!(segv_repair(None, Some(caught), Some(reset)).block);
        assert_eq!(
            segv_repair(None, Some(caught), Some(reset)).write,
            Some(caught)
        );
        assert_eq!(
            segv_repair(None, Some(caught), Some(caught)),
            SegvRepair {
                block: false,
                write: None,
                after: Some(caught),
                known: true
            }
        );
        // An action that is not what the reset left of the record is not
        // written, and the record is given up.
        let other = action(HANDLER + 16, 0, 0);
        assert_eq!(
            segv_repair(Some(true), Some(caught), Some(other)),
            SegvRepair {
                block: true,
                write: None,
                after: None,
                known: false
            }
        );
        // An unknown action: nothing is written; a caught one read now was
        // not reset, and is learned.
        assert_eq!(
            segv_repair(Some(false), None, Some(caught)),
            SegvRepair {
                block: false,
                write: None,
                after: Some(caught),
                known: true
            }
        );
        assert_eq!(
            segv_repair(Some(true), None, Some(reset)),
            SegvRepair {
                block: true,
                write: None,
                after: None,
                known: false
            }
        );
        // A default action survives the reset, whatever the mask was.
        assert_eq!(
            segv_repair(None, Some(default), None),
            SegvRepair {
                block: false,
                write: None,
                after: Some(default),
                known: false
            }
        );
        // Nothing known: nothing done.
        assert_eq!(segv_repair(None, None, None), SegvRepair::default());
    }
}
