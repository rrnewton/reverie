/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! The pidfd self-sentinel spelling of the kernel that executes this guest's
//! syscalls ([`reverie::pidfd_self`]).
//!
//! The DBT backend executes a forwarded syscall through the task's native
//! invoker ([`crate::host_linux::forward`]), in the guest process, on the
//! guest thread. It qualifies that same leaf once per process with a
//! signal-0 `pidfd_send_signal` naming the process itself, which sends
//! nothing. It qualifies only while the process carries no seccomp filter,
//! so a success can only be the kernel's own answer; otherwise nothing is
//! executed and the spelling is unknown. An interrupted qualification is
//! kept nowhere and is repeated on the next request.

use std::sync::OnceLock;

use reverie::Errno;
use reverie::InjectionPurpose;
use reverie::Pid;
use reverie::PidfdSelfSpelling;
use reverie::pidfd_self;
use reverie::provider::SyscallRequest;

use crate::SyscallInvoker;

static SPELLING: OnceLock<PidfdSelfSpelling> = OnceLock::new();

/// The spelling for the task whose native invoker is `invoke`, qualified on
/// first use in this process and then kept.
pub(crate) fn spelling(
    tid: Pid,
    pid: Pid,
    context: usize,
    invoke: SyscallInvoker,
) -> PidfdSelfSpelling {
    spelling_in(
        &SPELLING,
        pidfd_self::process_has_exactly_seccomp_filters(0),
        tid,
        pid,
        context,
        invoke,
    )
}

/// [`spelling`], kept in `cell`, with the admission decided by the caller.
fn spelling_in(
    cell: &OnceLock<PidfdSelfSpelling>,
    admitted: bool,
    tid: Pid,
    pid: Pid,
    context: usize,
    invoke: SyscallInvoker,
) -> PidfdSelfSpelling {
    if let Some(spelling) = cell.get() {
        return *spelling;
    }
    let probe = |pidfd: i32| {
        let args = pidfd_self::probe_args(pidfd).map(|arg| arg as u64);
        let raw = crate::host_linux::forward(
            tid,
            pid,
            context,
            invoke,
            InjectionPurpose::Application,
            &SyscallRequest::new(libc::SYS_pidfd_send_signal as u64, args),
        );
        Errno::from_ret(raw as usize)
    };
    match pidfd_self::qualify(admitted, probe) {
        Some(spelling) => *cell.get_or_init(|| spelling),
        None => PidfdSelfSpelling::Unknown,
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use super::*;

    std::thread_local! {
        /// The raw results the scripted invoker returns, in order.
        static ANSWERS: RefCell<Vec<i64>> = const { RefCell::new(Vec::new()) };
    }

    unsafe extern "C" fn scripted(
        _context: usize,
        _number: i64,
        _args: *const u64,
        _purpose: u32,
    ) -> i64 {
        ANSWERS.with(|answers| answers.borrow_mut().remove(0))
    }

    fn run(
        cell: &OnceLock<PidfdSelfSpelling>,
        admitted: bool,
        answers: &[i64],
    ) -> (PidfdSelfSpelling, Vec<[u64; 6]>) {
        ANSWERS.with(|a| *a.borrow_mut() = answers.to_vec());
        let _ = crate::host_linux::take_observed();
        let tid = Pid::from_raw(7);
        let spelling = spelling_in(cell, admitted, tid, Pid::from_raw(5), 0x1234, scripted);
        let observed = crate::host_linux::take_observed();
        for call in &observed {
            assert_eq!(call.request.number(), libc::SYS_pidfd_send_signal as u64);
            assert_eq!(call.purpose, InjectionPurpose::Application);
            assert_eq!(call.tid, tid);
            assert_eq!(call.context, 0x1234);
        }
        assert!(
            ANSWERS.with(|a| a.borrow().is_empty()),
            "unused scripted answers"
        );
        (
            spelling,
            observed.iter().map(|call| *call.request.args()).collect(),
        )
    }

    const EBADF: i64 = -(libc::EBADF as i64);

    #[test]
    fn the_native_invoker_leaf_qualifies_and_the_answer_is_kept() {
        let cell = OnceLock::new();
        let (spelling, calls) = run(&cell, true, &[0]);
        assert_eq!(spelling, PidfdSelfSpelling::Modern);
        assert_eq!(
            calls,
            [pidfd_self::probe_args(-10001).map(|arg| arg as u64)]
        );
        // Kept: a second request issues nothing.
        let (spelling, calls) = run(&cell, true, &[]);
        assert_eq!(spelling, PidfdSelfSpelling::Modern);
        assert!(calls.is_empty());

        let cell = OnceLock::new();
        let (spelling, calls) = run(&cell, true, &[EBADF, 0]);
        assert_eq!(spelling, PidfdSelfSpelling::Legacy);
        assert_eq!(calls[1][0] as i64, -20000);
    }

    #[test]
    fn an_unadmitted_process_executes_nothing_and_an_interruption_is_not_kept() {
        let cell = OnceLock::new();
        let (spelling, calls) = run(&cell, false, &[]);
        assert_eq!(spelling, PidfdSelfSpelling::Unknown);
        assert!(calls.is_empty());

        let cell = OnceLock::new();
        let restart = -(Errno::ERESTARTSYS.into_raw() as i64);
        let (spelling, _) = run(&cell, true, &[restart]);
        assert_eq!(spelling, PidfdSelfSpelling::Unknown);
        assert!(
            cell.get().is_none(),
            "an interrupted qualification was kept"
        );
        let (spelling, _) = run(&cell, true, &[0]);
        assert_eq!(spelling, PidfdSelfSpelling::Modern);
    }
}
