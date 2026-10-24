/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! The pidfd self-sentinel spelling of the kernel that executes a traced
//! guest's syscalls ([`reverie::pidfd_self`]).
//!
//! A traced guest's syscalls run in the tracee, on the same kernel as the
//! tracer. The tracer qualifies that kernel once per process, with a raw
//! signal-0 `pidfd_send_signal` against itself: no tracee is stopped,
//! injected into or signalled. It qualifies only when the tracer carries no
//! seccomp filter, so a success can only be the kernel's own answer; with a
//! filter, or when that cannot be read, nothing is executed and the spelling
//! is unknown. The answer is kept for the life of the process, which is the
//! life of every run it traces: a kernel's sentinel spelling cannot change.

use std::sync::OnceLock;

use reverie::Errno;
use reverie::PidfdSelfSpelling;
use reverie::pidfd_self;

static SPELLING: OnceLock<PidfdSelfSpelling> = OnceLock::new();

/// The kernel's spelling, qualified on first use and then kept. An
/// interrupted qualification is kept nowhere: this call answers unknown and
/// the next one qualifies again.
pub(crate) fn spelling() -> PidfdSelfSpelling {
    if let Some(spelling) = SPELLING.get() {
        return *spelling;
    }
    match pidfd_self::qualify(pidfd_self::process_has_exactly_seccomp_filters(0), probe) {
        Some(spelling) => *SPELLING.get_or_init(|| spelling),
        None => PidfdSelfSpelling::Unknown,
    }
}

/// `pidfd_send_signal(pidfd, 0, NULL, 0)` on the tracer's own thread.
fn probe(pidfd: i32) -> Result<usize, Errno> {
    let [a0, a1, a2, a3, _, _] = pidfd_self::probe_args(pidfd);
    // SAFETY: signal 0 with a null siginfo reads and writes no memory.
    let result = unsafe { libc::syscall(libc::SYS_pidfd_send_signal, a0, a1, a2, a3) };
    if result < 0 {
        Err(Errno::last())
    } else {
        Ok(result as usize)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The probe is the kernel's own answer on this host: whatever the
    /// spelling, it is stable across calls and never a measured absence.
    #[test]
    fn the_tracer_qualifies_once_and_keeps_the_answer() {
        let first = spelling();
        assert_eq!(spelling(), first);
        assert_ne!(first, PidfdSelfSpelling::Unsupported);
        if first != PidfdSelfSpelling::Unknown {
            // A qualified spelling's group sentinel answers signal 0 here.
            let group = first.thread_group().unwrap();
            assert_eq!(probe(group), Ok(0));
            assert_eq!(probe(first.thread().unwrap()), Ok(0));
        }
    }

    /// An ordinary negative descriptor is never a sentinel.
    #[test]
    fn a_non_sentinel_negative_descriptor_is_ebadf() {
        assert_eq!(probe(-10002), Err(Errno::EBADF));
    }
}
