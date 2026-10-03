/* Copyright (c) Meta Platforms, Inc. and affiliates. All rights reserved.
 * Licensed under the BSD-style license in the root LICENSE file. */
//! Physical completion of a fixed set of explicitly marked peer timers.
//! No scheduler continuation, source hold or guest timeout is issued here.

use super::*;

struct JoinAttempt {
    session: Arc<FatalSession>,
    origin: BackendFailure,
    completed: bool,
}
impl Drop for JoinAttempt {
    fn drop(&mut self) {
        if !self.completed {
            self.session.fail_at(
                self.origin,
                anyhow::anyhow!("peer observation-timer join abandoned before exact completion")
                    .into(),
            );
        }
    }
}

impl<L: Tool + 'static> TracedTask<L> {
    fn validate_timer_join_caller(&self, original: Syscall) -> Result<(), TraceError> {
        let session = &self.global_state.fatal_session;
        if !session.source_jobs.enabled()
            || session.is_failed()
            || !session.source_jobs.idle()
            || self.cancel_handler.load(Ordering::Acquire)
            || self.injected_syscall_frame.is_some()
            || self.pending_syscall_already_skipped
            || self.interrupted_read.is_some()
            || self.pending_signal.is_some()
            || self.private_signal.consulting
            || self.private_signal.frame.is_some()
            || self.private_signal.read.is_some()
            || self.private_signal.completing.is_some()
        {
            return Err(Errno::EBUSY.into());
        }
        let call = original.into_parts();
        if !matches!(
            call.0,
            Sysno::read | Sysno::recvfrom | Sysno::sendto | Sysno::poll
        ) {
            return Err(Errno::EOPNOTSUPP.into());
        }
        let task = self.assume_stopped();
        let logical = self.private_signal.logical.as_ref().ok_or(Errno::ESTALE)?;
        if !logical.matches_original(&task, call) {
            return Err(Errno::ESTALE.into());
        }
        if logical.receive.is_some() {
            self.validate_restored_receive(original)?;
        } else {
            if self.pending_syscall != Some(call) {
                return Err(Errno::ESTALE.into());
            }
            let actual =
                original_context::checked_entry_registers(&task).map_err(|_| Errno::ESTALE)?;
            if !logical.matches_frame(&actual) {
                return Err(Errno::ESTALE.into());
            }
            original_context::check_entry(&task, call.0, call.1, actual.rip, actual.rsp, true)?;
        }
        Ok(())
    }

    pub(super) async fn run_followed_timer_join(
        &mut self,
        original: Syscall,
    ) -> Result<(), TraceError> {
        self.validate_timer_join_caller(original)?;
        if self
            .private_signal
            .logical
            .as_ref()
            .is_none_or(|l| l.timer_join_pending)
        {
            return Err(Errno::EBUSY.into());
        }
        let member = self.cohort.as_ref().ok_or(Errno::EOPNOTSUPP)?;
        let snapshot = member.snapshot_receive_timers()?;
        self.private_signal
            .logical
            .as_mut()
            .ok_or(Errno::ESTALE)?
            .timer_join_pending = true;
        let mut attempt = JoinAttempt {
            session: Arc::clone(&self.global_state.fatal_session),
            origin: BackendFailure {
                pid: self.pid(),
                tid: self.tid(),
                phase: "peer observation-timer join",
            },
            completed: false,
        };
        snapshot.wait().await?;
        self.validate_timer_join_caller(original)?;
        if !snapshot.status()? {
            return Err(Errno::ESTALE.into());
        }
        self.private_signal
            .logical
            .as_mut()
            .ok_or(Errno::ESTALE)?
            .timer_join_pending = false;
        attempt.completed = true;
        Ok(())
    }
}
