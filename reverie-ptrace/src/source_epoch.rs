/* Copyright (c) Meta Platforms, Inc. and affiliates. All rights reserved.
 * Licensed under the BSD-style license in the root LICENSE file. */

//! One initial-EXEC lineage; postinitial exposure irreversibly closes it.
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;

use reverie::syscalls::SyscallArgs;
use reverie::syscalls::Sysno;
use safeptrace::Errno;
use safeptrace::SourceStop;
use safeptrace::Stopped;

#[derive(Default)]
pub(crate) struct SourceEpoch(Mutex<State>, AtomicBool);
#[derive(Default)]
enum State {
    #[default]
    Uninitialised,
    Active(SourceStop),
    Revoked,
}

// These observations must precede the private-IP exemption. Failed attempts
// conservatively revoke too: absence of a successful receipt cannot undo history.
pub(crate) fn observed_syscalls() -> &'static [Sysno] {
    &[
        Sysno::clone,
        Sysno::clone3,
        Sysno::execve,
        Sysno::execveat,
        #[cfg(target_arch = "x86_64")]
        Sysno::fork,
        #[cfg(target_arch = "x86_64")]
        Sysno::vfork,
        Sysno::madvise,
        Sysno::process_madvise,
        Sysno::io_setup,
        Sysno::io_submit,
        Sysno::io_uring_setup,
        Sysno::io_uring_enter,
        Sysno::io_uring_register,
        Sysno::userfaultfd,
        Sysno::seccomp,
        Sysno::ioctl,
        Sysno::vmsplice,
        Sysno::prctl,
    ]
}

pub(crate) fn observes(nr: Sysno) -> bool {
    observed_syscalls().contains(&nr)
}

fn exposes(nr: Sysno, args: SyscallArgs) -> bool {
    match nr {
        // FIONBIO is handled by do_vfs_ioctl itself (get_user + f_flags),
        // independent of the underlying device. Do NOT whitelist TCGETS or
        // FIONREAD by number: those can dispatch to arbitrary device drivers.
        Sysno::ioctl => args.arg1 as u32 != 0x5421,
        // A stacked USER_NOTIF filter outranks TRACE and can CONTINUE an
        // otherwise unobserved birth/write. Observe installation before effect.
        // PR_SET_PTRACER can expose the mm to an external control owner.
        Sysno::prctl => matches!(args.arg0 as u32, 22 | 0x59616d61),
        _ => observes(nr),
    }
}

impl SourceEpoch {
    pub(crate) fn initial_exec(&self, task: &Stopped, supported_command: bool) {
        let mut state = self.0.lock().unwrap();
        if !matches!(*state, State::Uninitialised) {
            *state = State::Revoked;
            return;
        }
        // This call site owns the actual initial EXEC. source_stop additionally
        // requires the consumed notifier receipt and its original directory.
        *state = match supported_command.then(|| task.source_stop()) {
            Some(Ok(stop)) => State::Active(stop),
            _ => State::Revoked,
        };
    }

    pub(crate) fn observe(&self, nr: Sysno, args: SyscallArgs) {
        self.observe_classified(nr, args, false);
    }

    pub(super) fn observe_classified(&self, nr: Sysno, args: SyscallArgs, original_ioctl: bool) {
        if exposes(nr, args) && !(nr == Sysno::ioctl && original_ioctl) {
            self.revoke();
        }
    }

    pub(crate) fn observe_resume(&self, stopped: &Stopped) {
        self.observe_resume_classified(stopped, false);
    }

    pub(super) fn observe_resume_classified(&self, stopped: &Stopped, original_ioctl: bool) {
        if !matches!(*self.0.lock().unwrap(), State::Active(_)) {
            return;
        }
        // Ptrace can rewrite orig_rax/arguments after the seccomp callback. Linux
        // rechecks the filter with recheck_after_trace=true and does not issue a
        // second TRACE stop. Inspect the actual pending operands before effect.
        match stopped.pending_syscall_entry() {
            Ok(Some(entry)) => {
                self.observe_raw_classified(entry.number, entry.arguments, original_ioctl)
            }
            Ok(None) => {}
            Err(_) => self.revoke(),
        }
    }

    pub(crate) fn observe_raw(&self, number: u64, arguments: [u64; 6]) {
        self.observe_raw_classified(number, arguments, false);
    }

    pub(super) fn observe_raw_classified(
        &self,
        number: u64,
        arguments: [u64; 6],
        original_ioctl: bool,
    ) {
        // Linux skips a negative syscall number before executing any effect.
        // Reverie's ordinary emulation uses -1; it must not poison subsequent
        // source operations merely for having skipped an earlier syscall.
        if (number as i32) < 0 {
            return;
        }
        if number as u32 >= 0x4000_0000 {
            self.revoke(); // x32 (or unknown high-number ABI) history is unsupported
        } else if let Some(&nr) = observed_syscalls().iter().find(|&&nr| nr as u64 == number) {
            self.observe_classified(
                nr,
                SyscallArgs::new(
                    arguments[0] as usize,
                    arguments[1] as usize,
                    arguments[2] as usize,
                    arguments[3] as usize,
                    arguments[4] as usize,
                    arguments[5] as usize,
                ),
                original_ioctl,
            );
        }
    }

    pub(crate) fn revoke(&self) {
        let mut state = self.0.lock().unwrap();
        // The genuine initial EXEC creates a new mm. No pre-callback
        // zero-birth launch proof is required. Once active, never reset.
        if matches!(*state, State::Active(_)) {
            *state = State::Revoked;
        }
    }

    pub(super) fn begin_ioctl(self: &Arc<Self>) -> Option<NativeIoctl> {
        if !matches!(*self.0.lock().unwrap(), State::Active(_))
            || self
                .1
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                .is_err()
        {
            self.revoke();
            return None;
        }
        Some(NativeIoctl {
            epoch: Arc::clone(self),
            completed: false,
        })
    }

    pub(crate) fn validate(&self, stop: &SourceStop) -> Result<(), Errno> {
        stop.validate_current()?;
        match &*self.0.lock().unwrap() {
            State::Active(root) if stop.same_task(root) => {
                if self.1.load(Ordering::Acquire) {
                    Err(Errno::EBUSY)
                } else {
                    Ok(())
                }
            }
            _ => Err(Errno::ENOTSUPP),
        }
    }
}

/// Carried by the existing NativeOperation, through actual return/restoration.
/// Cancellation, missing completion and unexpected returns permanently revoke.
pub(super) struct NativeIoctl {
    epoch: Arc<SourceEpoch>,
    completed: bool,
}
impl NativeIoctl {
    pub(super) fn restored(mut self, result: i64) -> bool {
        #[cfg(test)]
        if OMIT_IOCTL_COMPLETION.with(|value| value.replace(false)) {
            return false;
        }
        if result != -(libc::ENOTTY as i64) {
            return false;
        }
        self.completed = true;
        self.epoch.1.store(false, Ordering::Release);
        true
    }
}
impl Drop for NativeIoctl {
    fn drop(&mut self) {
        if !self.completed {
            self.epoch.revoke();
        }
    }
}

#[cfg(test)]
thread_local! {
    static OMIT_IOCTL_COMPLETION: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}
/// A one-operation omission in the same native body, after actual return.
#[cfg(test)]
pub(crate) fn omit_next_ioctl_completion_for_test() {
    OMIT_IOCTL_COMPLETION.with(|value| {
        assert!(!value.replace(true));
    });
}

#[cfg(all(test, target_arch = "x86_64"))]
mod policy_tests {
    use super::*;

    #[test]
    fn exact_eighteen_families_and_argument_sensitive_exposures() {
        // Independent specification, not an expectation generated from the
        // producer's list. This is policy/schema coverage, not 18 native runs.
        let expected = [
            Sysno::clone,
            Sysno::clone3,
            Sysno::execve,
            Sysno::execveat,
            Sysno::fork,
            Sysno::vfork,
            Sysno::madvise,
            Sysno::process_madvise,
            Sysno::io_setup,
            Sysno::io_submit,
            Sysno::io_uring_setup,
            Sysno::io_uring_enter,
            Sysno::io_uring_register,
            Sysno::userfaultfd,
            Sysno::seccomp,
            Sysno::ioctl,
            Sysno::vmsplice,
            Sysno::prctl,
        ];
        assert_eq!(expected.len(), 18);
        let unique: std::collections::BTreeSet<_> = expected.iter().map(|nr| *nr as u32).collect();
        assert_eq!(unique.len(), 18);
        assert_eq!(
            observed_syscalls(),
            expected.as_slice(),
            "complete ordered family schema"
        );
        for nr in expected {
            assert!(observes(nr), "observed family {nr:?}");
            if !matches!(nr, Sysno::ioctl | Sysno::prctl) {
                for args in [
                    SyscallArgs::new(0, 0, 0, 0, 0, 0),
                    SyscallArgs::new(
                        usize::MAX,
                        usize::MAX,
                        usize::MAX,
                        usize::MAX,
                        usize::MAX,
                        usize::MAX,
                    ),
                ] {
                    assert!(exposes(nr, args), "unconditional exposure {nr:?} {args:?}");
                }
            }
        }
        // Linux consumes these ioctl/prctl selectors as u32. Upper words must
        // neither add an exception nor revoke the existing FIONBIO exception.
        for high in [0, 0xffff_ffff_0000_0000usize] {
            for (request, expected) in [
                (0x541b, true),  // FIONREAD: includes failed exposure attempts
                (0x5401, true),  // TCGETS: not a core-only exception
                (0x5421, false), // FIONBIO: existing core f_flags exception
                (0, true),
                (u32::MAX as usize, true),
            ] {
                for fd in [0, usize::MAX] {
                    assert_eq!(
                        exposes(
                            Sysno::ioctl,
                            SyscallArgs::new(fd, high | request, 1, 2, 3, 4)
                        ),
                        expected,
                        "ioctl request={request:#x} high={high:#x} fd={fd}"
                    );
                }
            }
            for (option, expected) in [
                (22, true),
                (0x5961_6d61, true), // SET_SECCOMP / SET_PTRACER
                (3, false),
                (0, false),
                (u32::MAX as usize, false),
            ] {
                assert_eq!(
                    exposes(Sysno::prctl, SyscallArgs::new(high | option, 1, 2, 3, 4, 5)),
                    expected,
                    "prctl option={option:#x} high={high:#x}"
                );
            }
        }
        for nr in [Sysno::getpid, Sysno::write, Sysno::pipe2, Sysno::fcntl] {
            assert!(!observes(nr), "non-family {nr:?}");
            assert!(!exposes(nr, SyscallArgs::new(0, 0, 0, 0, 0, 0)));
        }
    }
}
