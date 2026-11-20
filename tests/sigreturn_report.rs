/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! The ptrace backend reports a guest's `rt_sigreturn` to a Tool that observes
//! sigreturns (`Tool::handle_sigreturn`), before the call runs, with the mask
//! its frame installs, on every architecture it supports.

use std::sync::Mutex;

use reverie::SigreturnFrame;
use reverie::SigreturnReport;
use reverie::Tool;
use reverie_ptrace::testing::test_fn;

/// The frames reported, in order.
static REPORTS: Mutex<Vec<SigreturnFrame>> = Mutex::new(Vec::new());

#[derive(Debug, Default, Clone)]
struct SigreturnTool;

#[reverie::tool]
impl Tool for SigreturnTool {
    type GlobalState = ();
    type ThreadState = ();

    fn observes_sigreturns(_config: &()) -> bool {
        true
    }

    fn handle_sigreturn(&self, _thread_state: &mut (), report: SigreturnReport) {
        REPORTS.lock().unwrap().push(report.frame());
    }
}

/// A handler that adds SIGUSR2 to the mask its frame restores.
extern "C" fn block_usr2_on_return(
    _signal: libc::c_int,
    _info: *mut libc::siginfo_t,
    context: *mut libc::c_void,
) {
    let context = context.cast::<libc::ucontext_t>();
    // SAFETY: an SA_SIGINFO handler's third argument is its frame's context.
    unsafe { libc::sigaddset(&mut (*context).uc_sigmask, libc::SIGUSR2) };
}

/// The guest's handler adds SIGUSR2 to its frame's `uc_sigmask`. Its return
/// is reported once, with that mask: SIGUSR2 blocked, and SIGUSR1, which
/// Linux blocked only for the handler, not. The call still runs as without
/// the report, and the guest then has SIGUSR2 blocked.
#[test]
fn a_handler_return_is_reported_with_the_mask_its_frame_installs() {
    let usr1 = 1u64 << (libc::SIGUSR1 - 1);
    let usr2 = 1u64 << (libc::SIGUSR2 - 1);
    let (output, ()) = test_fn::<SigreturnTool, _>(|| unsafe {
        let mut action: libc::sigaction = std::mem::zeroed();
        action.sa_sigaction = block_usr2_on_return as *const () as usize;
        action.sa_flags = libc::SA_SIGINFO;
        assert_eq!(
            libc::sigaction(libc::SIGUSR1, &action, std::ptr::null_mut()),
            0
        );
        assert_eq!(libc::raise(libc::SIGUSR1), 0);
        let mut blocked: libc::sigset_t = std::mem::zeroed();
        assert_eq!(
            libc::sigprocmask(libc::SIG_BLOCK, std::ptr::null(), &mut blocked),
            0
        );
        assert_eq!(libc::sigismember(&blocked, libc::SIGUSR2), 1);
        assert_eq!(libc::sigismember(&blocked, libc::SIGUSR1), 0);
    })
    .unwrap();
    assert!(output.status.success(), "{output:?}");
    let reports = std::mem::take(&mut *REPORTS.lock().unwrap());
    assert_eq!(
        reports.len(),
        1,
        "one report per handler return: {reports:?}"
    );
    let SigreturnFrame::Readable { mask } = reports[0] else {
        panic!("the frame is readable: {reports:?}");
    };
    assert_eq!(mask & (usr1 | usr2), usr2, "frame mask {mask:#x}");
}
