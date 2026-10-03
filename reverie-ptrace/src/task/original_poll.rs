/* Copyright (c) Meta Platforms, Inc. and affiliates. All rights reserved.
 * Licensed under the BSD-style license in the root LICENSE file. */
//! Original native Poll input and one-use output identity. No readiness issuer.
use reverie::syscalls::OriginalPollInput;

use super::*;

pub(super) struct OriginalPollEntry {
    terminal: TerminalCleanup,
    pub(super) call: (Sysno, SyscallArgs),
    pub(super) limit: (u64, u64),
    input: Option<OriginalPollInput>,
    failed: AtomicBool,
    used: AtomicBool,
}
impl OriginalPollEntry {
    pub(super) fn new(
        task: &Stopped,
        call: (Sysno, SyscallArgs),
        regs: &libc::user_regs_struct,
        limit: (u64, u64),
    ) -> Result<Self, Errno> {
        // Native __x64_sys_poll casts nfds to unsigned int and timeout to int;
        // retain and authenticate ALL original raw operands independently.
        if call.0 != Sysno::poll
            || call.1.arg1 as u32 != 1
            || (call.1.arg2 as i32) < 0
            || limit.0 < 1
        {
            return Err(Errno::EOPNOTSUPP);
        }
        original_context::check_entry(task, call.0, call.1, regs.rip, regs.rsp, true)
            .map_err(|_| Errno::ESTALE)?;
        Ok(Self {
            terminal: task.terminal_cleanup(),
            call,
            limit,
            input: None,
            failed: AtomicBool::new(false),
            used: AtomicBool::new(false),
        })
    }
    pub(super) fn decode(bytes: &[u8], timeout_millis: i32) -> Result<OriginalPollInput, Errno> {
        if bytes.len() != 8 {
            return Err(Errno::EPROTO);
        }
        let fd = i32::from_ne_bytes(bytes[..4].try_into().unwrap());
        let events = i16::from_ne_bytes(bytes[4..6].try_into().unwrap());
        if fd < 0 || events != libc::POLLIN {
            return Err(Errno::EOPNOTSUPP);
        }
        // bytes[6..8] is output-only, including arbitrary initial bit patterns.
        Ok(OriginalPollInput {
            fd,
            events,
            timeout_millis,
        })
    }
    pub(super) fn captured(&mut self, bytes: &[u8]) -> Result<(), Errno> {
        if self.input.is_some()
            || self.failed.load(Ordering::Acquire)
            || self.used.load(Ordering::Acquire)
        {
            return Err(Errno::ESTALE);
        }
        self.input = Some(Self::decode(bytes, self.call.1.arg2 as i32)?);
        Ok(())
    }
    pub(super) fn input(&self) -> Result<OriginalPollInput, Errno> {
        self.input.ok_or(Errno::ESTALE)
    }
    pub(super) fn state(&self) -> Result<(), Errno> {
        if self.failed.load(Ordering::Acquire) || self.input.is_none() {
            Err(Errno::ESTALE)
        } else {
            Ok(())
        }
    }
    pub(super) fn validate(&self, task: &Stopped, call: (Sysno, SyscallArgs)) -> Result<(), Errno> {
        self.state()?;
        if self.call != call || !self.terminal.same_generation(&task.terminal_cleanup()) {
            return Err(Errno::ESTALE);
        }
        Ok(())
    }
    pub(super) fn unused(&self) -> Result<(), Errno> {
        self.state()?;
        if self.used.load(Ordering::Acquire) {
            Err(Errno::EALREADY)
        } else {
            Ok(())
        }
    }
    pub(super) fn claim(&self) -> Result<(), Errno> {
        self.unused()?;
        self.used
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| Errno::EALREADY)?;
        Ok(())
    }
    pub(super) fn fail(&self) {
        self.failed.store(true, Ordering::Release);
    }
}
