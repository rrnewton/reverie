/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! The pidfd self-sentinel spelling of the kernel that executes this guest's
//! syscalls ([`reverie::pidfd_self`]), qualified once per process at the
//! guest's trusted physical gate and then kept. An interrupted qualification
//! is kept nowhere and is repeated on the next request.

use std::sync::OnceLock;

use reverie::Errno;
use reverie::PidfdSelfSpelling;
use reverie::pidfd_self;

/// The runtime's own filter: the only one a qualifying process may carry.
const RUNTIME_FILTERS: u64 = 1;

static SPELLING: OnceLock<PidfdSelfSpelling> = OnceLock::new();

/// The spelling, qualified with `probe` (a signal-0 `pidfd_send_signal` at
/// the trusted gate) on first use in this process. The process qualifies
/// only while it carries exactly the runtime's filter.
pub(crate) fn spelling(probe: impl FnMut(i32) -> Result<usize, Errno>) -> PidfdSelfSpelling {
    spelling_in(
        &SPELLING,
        pidfd_self::process_has_exactly_seccomp_filters(RUNTIME_FILTERS),
        probe,
    )
}

/// [`spelling`], kept in `cell`, with the admission decided by the caller.
fn spelling_in(
    cell: &OnceLock<PidfdSelfSpelling>,
    admitted: bool,
    probe: impl FnMut(i32) -> Result<usize, Errno>,
) -> PidfdSelfSpelling {
    if let Some(spelling) = cell.get() {
        return *spelling;
    }
    match pidfd_self::qualify(admitted, probe) {
        Some(spelling) => *cell.get_or_init(|| spelling),
        None => PidfdSelfSpelling::Unknown,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_gate_answer_is_kept_and_an_unadmitted_process_probes_nothing() {
        let cell = OnceLock::new();
        let mut calls = Vec::new();
        let spelling = spelling_in(&cell, true, |pidfd| {
            calls.push(pidfd);
            Ok(0)
        });
        assert_eq!(spelling, PidfdSelfSpelling::Modern);
        assert_eq!(calls, [-10001]);
        assert_eq!(
            spelling_in(&cell, true, |_| panic!("a kept answer was qualified again")),
            PidfdSelfSpelling::Modern
        );
        let cell = OnceLock::new();
        assert_eq!(
            spelling_in(&cell, false, |_| panic!("an unadmitted process was probed")),
            PidfdSelfSpelling::Unknown
        );
    }

    #[test]
    fn an_interrupted_gate_call_is_not_kept() {
        let cell = OnceLock::new();
        assert_eq!(
            spelling_in(&cell, true, |_| Err(Errno::ERESTARTSYS)),
            PidfdSelfSpelling::Unknown
        );
        assert!(cell.get().is_none());
        assert_eq!(
            spelling_in(&cell, true, |pidfd| if pidfd == -10001 {
                Err(Errno::EBADF)
            } else {
                Ok(0)
            }),
            PidfdSelfSpelling::Legacy
        );
    }
}
