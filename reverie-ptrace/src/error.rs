/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

use reverie::Pid;
use thiserror::Error;

/// A reverie-ptrace error. This error type isn't meant to be exposed to the
/// user.
#[derive(Error, Debug)]
pub enum Error {
    /// An internal error that is only ever meant to be used as a reverie-ptrace
    /// implementation detail. None of these errors should make it through to the
    /// user.
    #[error(transparent)]
    Internal(#[from] safeptrace::Error),

    /// A ptrace failure annotated with the operation and tracee that failed.
    #[error("{operation} failed for tracee {pid}: {source}")]
    Tracee {
        /// The high-level ptrace operation that was in progress.
        operation: &'static str,
        /// The tracee on which the operation was attempted.
        pid: Pid,
        /// The underlying ptrace error.
        #[source]
        source: safeptrace::Error,
    },

    /// An internal runtime failure that is not represented by safeptrace.
    #[error("{operation} failed for tracee {pid}: {message}")]
    Runtime {
        /// The runtime operation that was in progress.
        operation: &'static str,
        /// The affected tracee.
        pid: Pid,
        /// Additional diagnostic detail.
        message: String,
    },

    /// A public error.
    #[error(transparent)]
    External(#[from] reverie::Error),
}

impl Error {
    pub(crate) fn runtime(pid: Pid, operation: &'static str, message: impl Into<String>) -> Self {
        Self::Runtime {
            operation,
            pid,
            message: message.into(),
        }
    }
}

impl From<reverie::Errno> for Error {
    fn from(error: reverie::Errno) -> Self {
        Self::Internal(safeptrace::Error::Errno(error))
    }
}

/// Why a LiteInst activation-time failure was raised.
///
/// This classification is carried *with* the failure as a typed value so that
/// callers — and tests — never recover the reason by parsing a rendered
/// `Display` string. Message text is for humans; the `kind` is the fact.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LiteinstActivationFailureKind {
    /// A nested signal arrived during activation that did not originate from the
    /// controller's own injected / reinjected / skipped / post-exec step: it
    /// lacked the expected controller provenance and may be an external
    /// impersonation of an activation trap.
    NestedSignalProvenance,
    /// Any other activation-time rejection: handshake ordering, a queued-signal
    /// delivery attempt, an out-of-allowlist signal, an unexpected fault, or
    /// premature tracee termination.
    Other,
}

/// A LiteInst activation failure carrying a typed [`LiteinstActivationFailureKind`]
/// alongside a human-readable diagnostic message.
///
/// The `kind` is the classification of record. `Display` renders `message` for
/// diagnostics only; do not classify a failure by parsing that text. Because
/// this type implements [`std::error::Error`], it survives being wrapped in an
/// [`anyhow::Error`] and can be recovered downstream with `downcast_ref`,
/// keeping the condition bound to the value rather than to its rendering.
#[derive(Debug, Clone)]
pub struct LiteinstActivationError {
    kind: LiteinstActivationFailureKind,
    message: String,
}

impl LiteinstActivationError {
    pub(crate) fn new(
        kind: LiteinstActivationFailureKind,
        message: impl Into<String>,
    ) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }

    /// The typed classification of this failure.
    pub fn kind(&self) -> LiteinstActivationFailureKind {
        self.kind
    }
}

impl std::fmt::Display for LiteinstActivationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for LiteinstActivationError {}

pub(crate) trait TraceResultExt<T> {
    fn tracee_context(self, pid: Pid, operation: &'static str) -> Result<T, Error>;
}

impl<T> TraceResultExt<T> for Result<T, safeptrace::Error> {
    fn tracee_context(self, pid: Pid, operation: &'static str) -> Result<T, Error> {
        self.map_err(|source| Error::Tracee {
            operation,
            pid,
            source,
        })
    }
}

#[cfg(test)]
mod tests {
    use reverie::Errno;

    use super::*;

    #[test]
    fn tracee_error_includes_operation_and_pid() {
        let error = Err::<(), _>(safeptrace::Error::Errno(Errno::EPERM))
            .tracee_context(Pid::from_raw(42), "resume after seccomp stop")
            .expect_err("the synthetic ptrace operation should fail");

        let message = error.to_string();
        assert!(message.contains("resume after seccomp stop"));
        assert!(message.contains("42"));
        assert!(message.contains("Operation not permitted"));
    }
}
