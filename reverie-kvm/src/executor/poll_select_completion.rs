/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! Executor completion facts, not signal-selection or restoration authority.
//! Final EINTR can retain the saved-mask obligation after timeout finishing.

use crate::signal::KernelSigset;

const NOHAND: i64 = -514;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PollSelectRestart {
    None,
    NoHandler,
}

/// Actual wait outcome after syscall-specific fd-result copyout. Never
/// constructed by classifying a Tool's scalar errno or a host EINTR.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum PollSelectWaitResult {
    Returned(i64),
    SignalPending,
}

#[derive(Debug, Eq, PartialEq)]
#[must_use]
pub(crate) struct DeferredPollSelectMask {
    original: KernelSigset,
}

#[derive(Debug, Eq, PartialEq)]
pub(crate) enum PollSelectMask {
    NotInstalled,
    Restored,
    Pending(DeferredPollSelectMask),
}

/// Result of the actual NULL/STICKY/initial-zero/remaining-time-copyout path.
/// The caller supplies real elapsed time and preserves Linux's branch order.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum PollSelectTimeoutFinish {
    Unchanged,
    CannotRestart,
}

#[derive(Debug, Eq, PartialEq)]
#[must_use]
pub(crate) struct PollSelectCompletion {
    raw: i64,
    restart: PollSelectRestart,
    mask: PollSelectMask,
}

impl PollSelectCompletion {
    pub(crate) fn raw(&self) -> i64 {
        self.raw
    }

    /// Current scalar ppoll/pselect paths install no temporary guest mask and
    /// provide no genuine restart classification, regardless of their errno.
    pub(super) fn existing(raw: i64) -> Self {
        Self {
            raw,
            restart: PollSelectRestart::None,
            mask: PollSelectMask::NotInstalled,
        }
    }

    /// Linux restores-or-defers before attempting timeout writeback. This
    /// helper is not activated by the current scalar producers.
    pub(super) fn begin_finish(
        result: PollSelectWaitResult,
        installed_original: Option<KernelSigset>,
        restore_now: impl FnOnce(KernelSigset),
    ) -> Self {
        let (raw, restart) = match result {
            PollSelectWaitResult::Returned(raw) => (raw, PollSelectRestart::None),
            PollSelectWaitResult::SignalPending => (NOHAND, PollSelectRestart::NoHandler),
        };
        let mask = match installed_original {
            None => PollSelectMask::NotInstalled,
            Some(original) if result == PollSelectWaitResult::SignalPending => {
                PollSelectMask::Pending(DeferredPollSelectMask { original })
            }
            Some(original) => {
                restore_now(original);
                PollSelectMask::Restored
            }
        };
        Self { raw, restart, mask }
    }

    pub(super) fn finish_timeout(mut self, finish: PollSelectTimeoutFinish) -> Self {
        if finish == PollSelectTimeoutFinish::CannotRestart
            && self.restart == PollSelectRestart::NoHandler
        {
            self.raw = -(libc::EINTR as i64);
            self.restart = PollSelectRestart::None;
        }
        self
    }

    /// Move independent facts into the eventual durable consumer. This move
    /// itself authorizes no signal selection, frame write or mask restoration.
    pub(crate) fn into_parts(self) -> (i64, PollSelectRestart, PollSelectMask) {
        (self.raw, self.restart, self.mask)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper-level ordering only. Actual read-only timeout copyout and actual
    /// handler masks still require producer/runtime integration coverage.
    #[test]
    fn timeout_eintr_retains_mask_without_restart_classification() {
        let mut original = KernelSigset::default();
        original.insert(libc::SIGUSR1);
        let finished = PollSelectCompletion::begin_finish(
            PollSelectWaitResult::SignalPending,
            Some(original),
            |_| panic!("NOHAND must not restore before timeout finishing"),
        )
        .finish_timeout(PollSelectTimeoutFinish::CannotRestart);
        assert_eq!(
            finished.into_parts(),
            (
                -(libc::EINTR as i64),
                PollSelectRestart::None,
                PollSelectMask::Pending(DeferredPollSelectMask { original }),
            )
        );

        let unmasked = PollSelectCompletion::begin_finish(
            PollSelectWaitResult::SignalPending,
            None,
            |_| unreachable!(),
        )
        .finish_timeout(PollSelectTimeoutFinish::Unchanged);
        assert_eq!(
            unmasked.into_parts(),
            (
                NOHAND,
                PollSelectRestart::NoHandler,
                PollSelectMask::NotInstalled,
            )
        );

        let restored = std::cell::Cell::new(None);
        let ready = PollSelectCompletion::begin_finish(
            PollSelectWaitResult::Returned(2),
            Some(original),
            |mask| restored.set(Some(mask)),
        );
        assert_eq!(restored.get(), Some(original));
        let ready = ready.finish_timeout(PollSelectTimeoutFinish::CannotRestart);
        assert_eq!(
            ready.into_parts(),
            (2, PollSelectRestart::None, PollSelectMask::Restored)
        );
    }
}
