/* Copyright (c) Meta Platforms, Inc. and affiliates. All rights reserved.
 * Licensed under the BSD-style license in the root LICENSE file. */

//! Synchronous, nonescaping destination custody. The callback can record the
//! actual outcome on its existing semantic Call before ANY hold is released.
//! This inactive backend primitive grants no network-prefix consumption.

use reverie::syscalls::FollowedStore;
use reverie::syscalls::NativeUserReadRefusal as Evidence;
use reverie::syscalls::NativeUserStoreOutcome as Outcome;
use reverie::syscalls::NativeUserStoreRefusal as Refusal;

use super::*;

fn state(error: Errno) -> Refusal {
    Refusal::Evidence(Evidence::TargetState(error))
}

struct Store<'a> {
    permit: safeptrace::NativeStorePermit<'a>,
    check: &'a dyn Fn() -> Result<(), Errno>,
    claim: &'a dyn Fn() -> Result<(), Errno>,
    address: usize,
    capacity: usize,
    used: bool,
}
impl FollowedStore for Store<'_> {
    fn store(&mut self, bytes: &[u8]) -> Outcome {
        if std::mem::replace(&mut self.used, true) {
            return Outcome::Refused(state(Errno::EALREADY));
        }
        if bytes.is_empty() || bytes.len() > self.capacity {
            return Outcome::Refused(Refusal::Evidence(Evidence::UnsupportedRange));
        }
        if let Err(error) = (self.check)().and_then(|_| self.permit.validate()) {
            return Outcome::Refused(state(error));
        }
        if let Err(error) = (self.claim)() {
            return Outcome::Refused(state(error));
        }
        match self.permit.write(self.address, bytes) {
            Outcome::Refused(error) => Outcome::Refused(error),
            Outcome::Attempted { raw, postcheck } => {
                // Actual kernel result survives ALL later failures. The caller
                // receives this while the entire cohort and notifier ticket
                // remain held; callback return cannot manufacture this result.
                let postcheck = postcheck
                    .and_then(|_| (self.check)())
                    .and_then(|_| self.permit.validate());
                Outcome::Attempted { raw, postcheck }
            }
        }
    }
}

impl<L: Tool + 'static> TracedTask<L> {
    pub(super) fn with_native_followed_store<R>(
        &self,
        original: Syscall,
        action: impl FnOnce(&mut dyn FollowedStore) -> R,
    ) -> Result<R, Refusal> {
        let session = &self.global_state.fatal_session;
        let alive = || {
            if !session.source_jobs.enabled()
                || session.is_failed()
                || self.cancel_handler.load(Ordering::Acquire)
            {
                Err(Errno::ECANCELED)
            } else if !session.source_jobs.idle() {
                Err(Errno::EBUSY)
            } else {
                Ok(())
            }
        };
        alive().map_err(state)?;
        self.original_scalar_store_unused().map_err(state)?;
        let (nr, args) = original.into_parts();
        if nr != Sysno::read
            && !(nr == Sysno::recvfrom && args.arg3 == 0 && args.arg4 == 0 && args.arg5 == 0)
        {
            return Err(Refusal::Evidence(Evidence::UnsupportedRange));
        }
        let member = self
            .cohort
            .as_ref()
            .ok_or(Refusal::Evidence(Evidence::UnsupportedBackend))?;
        let hold = member.acquire().map_err(state)?;
        let sender = hold.sender();
        let permit = sender.begin_native_store().map_err(state)?;
        // The original full capacity must have Linux's range verdict, even
        // when this operation only writes a shorter positive prefix.
        if self
            .inspect_native_scalar_receive_range(nr, args)
            .map_err(|_| state(Errno::ESTALE))?
            != reverie::OriginalReadRangeVerdict::Allowed
        {
            return Err(Refusal::Evidence(Evidence::UnsupportedRange));
        }
        let check = || {
            alive()?;
            hold.validate()?;
            self.validate_original_scalar_store(nr, args)
        };
        check().map_err(state)?;
        let claim = || self.claim_original_scalar_store();
        let mut writer = Store {
            permit,
            check: &check,
            claim: &claim,
            address: args.arg1,
            capacity: args.arg2,
            used: false,
        };
        // No await, release/reacquire, injection or guest continuation is
        // possible through the borrowed capability. The callback owns outcome
        // retention, not a caller-reported completion predicate.
        Ok(action(&mut writer))
    }
}

impl<L: Tool + 'static> TracedTask<L> {
    pub(super) fn with_restored_native_followed_store<R>(
        &self,
        original: Syscall,
        action: impl FnOnce(&mut dyn FollowedStore) -> R,
    ) -> Result<R, Refusal> {
        let session = &self.global_state.fatal_session;
        let alive = || {
            if !session.source_jobs.enabled()
                || session.is_failed()
                || self.cancel_handler.load(Ordering::Acquire)
            {
                Err(Errno::ECANCELED)
            } else if !session.source_jobs.idle() {
                Err(Errno::EBUSY)
            } else {
                Ok(())
            }
        };
        alive().map_err(state)?;
        let entry = self.restored_receive_entry().map_err(state)?;
        entry.retained_store_unused().map_err(state)?;
        self.validate_restored_receive(original).map_err(state)?;
        let (_, args) = original.into_parts();
        let member = self
            .cohort
            .as_ref()
            .ok_or(Refusal::Evidence(Evidence::UnsupportedBackend))?;
        let hold = member.acquire().map_err(state)?;
        let sender = hold.sender();
        let permit = sender.begin_native_store().map_err(state)?;
        let check = || {
            alive()?;
            hold.validate()?;
            self.validate_restored_receive(original)
        };
        check().map_err(state)?;
        let verdict = entry.inspect_retained_range().map_err(state)?;
        check().map_err(state)?;
        if verdict != reverie::OriginalReadRangeVerdict::Allowed {
            return Err(Refusal::Evidence(Evidence::UnsupportedRange));
        }
        let claim = || entry.claim_retained_store();
        let mut writer = Store {
            permit,
            check: &check,
            claim: &claim,
            address: args.arg1,
            capacity: args.arg2,
            used: false,
        };
        Ok(action(&mut writer))
    }
}
