/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

use reverie::Pid;
use thiserror::Error;

/// The controller operation whose LiteInst activation invariants failed.
///
/// This is internal to the ptrace-owned LiteInst runtime. Keeping it typed lets
/// tests and internal consumers distinguish failure paths without parsing diagnostic
/// text.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum LiteinstActivationOperation {
    ResumeInjectedSyscall,
    ResumeInterceptedInjectedSyscall,
    ResumeAfterSeccompStop,
    WaitForPostExecTrap,
    SkipInterceptedSyscall,
    FinishReinjectedSyscall,
    FinishInjectedSyscall,
}

impl LiteinstActivationOperation {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::ResumeInjectedSyscall => "resume injected syscall",
            Self::ResumeInterceptedInjectedSyscall => "resume intercepted injected syscall",
            Self::ResumeAfterSeccompStop => "resume after seccomp stop",
            Self::WaitForPostExecTrap => "wait for the LiteInst post-exec trap",
            Self::SkipInterceptedSyscall => "skip intercepted syscall",
            Self::FinishReinjectedSyscall => "finish reinjected syscall",
            Self::FinishInjectedSyscall => "finish injected syscall",
        }
    }
}

/// Stable classification for a fail-closed LiteInst activation error.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum LiteinstActivationFailureReason {
    UnexpectedPreinitSignal,
    ExecutableEntryBeforeHandshake,
    RestoreExecutableEntryGuard,
    UnexpectedActivationTrap,
    SignalBeforeHandshake(LiteinstActivationOperation),
    UnexpectedControllerProvenance(LiteinstActivationOperation),
    UnexpectedActivationSignal,
    PostStartExec,
    UnexpectedPostExecEvent,
    ExitedBeforePostExecTrap,
    InstallExecutableEntryGuard,
    TerminatedBeforeHandshake,
}

/// A typed LiteInst activation failure retaining its human-readable diagnostic.
#[derive(Debug, Error)]
#[error("{error}")]
pub(crate) struct LiteinstActivationFailure {
    reason: LiteinstActivationFailureReason,
    #[source]
    error: Error,
}

impl LiteinstActivationFailure {
    pub(crate) fn new(reason: LiteinstActivationFailureReason, error: Error) -> Self {
        Self { reason, error }
    }

    #[cfg(test)]
    pub(crate) const fn reason(&self) -> LiteinstActivationFailureReason {
        self.reason
    }
}

#[cfg(test)]
pub(crate) fn liteinst_activation_failure_reason(
    error: &reverie::Error,
) -> Option<LiteinstActivationFailureReason> {
    let reverie::Error::Tool(error) = error else {
        return None;
    };
    error
        .downcast_ref::<LiteinstActivationFailure>()
        .map(LiteinstActivationFailure::reason)
}

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

    fn activation_error(
        reason: LiteinstActivationFailureReason,
        message: &'static str,
    ) -> reverie::Error {
        anyhow::Error::new(LiteinstActivationFailure::new(
            reason,
            Error::runtime(Pid::from_raw(42), "activate LiteInst", message),
        ))
        .into()
    }

    #[test]
    fn liteinst_activation_reason_accepts_the_qualifying_typed_failure() {
        let reason = LiteinstActivationFailureReason::UnexpectedControllerProvenance(
            LiteinstActivationOperation::FinishInjectedSyscall,
        );
        let error = activation_error(reason, "diagnostic wording is not authoritative");

        assert_eq!(liteinst_activation_failure_reason(&error), Some(reason));
    }

    #[test]
    fn liteinst_activation_reason_rejects_tampered_diagnostic_text() {
        let expected = LiteinstActivationFailureReason::UnexpectedControllerProvenance(
            LiteinstActivationOperation::FinishInjectedSyscall,
        );
        let error = activation_error(
            LiteinstActivationFailureReason::UnexpectedActivationSignal,
            "finish injected syscall observed a nested signal without the expected controller provenance",
        );

        assert_ne!(liteinst_activation_failure_reason(&error), Some(expected));
    }
}
