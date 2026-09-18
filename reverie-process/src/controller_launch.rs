/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

use core::fmt;
use std::io;
use std::io::Write;
use std::num::NonZeroU64;
use std::os::fd::AsRawFd;
use std::os::fd::OwnedFd;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;

use syscalls::Errno;

use super::ChildStderr;
use super::ChildStdin;
use super::ChildStdout;
use super::Context;
use super::Error;
use super::Pid;
use super::fd::Fd;

static NEXT_CONTROLLER_LAUNCH_ID: AtomicU64 = AtomicU64::new(1);

/// Session-scoped identity of one controller-only clone operation.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ControllerLaunchId {
    controller_tgid: Pid,
    controller_tid: Pid,
    sequence: NonZeroU64,
}

impl ControllerLaunchId {
    pub(super) fn allocate() -> Result<Self, Error> {
        let raw = NEXT_CONTROLLER_LAUNCH_ID
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |current| {
                current.checked_add(1)
            })
            .map_err(|_| Error::new(Errno::EOVERFLOW, Context::Clone))?;
        let sequence = NonZeroU64::new(raw)
            .ok_or_else(|| Error::new(Errno::EOVERFLOW, Context::Clone))?;
        let controller_tid_raw = unsafe { libc::syscall(libc::SYS_gettid) };
        if controller_tid_raw <= 0 || controller_tid_raw > i64::from(i32::MAX) {
            return Err(Error::new(
                if controller_tid_raw == -1 {
                    Errno::last()
                } else {
                    Errno::EPROTO
                },
                Context::Clone,
            ));
        }
        Ok(Self {
            controller_tgid: Pid::from_raw(unsafe { libc::getpid() }),
            controller_tid: Pid::from_raw(controller_tid_raw as libc::pid_t),
            sequence,
        })
    }

    /// Returns the spawning controller's thread-group ID.
    pub fn controller_tgid(self) -> Pid {
        self.controller_tgid
    }

    /// Returns the exact spawning controller task ID.
    pub fn controller_tid(self) -> Pid {
        self.controller_tid
    }

    /// Returns the nonzero controller-local launch sequence.
    pub fn sequence(self) -> u64 {
        self.sequence.get()
    }
}

#[derive(Debug)]
struct ControllerSpawnTokenInner {
    launch: ControllerLaunchId,
    child: Pid,
    pidfd: OwnedFd,
}

/// Linear proof of a controller child created with an atomic clone-time pidfd.
///
/// Dropping an unconsumed token aborts rather than silently discarding the sole
/// exact-generation cleanup authority.
#[must_use = "controller spawn authority must be transferred to exact cleanup"]
#[derive(Debug)]
pub struct ControllerSpawnToken {
    inner: Option<ControllerSpawnTokenInner>,
}

impl ControllerSpawnToken {
    pub(super) fn new(launch: ControllerLaunchId, child: Pid, pidfd: OwnedFd) -> Self {
        Self {
            inner: Some(ControllerSpawnTokenInner {
                launch,
                child,
                pidfd,
            }),
        }
    }

    fn inner(&self) -> &ControllerSpawnTokenInner {
        self.inner
            .as_ref()
            .expect("controller spawn token was already consumed")
    }

    /// Returns this launch's session-scoped controller identity.
    pub fn launch_id(&self) -> ControllerLaunchId {
        self.inner().launch
    }

    /// Returns the exact child PID returned by clone.
    pub fn child(&self) -> Pid {
        self.inner().child
    }

    /// Returns the controller thread-group ID at clone return.
    pub fn controller_tgid(&self) -> Pid {
        self.inner().launch.controller_tgid
    }

    /// Returns the exact controller task ID which called clone.
    pub fn controller_tid(&self) -> Pid {
        self.inner().launch.controller_tid
    }

    /// Consumes the token into its launch ID, child PID, and exact pidfd.
    pub fn into_parts(mut self) -> (ControllerLaunchId, Pid, OwnedFd) {
        let inner = self
            .inner
            .take()
            .expect("controller spawn token was already consumed");
        (inner.launch, inner.child, inner.pidfd)
    }

    pub(super) fn validate_pidfd_cloexec(&self) -> Result<(), Errno> {
        let flags = Errno::result(unsafe {
            libc::fcntl(self.inner().pidfd.as_raw_fd(), libc::F_GETFD)
        })?;
        if flags & libc::FD_CLOEXEC == 0 {
            return Err(Errno::EOPNOTSUPP);
        }
        Ok(())
    }

}

impl Drop for ControllerSpawnToken {
    fn drop(&mut self) {
        if let Some(inner) = self.inner.as_ref() {
            // Exactly one raw attempt through the still-owned pidfd. There is
            // intentionally no retry, wait, or numeric-PID fallback.
            let _ = unsafe {
                libc::syscall(
                    libc::SYS_pidfd_send_signal,
                    inner.pidfd.as_raw_fd(),
                    libc::SIGKILL,
                    std::ptr::null::<libc::siginfo_t>(),
                    0,
                )
            };
            std::process::abort();
        }
    }
}

const STARTUP_RECORD_LEN: usize = 16;
const STARTUP_RECORD_MAGIC: [u8; 4] = *b"RCTL";
const STARTUP_RECORD_VERSION: u8 = 1;
const STARTUP_RECORD_READY: u8 = 1;
const STARTUP_RECORD_ERROR: u8 = 2;

/// Child-side publisher for the fixed controller-ownership startup record.
///
/// A READY record means `PTRACE_TRACEME` succeeded and is published
/// immediately before the initial SIGSTOP. It does not claim that exec
/// succeeded.
pub struct ControllerStartupPublisher {
    raw_fd: Option<libc::c_int>,
}

impl ControllerStartupPublisher {
    pub(super) fn new(raw_fd: libc::c_int) -> Self {
        Self { raw_fd: Some(raw_fd) }
    }

    fn publish(&mut self, kind: u8, error: Option<Error>) -> Result<(), Errno> {
        let fd = self.raw_fd.take().ok_or(Errno::EALREADY)?;
        let mut record = [0u8; STARTUP_RECORD_LEN];
        record[..4].copy_from_slice(&STARTUP_RECORD_MAGIC);
        record[4] = STARTUP_RECORD_VERSION;
        record[5] = kind;
        if let Some(error) = error {
            record[8..12].copy_from_slice(&error.errno().into_raw().to_ne_bytes());
            record[12..16].copy_from_slice(&error.context().wire_value().to_ne_bytes());
        }
        let written = unsafe { libc::write(fd, record.as_ptr().cast(), record.len()) };
        let write_error = (written == -1).then(Errno::last);
        let _ = unsafe { libc::close(fd) };
        match (written, write_error) {
            (written, _) if written == record.len() as isize => Ok(()),
            (-1, Some(error)) => Err(error),
            _ => Err(Errno::EIO),
        }
    }

    /// Publishes exact controller ownership immediately before SIGSTOP.
    pub fn publish_ready(&mut self) -> Result<(), Errno> {
        self.publish(STARTUP_RECORD_READY, None)
    }

    pub(super) fn publish_error(&mut self, error: Error) {
        if self.raw_fd.is_some() {
            let _ = self.publish(STARTUP_RECORD_ERROR, Some(error));
        }
    }

    pub(super) fn close_without_record(&mut self) {
        if let Some(fd) = self.raw_fd.take() {
            let _ = unsafe { libc::close(fd) };
        }
    }

    pub(super) fn is_published(&self) -> bool {
        self.raw_fd.is_none()
    }
}

impl fmt::Debug for ControllerStartupPublisher {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ControllerStartupPublisher")
            .field("published", &self.is_published())
            .finish()
    }
}

pub(super) enum ControllerStartupRecord {
    Ready,
    Error(Error),
}

pub(super) fn decode_startup_record(
    record: [u8; STARTUP_RECORD_LEN],
) -> Option<ControllerStartupRecord> {
    if record[..4] != STARTUP_RECORD_MAGIC
        || record[4] != STARTUP_RECORD_VERSION
        || record[6..8] != [0, 0]
    {
        return None;
    }
    match record[5] {
        STARTUP_RECORD_READY if record[8..].iter().all(|byte| *byte == 0) => {
            Some(ControllerStartupRecord::Ready)
        }
        STARTUP_RECORD_ERROR => {
            let errno = i32::from_ne_bytes(record[8..12].try_into().ok()?);
            if !(1..4096).contains(&errno) {
                return None;
            }
            let context = u32::from_ne_bytes(record[12..16].try_into().ok()?);
            let context = Context::try_from_wire(context)?;
            Some(ControllerStartupRecord::Error(Error::new(
                Errno::new(errno),
                context,
            )))
        }
        _ => None,
    }
}

pub(super) const fn startup_record_len() -> usize {
    STARTUP_RECORD_LEN
}

/// Kernel-execution phase retained with an incomplete controller launch.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ControllerLaunchPhase {
    /// The gate closed without releasing the child into container setup.
    ContainedBeforeControllerInit,
    /// The child was released through the gate, with no valid READY record yet.
    ChildReleased,
    /// TRACEME ownership was established immediately before initial SIGSTOP.
    TracerOwnershipReady,
}

/// Consuming payload of a controller launch or incomplete launch authority.
#[must_use = "controller launch parts contain exact cleanup authority"]
#[derive(Debug)]
pub struct ControllerLaunchParts {
    /// Exact controller-spawn token and atomic pidfd.
    pub token: ControllerSpawnToken,
    /// Optional parent-side stdin handle created before clone.
    pub stdin: Option<ChildStdin>,
    /// Optional parent-side stdout handle created before clone.
    pub stdout: Option<ChildStdout>,
    /// Optional parent-side stderr handle created before clone.
    pub stderr: Option<ChildStderr>,
    /// Retained controller-startup phase at the ownership handoff.
    pub phase: ControllerLaunchPhase,
}

/// Controller launch with TRACEME ownership established before SIGSTOP.
#[must_use = "controller launch must be transferred into safeptrace"]
#[derive(Debug)]
pub struct ControllerLaunch {
    parts: Option<ControllerLaunchParts>,
}

impl ControllerLaunch {
    pub(super) fn from_pending(mut pending: PendingControllerLaunch) -> Self {
        debug_assert_eq!(pending.phase, ControllerLaunchPhase::TracerOwnershipReady);
        pending.startup_reader = None;
        Self {
            parts: pending.parts.take(),
        }
    }

    /// Returns the exact child PID without consuming launch authority.
    pub fn id(&self) -> Pid {
        self.parts
            .as_ref()
            .expect("controller launch was already consumed")
            .token
            .child()
    }

    /// Consumes the launch into the exact token and prebuilt parent stdio.
    pub fn into_parts(mut self) -> ControllerLaunchParts {
        self.parts
            .take()
            .expect("controller launch was already consumed")
    }
}

/// Exact post-clone authority returned when controller launch cannot finish.
#[must_use = "post-clone authority must be consumed by exact pidfd cleanup"]
#[derive(Debug)]
pub struct PendingControllerLaunch {
    parts: Option<ControllerLaunchParts>,
    gate_writer: Option<Fd>,
    startup_reader: Option<Fd>,
    phase: ControllerLaunchPhase,
}

impl PendingControllerLaunch {
    pub(super) fn new(
        token: ControllerSpawnToken,
        stdin: Option<ChildStdin>,
        stdout: Option<ChildStdout>,
        stderr: Option<ChildStderr>,
        gate_writer: Fd,
        startup_reader: Fd,
    ) -> Self {
        Self {
            parts: Some(ControllerLaunchParts {
                token,
                stdin,
                stdout,
                stderr,
                phase: ControllerLaunchPhase::ContainedBeforeControllerInit,
            }),
            gate_writer: Some(gate_writer),
            startup_reader: Some(startup_reader),
            phase: ControllerLaunchPhase::ContainedBeforeControllerInit,
        }
    }

    pub(super) fn release_gate(&mut self) -> io::Result<()> {
        let result = self
            .gate_writer
            .as_mut()
            .expect("controller child gate was already resolved")
            .write(&[1]);
        self.gate_writer = None;
        match result {
            Ok(1) => {
                self.phase = ControllerLaunchPhase::ChildReleased;
                self.parts
                    .as_mut()
                    .expect("pending controller launch lost its authority")
                    .phase = ControllerLaunchPhase::ChildReleased;
                Ok(())
            }
            Ok(written) => Err(io::Error::new(
                io::ErrorKind::WriteZero,
                format!("controller child gate wrote {written} bytes"),
            )),
            Err(error) => Err(error),
        }
    }

    pub(super) fn startup_reader_mut(&mut self) -> &mut Fd {
        self.startup_reader
            .as_mut()
            .expect("controller startup-record reader was already consumed")
    }

    pub(super) fn mark_ready(&mut self) {
        debug_assert_eq!(self.phase, ControllerLaunchPhase::ChildReleased);
        self.phase = ControllerLaunchPhase::TracerOwnershipReady;
        self.parts
            .as_mut()
            .expect("pending controller launch lost its authority")
            .phase = ControllerLaunchPhase::TracerOwnershipReady;
    }

    /// Returns the exact child PID without consuming cleanup authority.
    pub fn id(&self) -> Pid {
        self.parts
            .as_ref()
            .expect("pending controller launch was already consumed")
            .token
            .child()
    }

    /// Consumes the incomplete launch into its exact pidfd and parent stdio.
    pub fn into_parts(mut self) -> ControllerLaunchParts {
        self.gate_writer = None;
        self.startup_reader = None;
        self.parts
            .take()
            .expect("pending controller launch was already consumed")
    }
}

/// Post-clone operation that failed while exact launch authority was retained.
#[derive(Debug)]
pub enum ControllerSpawnFailure {
    /// Releasing the pre-exec child gate failed.
    GateWrite(io::Error),
    /// The clone-returned pidfd failed exact descriptor validation.
    PidfdValidation(Errno),
    /// Reading the fixed startup record failed, including EINTR.
    StartupRecordRead(io::Error),
    /// The startup record had an unexpected byte count, including EOF.
    StartupRecordShortRead(usize),
    /// The fixed startup record failed exact tag/version/payload validation.
    StartupRecordMalformed,
    /// The child reported a concrete setup or controller-initialization error.
    ChildStartup(Error),
}

impl ControllerSpawnFailure {
    /// Returns the exact errno when one exists, otherwise a protocol errno.
    pub fn errno(&self) -> Errno {
        match self {
            Self::GateWrite(error) | Self::StartupRecordRead(error) => error
                .raw_os_error()
                .map(Errno::new)
                .unwrap_or(Errno::EIO),
            Self::PidfdValidation(error) => *error,
            Self::StartupRecordShortRead(_) | Self::StartupRecordMalformed => Errno::EPROTO,
            Self::ChildStartup(error) => error.errno(),
        }
    }
}

impl fmt::Display for ControllerSpawnFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::GateWrite(error) => write!(f, "release controller child gate: {error}"),
            Self::PidfdValidation(error) => {
                write!(f, "validate clone-returned controller pidfd: {error}")
            }
            Self::StartupRecordRead(error) => write!(f, "read controller startup record: {error}"),
            Self::StartupRecordShortRead(bytes) => {
                write!(f, "controller startup record contained {bytes} bytes")
            }
            Self::StartupRecordMalformed => write!(f, "malformed controller startup record"),
            Self::ChildStartup(error) => write!(f, "controller child startup failed: {error}"),
        }
    }
}

impl std::error::Error for ControllerSpawnFailure {}

/// Failure to create a controller-only launch.
#[derive(Debug)]
pub enum ControllerSpawnError {
    /// Failure before clone; no child was created.
    BeforeClone(Error),
    /// The kernel violated the probed clone-pidfd contract; the gated child
    /// exited without executing and was synchronously reaped.
    UnsupportedKernelContract(Error),
    /// Failure after clone with the sole exact pidfd authority retained.
    AfterClone {
        /// Exact failing operation.
        source: ControllerSpawnFailure,
        /// Linear authority which must be consumed by exact cleanup.
        authority: PendingControllerLaunch,
    },
}

impl fmt::Display for ControllerSpawnError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BeforeClone(error) => write!(f, "controller launch before clone: {error}"),
            Self::UnsupportedKernelContract(error) => {
                write!(f, "unsupported clone-pidfd kernel contract: {error}")
            }
            Self::AfterClone { source, .. } => write!(f, "controller launch after clone: {source}"),
        }
    }
}

impl std::error::Error for ControllerSpawnError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::BeforeClone(error) | Self::UnsupportedKernelContract(error) => Some(error),
            Self::AfterClone { source, .. } => Some(source),
        }
    }
}
