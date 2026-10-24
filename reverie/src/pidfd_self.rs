/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! How the kernel behind a backend spells the pidfd self sentinels.
//!
//! Linux names the calling thread and its thread group without a descriptor
//! through two negative "pidfd" values, accepted by `pidfd_send_signal` and by
//! every caller of `pidfd_get_task` (`pidfd_getfd`, `process_madvise`, ...).
//! The thread sentinel is -10000 in every kernel that has them. The thread
//! group sentinel is not: Linux 6.15 and 6.16 define `PIDFD_SELF_THREAD_GROUP`
//! as -20000 (`include/uapi/linux/pidfd.h`), and Linux 6.17 and later define
//! it as -10001 (`include/uapi/linux/fcntl.h`). A value a kernel does not
//! recognise is looked up as a descriptor and fails with `EBADF`.
//!
//! A tool that forwards such a call must spell it the way the kernel that
//! physically executes it does. [`PidfdSelfSpelling`] is that fact, owned by
//! the backend that executes the physical syscall, and qualified once at its
//! own raw native leaf. Only a successful call is evidence: with signal 0 a
//! kernel answers 0 for a negative pidfd only when it recognises it as a
//! sentinel. Every other answer leaves the fact unknown, and is never read as
//! the absence of support.

use crate::Errno;

/// The thread sentinel, `PIDFD_SELF_THREAD`, in every kernel that has one.
pub const PIDFD_SELF_THREAD: i32 = -10000;
/// `PIDFD_SELF_THREAD_GROUP` in Linux 6.17 and later.
pub const PIDFD_SELF_THREAD_GROUP_MODERN: i32 = -10001;
/// `PIDFD_SELF_THREAD_GROUP` in Linux 6.15 and 6.16.
pub const PIDFD_SELF_THREAD_GROUP_LEGACY: i32 = -20000;

/// How the kernel executing a backend's physical syscalls spells the pidfd
/// self sentinels, as the backend that owns that execution states it.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum PidfdSelfSpelling {
    /// The thread group sentinel is -10001 (Linux 6.17 and later).
    Modern,
    /// The thread group sentinel is -20000 (Linux 6.15 and 6.16).
    Legacy,
    /// The backend executes these calls itself and implements no pidfd self
    /// sentinel: it declares so rather than having it measured.
    Unsupported,
    /// Not qualified: the backend states nothing, or its qualification could
    /// not establish a spelling. Never evidence that the kernel lacks one.
    Unknown,
}

impl PidfdSelfSpelling {
    /// The value this spelling gives `PIDFD_SELF_THREAD_GROUP`, if it has one.
    pub const fn thread_group(self) -> Option<i32> {
        match self {
            Self::Modern => Some(PIDFD_SELF_THREAD_GROUP_MODERN),
            Self::Legacy => Some(PIDFD_SELF_THREAD_GROUP_LEGACY),
            Self::Unsupported | Self::Unknown => None,
        }
    }

    /// The value this spelling gives `PIDFD_SELF_THREAD`, if it has one.
    pub const fn thread(self) -> Option<i32> {
        match self {
            Self::Modern | Self::Legacy => Some(PIDFD_SELF_THREAD),
            Self::Unsupported | Self::Unknown => None,
        }
    }
}

/// How many times a probe that a signal genuinely interrupted is repeated.
const INTERRUPTED_RETRIES: usize = 3;

/// Qualifies the spelling from a backend's own raw native leaf.
///
/// `admitted` is the backend's statement that nothing between this leaf and
/// the kernel can answer for it (no seccomp filter beyond any the backend
/// installed and admits this leaf through). When it is false, nothing is
/// executed and the answer is [`PidfdSelfSpelling::Unknown`].
///
/// `probe(pidfd)` must execute `pidfd_send_signal(pidfd, 0, NULL, 0)` at that
/// leaf and return its raw result. Signal 0 checks permission and existence
/// and sends nothing. The thread group spellings are tried in turn: a 0 for
/// -10001 is [`PidfdSelfSpelling::Modern`]; only an `EBADF` for it permits
/// the -20000 probe, whose 0 is [`PidfdSelfSpelling::Legacy`]. Any other
/// settled outcome, including `EBADF` for both, is
/// [`PidfdSelfSpelling::Unknown`].
///
/// An interruption is not an answer. An `EINTR` is repeated a bounded number
/// of times; if one persists, or the leaf reports that a probe was not run to
/// completion (a restart errno), the result is `None`: nothing was learned,
/// the caller keeps nothing, and a later call qualifies again.
pub fn qualify(
    admitted: bool,
    mut probe: impl FnMut(i32) -> Result<usize, Errno>,
) -> Option<PidfdSelfSpelling> {
    if !admitted {
        return Some(PidfdSelfSpelling::Unknown);
    }
    let mut once = |pidfd: i32| {
        let mut result = probe(pidfd);
        for _ in 0..INTERRUPTED_RETRIES {
            if result != Err(Errno::EINTR) {
                break;
            }
            result = probe(pidfd);
        }
        match result {
            Err(errno) if interrupted(errno) => None,
            settled => Some(settled),
        }
    };
    Some(match once(PIDFD_SELF_THREAD_GROUP_MODERN)? {
        Ok(0) => PidfdSelfSpelling::Modern,
        Err(Errno::EBADF) => match once(PIDFD_SELF_THREAD_GROUP_LEGACY)? {
            Ok(0) => PidfdSelfSpelling::Legacy,
            _ => PidfdSelfSpelling::Unknown,
        },
        _ => PidfdSelfSpelling::Unknown,
    })
}

/// Whether `errno` says a call was interrupted rather than answered: `EINTR`,
/// or a restart errno a backend reports for a call it did not complete.
fn interrupted(errno: Errno) -> bool {
    matches!(
        errno,
        Errno::EINTR
            | Errno::ERESTARTSYS
            | Errno::ERESTARTNOINTR
            | Errno::ERESTARTNOHAND
            | Errno::ERESTART_RESTARTBLOCK
    )
}

/// The raw arguments of the signal-0 probe [`qualify`] asks a leaf to execute.
pub const fn probe_args(pidfd: i32) -> [usize; 6] {
    [pidfd as isize as usize, 0, 0, 0, 0, 0]
}

/// Whether this process carries exactly `expected` seccomp filters, read from
/// `/proc/self/status` (`Seccomp:` and, from Linux 5.9, `Seccomp_filters:`).
/// Anything unreadable or unrecognised is `false`, so a backend that cannot
/// tell what stands between its leaf and the kernel does not qualify.
pub fn process_has_exactly_seccomp_filters(expected: u64) -> bool {
    match std::fs::read_to_string("/proc/self/status") {
        Ok(status) => status_has_exactly_seccomp_filters(&status, expected),
        Err(_) => false,
    }
}

fn status_has_exactly_seccomp_filters(status: &str, expected: u64) -> bool {
    let field = |name: &str| {
        status.lines().find_map(|line| {
            let value = line.strip_prefix(name)?.strip_prefix(':')?;
            value.trim().parse::<u64>().ok()
        })
    };
    match (field("Seccomp"), field("Seccomp_filters")) {
        // Mode 0: no filter at all.
        (Some(0), None) => expected == 0,
        (Some(0), Some(0)) => expected == 0,
        // Mode 2: filter mode; only an exact count is trusted.
        (Some(2), Some(count)) => count == expected && expected > 0,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scripted(answers: &[(i32, Result<usize, Errno>)]) -> (Option<PidfdSelfSpelling>, Vec<i32>) {
        let mut calls = Vec::new();
        let mut answers = answers.iter();
        let spelling = qualify(true, |pidfd| {
            calls.push(pidfd);
            let (expected, answer) = answers.next().expect("an unexpected extra probe");
            assert_eq!(*expected, pidfd);
            *answer
        });
        (spelling, calls)
    }

    #[test]
    fn a_kernel_accepting_the_modern_group_sentinel_is_modern_after_one_probe() {
        let (spelling, calls) = scripted(&[(-10001, Ok(0))]);
        assert_eq!(spelling, Some(PidfdSelfSpelling::Modern));
        assert_eq!(calls, [-10001]);
        assert_eq!(PidfdSelfSpelling::Modern.thread_group(), Some(-10001));
        assert_eq!(PidfdSelfSpelling::Modern.thread(), Some(-10000));
    }

    #[test]
    fn only_ebadf_for_the_modern_value_permits_the_legacy_probe() {
        let (spelling, calls) = scripted(&[(-10001, Err(Errno::EBADF)), (-20000, Ok(0))]);
        assert_eq!(spelling, Some(PidfdSelfSpelling::Legacy));
        assert_eq!(calls, [-10001, -20000]);
        assert_eq!(PidfdSelfSpelling::Legacy.thread_group(), Some(-20000));
        assert_eq!(PidfdSelfSpelling::Legacy.thread(), Some(-10000));
    }

    #[test]
    fn no_failure_is_read_as_absence() {
        for (answers, expected_calls) in [
            (
                vec![(-10001, Err(Errno::EBADF)), (-20000, Err(Errno::EBADF))],
                2,
            ),
            (vec![(-10001, Err(Errno::EPERM))], 1),
            (vec![(-10001, Err(Errno::ENOSYS))], 1),
            (vec![(-10001, Err(Errno::EACCES))], 1),
            (vec![(-10001, Ok(7))], 1),
            (
                vec![(-10001, Err(Errno::EBADF)), (-20000, Err(Errno::EPERM))],
                2,
            ),
        ] {
            let (spelling, calls) = scripted(&answers);
            assert_eq!(spelling, Some(PidfdSelfSpelling::Unknown), "{answers:?}");
            assert_eq!(calls.len(), expected_calls, "{answers:?}");
        }
    }

    #[test]
    fn a_genuine_interruption_is_repeated_a_bounded_number_of_times() {
        let (spelling, calls) = scripted(&[(-10001, Err(Errno::EINTR)), (-10001, Ok(0))]);
        assert_eq!(spelling, Some(PidfdSelfSpelling::Modern));
        assert_eq!(calls, [-10001, -10001]);
        let (spelling, calls) = scripted(&[
            (-10001, Err(Errno::EBADF)),
            (-20000, Err(Errno::EINTR)),
            (-20000, Ok(0)),
        ]);
        assert_eq!(spelling, Some(PidfdSelfSpelling::Legacy));
        assert_eq!(calls, [-10001, -20000, -20000]);
    }

    #[test]
    fn an_interruption_that_persists_or_a_call_not_completed_teaches_nothing() {
        let (spelling, calls) = scripted(&[(-10001, Err(Errno::EINTR)); 4]);
        assert_eq!(spelling, None);
        assert_eq!(calls.len(), 4);
        for restart in [
            Errno::ERESTARTSYS,
            Errno::ERESTARTNOINTR,
            Errno::ERESTARTNOHAND,
            Errno::ERESTART_RESTARTBLOCK,
        ] {
            let (spelling, calls) = scripted(&[(-10001, Err(restart))]);
            assert_eq!(spelling, None, "{restart:?}");
            assert_eq!(calls, [-10001]);
            let (spelling, _) = scripted(&[(-10001, Err(Errno::EBADF)), (-20000, Err(restart))]);
            assert_eq!(spelling, None, "{restart:?}");
        }
    }

    #[test]
    fn an_unadmitted_leaf_executes_nothing() {
        let spelling = qualify(false, |_| panic!("an unadmitted leaf was probed"));
        assert_eq!(spelling, Some(PidfdSelfSpelling::Unknown));
        assert_eq!(PidfdSelfSpelling::Unsupported.thread_group(), None);
    }

    #[test]
    fn the_probe_carries_a_sign_extended_pidfd_and_signal_zero() {
        assert_eq!(probe_args(-10001), [(-10001isize) as usize, 0, 0, 0, 0, 0]);
        assert_eq!(probe_args(-20000)[0] as isize, -20000);
    }

    #[test]
    fn only_an_exact_seccomp_filter_count_admits() {
        let status = |mode: &str, filters: Option<&str>| {
            let mut s = format!("Name:\tx\nSeccomp:\t{mode}\n");
            if let Some(filters) = filters {
                s.push_str(&format!("Seccomp_filters:\t{filters}\n"));
            }
            s
        };
        assert!(status_has_exactly_seccomp_filters(
            &status("0", Some("0")),
            0
        ));
        assert!(status_has_exactly_seccomp_filters(&status("0", None), 0));
        assert!(!status_has_exactly_seccomp_filters(
            &status("2", Some("1")),
            0
        ));
        assert!(status_has_exactly_seccomp_filters(
            &status("2", Some("1")),
            1
        ));
        assert!(!status_has_exactly_seccomp_filters(
            &status("2", Some("2")),
            1
        ));
        // Filter mode without a count (a kernel older than 5.9) is not trusted.
        assert!(!status_has_exactly_seccomp_filters(&status("2", None), 1));
        // Strict mode, garbage and an absent field never admit.
        assert!(!status_has_exactly_seccomp_filters(&status("1", None), 0));
        assert!(!status_has_exactly_seccomp_filters("Seccomp:\tx\n", 0));
        assert!(!status_has_exactly_seccomp_filters("Name:\tx\n", 0));
    }
}
