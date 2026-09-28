/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! Panic payloads held by the concrete Tool owner until its cleanup finishes.

use std::sync::Mutex;

use super::owned_future::CaughtFuture;
use super::owned_future::PanicPayload;
use crate::Error;
use crate::Result;

#[derive(Default)]
pub(crate) struct ToolPanics {
    pending: Mutex<Vec<PanicPayload>>,
}

impl ToolPanics {
    pub(crate) fn take(&self) -> Vec<PanicPayload> {
        std::mem::take(&mut *self.pending.lock().expect("KVM Tool panic lock poisoned"))
    }

    pub(crate) fn append(&self, mut payloads: Vec<PanicPayload>) {
        self.pending
            .lock()
            .expect("KVM Tool panic lock poisoned")
            .append(&mut payloads);
    }

    /// Keep both an already returned error and any polling/destruction panic.
    /// A derived cancellation cannot hide a newly caught real panic.
    pub(crate) fn finish<R>(
        &self,
        caught: CaughtFuture<Result<R>>,
        phase: &'static str,
    ) -> Result<R> {
        let count = caught.panics.len();
        self.append(caught.panics);
        if count == 0 {
            return caught.output.expect("caught Tool future lost its outcome");
        }
        let mut diagnostics = (0..count)
            .map(|_| Error::GuestWorkerPanic.cleanup(phase))
            .collect::<Vec<_>>();
        match caught.output {
            Some(Err(error)) if !matches!(error.primary(), Error::RunAborted) => {
                Err(error.with_cleanup(diagnostics))
            }
            Some(Err(error)) => {
                let primary = diagnostics.remove(0);
                diagnostics.insert(0, error);
                Err(primary.with_cleanup(diagnostics))
            }
            Some(Ok(_)) | None => {
                let primary = diagnostics.remove(0);
                Err(primary.with_cleanup(diagnostics))
            }
        }
    }
}

#[cfg(test)]
#[path = "tool_panics_tests.rs"]
mod tests;
