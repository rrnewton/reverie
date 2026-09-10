/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! Linux reports ignored signals to a ptracer at a signal-delivery stop before
//! applying the tracee's disposition. Keep that reference behavior covered so
//! virtual backends can expose the same one-hook-per-event contract.

use std::sync::Mutex;

use reverie::Errno;
use reverie::ExitStatus;
use reverie::GlobalTool;
use reverie::Guest;
use reverie::Pid;
use reverie::Signal;
use reverie::Subscription;
use reverie::Tool;
use reverie_ptrace::testing::test_fn;

#[derive(Default)]
struct SignalLog {
    signals: Mutex<Vec<i32>>,
}

#[reverie::global_tool]
impl GlobalTool for SignalLog {
    type Request = i32;
    type Response = ();
    type Config = ();

    async fn receive_rpc(&self, _from: Pid, signal: i32) {
        self.signals
            .lock()
            .expect("ignored-signal log lock poisoned")
            .push(signal);
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct SignalLogTool;

#[reverie::tool]
impl Tool for SignalLogTool {
    type GlobalState = SignalLog;
    type ThreadState = ();

    fn subscriptions(_config: &()) -> Subscription {
        Subscription::none()
    }

    async fn handle_signal_event<G: Guest<Self>>(
        &self,
        guest: &mut G,
        signal: Signal,
    ) -> Result<Option<Signal>, Errno> {
        guest.send_rpc(signal as i32).await;
        Ok(Some(signal))
    }
}

#[test]
fn ptrace_tool_observes_explicit_and_default_ignored_signals_once() {
    let (output, log) = test_fn::<SignalLogTool, _>(|| unsafe {
        let mut action: libc::sigaction = std::mem::zeroed();
        action.sa_sigaction = libc::SIG_IGN;
        libc::sigemptyset(&mut action.sa_mask);
        assert_eq!(
            libc::sigaction(libc::SIGUSR1, &action, std::ptr::null_mut()),
            0
        );
        assert_eq!(libc::kill(libc::getpid(), libc::SIGUSR1), 0);
        assert_eq!(libc::kill(libc::getpid(), libc::SIGWINCH), 0);
    })
    .expect("run ignored-signal ptrace guest");

    assert_eq!(output.status, ExitStatus::Exited(0));
    assert_eq!(
        *log.signals
            .lock()
            .expect("ignored-signal log lock poisoned"),
        vec![libc::SIGUSR1, libc::SIGWINCH],
    );
}
