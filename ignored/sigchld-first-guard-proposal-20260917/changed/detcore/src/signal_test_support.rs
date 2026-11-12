/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

use std::os::unix::process::CommandExt;
use std::process::Child;
use std::process::Command;
use std::process::Stdio;

/// Own the only host process that a failed guard can signal. The child blocks
/// each tested signal before exec, so a successful send is synchronously
/// observable in its kernel pending mask without installing process-wide test
/// handlers, timing sleeps, or risking an unrelated host process.
pub(crate) struct OwnedSignalTarget(Child);

impl OwnedSignalTarget {
    pub(crate) fn new(signals: &[i32]) -> Self {
        let signals = signals.to_vec();
        let mut command = Command::new("/bin/sleep");
        command.arg("60").stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
        // SAFETY: the post-fork closure performs only signal-set and mask
        // operations; it neither allocates nor acquires a Rust lock.
        unsafe {
            command.pre_exec(move || {
                let mut mask = std::mem::zeroed::<libc::sigset_t>();
                if libc::sigemptyset(&mut mask) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                for signal in &signals {
                    if libc::sigaddset(&mut mask, *signal) != 0 {
                        return Err(std::io::Error::last_os_error());
                    }
                }
                if libc::sigprocmask(libc::SIG_BLOCK, &mask, std::ptr::null_mut()) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        Self(command.spawn().expect("spawn the owned blocked-signal target"))
    }

    pub(crate) fn pid(&self) -> i32 {
        i32::try_from(self.0.id()).unwrap()
    }

    pub(crate) fn assert_pending(&mut self, signal: i32, expected: bool) {
        assert!(self.0.try_wait().unwrap().is_none(), "owned target must remain alive");
        let status = std::fs::read_to_string(format!("/proc/{}/status", self.pid())).unwrap();
        let mask = |name: &str| {
            let value = status.lines().find_map(|line| line.strip_prefix(name)).unwrap();
            u64::from_str_radix(value.trim(), 16).unwrap()
        };
        let bit = 1_u64 << (signal - 1);
        assert_ne!(mask("SigBlk:") & bit, 0, "tested signal must stay blocked");
        assert_eq!((mask("SigPnd:") | mask("ShdPnd:")) & bit != 0, expected);
    }
}

impl Drop for OwnedSignalTarget {
    fn drop(&mut self) {
        // Child::kill and wait apply only to the child retained by this owner.
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
