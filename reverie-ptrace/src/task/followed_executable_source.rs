/* Copyright (c) Meta Platforms, Inc. and affiliates. All rights reserved.
 * Licensed under the BSD-style license in the root LICENSE file. */
//! Actual pre-GET ARM and registered worker collection under the whole hold.
//! AUTONOMOUS-BOT-IMPLEMENTED; TODO-HUMAN-REVIEW:
//! https://github.com/rrnewton/reverie/pull/897
use reverie::syscalls::ExecutableSourceArmer;
use reverie::syscalls::NativeUserReadError as ReadError;
use reverie::syscalls::NativeUserReadRefusal as ReadRefusal;

use super::*;

struct Attempt {
    session: Arc<FatalSession>,
    origin: BackendFailure,
    completed: bool,
}
impl Drop for Attempt {
    fn drop(&mut self) {
        if !self.completed {
            self.session.fail_at(
                self.origin,
                anyhow::anyhow!(
                    "executable source lost ARM/capture/collection/true-join authority"
                )
                .into(),
            );
        }
    }
}
impl<L: Tool + 'static> TracedTask<L> {
    pub(super) async fn stage_executable_source(
        &mut self,
        address: usize,
        length: usize,
        retention: Box<dyn Send + Sync>,
        armer: Box<dyn ExecutableSourceArmer>,
    ) -> Result<Vec<u8>, ReadError> {
        let refused = |error| ReadError::Refused(ReadRefusal::TargetState(error));
        let session = Arc::clone(&self.global_state.fatal_session);
        if !session.source_jobs.enabled()
            || session.is_failed()
            || self.cancel_handler.load(Ordering::Acquire)
        {
            return Err(refused(Errno::ECANCELED));
        }
        let member = self
            .cohort
            .as_ref()
            .ok_or(ReadError::Refused(ReadRefusal::UnsupportedBackend))?;
        if !session.source_jobs.idle() {
            return Err(refused(Errno::EBUSY));
        }
        let hold = Arc::new(member.acquire().map_err(refused)?);
        // Mandatory BEFORE arbitrary armer code. Synchronous arm/GET failure,
        // panic, uncertain registration, observer cancellation or worker failure
        // cannot become original guest success even if a Tool catches the error.
        let mut attempt = Attempt {
            session: Arc::clone(&session),
            origin: BackendFailure {
                pid: self.pid(),
                tid: self.tid(),
                phase: "executable source capture",
            },
            completed: false,
        };
        // Actual Member::acquire owns every original cohort member, and the
        // sticky guard above precedes arbitrary armer code. Both the full hold
        // and real capture allocation transfer to the existing joined Job.
        let plan = unsafe {
            safeptrace::FollowedExecutableSourceReadPlan::prepare(
                hold.sender(),
                address,
                length,
                armer,
            )
        }?;
        // Existing job retention, not another registry: keeps original capture
        // addresses allocated until real JoinHandle::join, including TLS teardown.
        #[cfg(all(test, cohort_final_test))]
        source_cohort::executable_tests::prepared(&plan);
        let capture_allocation = plan.keep_capture_allocation();
        let observer = session.source_jobs.submit_followed(
            Box::new((retention, capture_allocation)),
            Arc::clone(&hold),
            move || plan.run(),
        )?;
        let result = observer.await?;
        hold.validate().map_err(refused)?;
        if session.is_failed() || self.cancel_handler.load(Ordering::Acquire) {
            return Err(refused(Errno::ECANCELED));
        }
        attempt.completed = true;
        Ok(result)
    }
}
