/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

use core::marker::PhantomData;
use core::sync::atomic::AtomicI32;
use core::sync::atomic::Ordering;
use std::io;
use std::rc::Rc;

use tokio::io::AsyncReadExt;
use tokio::runtime::Handle;

use super::Child;
use super::Command;
use super::Error;
use super::Pid;
use super::PtyChild;
use super::fd::AsyncFd;
use super::fd::Fd;
use super::fd::pipe;
use super::seccomp::SeccompNotif;
use super::spawn::send_error;
use super::stdio::ChildStderr;
use super::stdio::ChildStdin;
use super::stdio::ChildStdout;
use super::util::SharedValue;

/// Original startup error retained by an owning parent continuation.
#[derive(Debug, thiserror::Error)]
pub enum CommandStartError {
    /// The existing process setup or child exec error, with its context intact.
    #[error(transparent)]
    Process(#[from] Error),
    /// A parent descriptor, reactor, or error-channel failure.
    #[error(transparent)]
    Io(#[from] io::Error),
}

/// A pre-birth slot for the original-thread execution owner's native startup.
///
/// This is an integration primitive, not a supervising scope. Its caller must
/// retain the slot outside client cancellation, establish a current-thread I/O
/// runtime before birth, and own that runtime and external resources until real
/// disposition. A runtime Handle alone does not provide that lifetime.
///
/// Dropping this slot does not kill or reap a child. Neither a birth PID nor the
/// seccomp-copy pidfd is exposed as cleanup authority. Early unavailable status
/// remains a separate obligation of the execution owner and existing notifier.
#[must_use]
#[derive(Default)]
pub struct CommandStart {
    attempted: bool,
    parent: Option<ParentContinuation>,
    exec: Option<ExecPipe>,
    runtime: Option<Handle>,
    refusal: Option<CommandStartError>,
    ready: bool,
    // Physical birth and later ptrace ownership remain on the original thread.
    _original_thread: PhantomData<Rc<()>>,
}

struct ExecPipe {
    reader: AsyncFd,
    writer: Option<Fd>,
    bytes: [u8; 8],
}

pub(super) struct ParentContinuation {
    pub(super) child: Option<Child>,
    pub(super) stdin: Option<Fd>,
    pub(super) stdout: Option<Fd>,
    pub(super) stderr: Option<Fd>,
    pub(super) child_ends: [Option<Fd>; 3],
    pub(super) pty: Option<PtyChild>,
    pub(super) seccomp_fd: Option<SharedValue<AtomicI32>>,
    pub(super) seccomp_pidfd: Option<Fd>,
    pub(super) seccomp_copy: Option<Fd>,
}

impl Command {
    /// Starts into an already-owned slot before any fallible parent conversion.
    ///
    /// The clone flags, birth thread, pre_exec order and child failure callback
    /// are the ordinary command path. The error-pipe reactor is established
    /// before clone; a pre-birth error creates no child. After successful clone,
    /// the native continuation is stored before returning or closing any parent
    /// copy of a child descriptor. Call `CommandStart::finish` on the owner side.
    ///
    /// # Panics
    /// Panics before another birth if this slot was already used. Like ordinary
    /// asynchronous stdio, requires an I/O-enabled Tokio runtime; this is checked
    /// by constructing the error reader before birth.
    #[doc(hidden)]
    pub fn spawn_into<'a>(
        &mut self,
        start: &'a mut CommandStart,
    ) -> Result<(), &'a CommandStartError> {
        assert!(!start.attempted, "startup slot must be fresh");
        start.attempted = true;
        let result = (|| -> Result<(), CommandStartError> {
            let runtime = Handle::try_current().map_err(io::Error::other)?;
            let (reader, writer) = pipe().map_err(Error::from)?;
            let reader = AsyncFd::readable(reader).map_err(Error::from)?;
            start.runtime = Some(runtime);
            start.exec = Some(ExecPipe {
                reader,
                writer: Some(writer),
                bytes: [0; 8],
            });
            let exec = start.exec.as_mut().expect("stored before birth");
            let mut onfail = |error| {
                send_error(exec.writer.as_mut().expect("child error writer"), error);
                1
            };
            self.spawn_parent_into(&mut start.parent, &mut onfail)?;
            Ok(())
        })();
        if let Err(error) = result {
            start.refusal = Some(error);
        }
        match &start.refusal {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }
}

impl CommandStart {
    /// Allocates no child or OS resource; the enclosing owner stores this first.
    pub fn new() -> Self {
        Self::default()
    }

    /// Actual successful clone result, for association with the normal notifier.
    /// This is not permission to reopen or signal a numeric PID.
    pub fn child_id(&self) -> Option<Pid> {
        self.parent.as_ref()?.child.as_ref().map(Child::id)
    }

    /// Returns the first actual startup refusal without discarding parent state.
    pub fn refusal(&self) -> Option<&CommandStartError> {
        self.refusal.as_ref()
    }

    /// Advances the owned native continuation without consuming it on error.
    ///
    /// Dropping this borrowed future leaves the raw/converted descriptors,
    /// seccomp exchange and partial error-channel state in the slot. On refusal,
    /// later calls return the same error; they do not retry an operation. EOF on
    /// the ordinary exec pipe only completes parent setup, not terminal cleanup.
    pub async fn finish(&mut self) -> Result<(), &CommandStartError> {
        if self.refusal.is_none()
            && !self.ready
            && let Err(error) = self.advance().await
        {
            self.refusal = Some(error);
        }
        match &self.refusal {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }

    /// Transfers a fully prepared Child to the execution owner's next phase.
    ///
    /// The receiver must accept ownership before exposing a client result. A
    /// failed or incomplete startup cannot transfer via this method, and its
    /// actual resources remain in the slot for an honest unresolved disposition.
    pub fn take_child(&mut self) -> Option<Child> {
        if self.ready && self.refusal.is_none() {
            self.parent.as_mut()?.child.take()
        } else {
            None
        }
    }

    async fn advance(&mut self) -> Result<(), CommandStartError> {
        let parent = self
            .parent
            .as_mut()
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "no child was started"))?;
        let exec = self.exec.as_mut().expect("native slot owns exec channel");
        let runtime = self
            .runtime
            .as_ref()
            .expect("reactor established before birth");
        parent.close_child_copies();
        drop(exec.writer.take());

        if let Some(shared) = &parent.seccomp_fd
            && parent.child.as_ref().unwrap().seccomp_notif.is_none()
        {
            // A completed exchange owns the actual SeccompNotif. Cancellation
            // later at the exec pipe must not re-enter the zeroed exchange.
            // While pending, the owner can poll other operations/cancellation.
            let target = loop {
                let target = shared.as_ref().load(Ordering::Relaxed);
                if target != 0 {
                    break target;
                }
                tokio::task::yield_now().await;
            };
            let child = parent
                .child
                .as_ref()
                .expect("child not transferred during startup");
            parent.seccomp_pidfd = Some(Fd::pidfd_open(child.pid.into(), 0).map_err(Error::from)?);
            parent.seccomp_copy = Some(
                parent
                    .seccomp_pidfd
                    .as_ref()
                    .unwrap()
                    .pidfd_getfd(target, 0)
                    .map_err(Error::from)?,
            );
            shared.as_ref().store(0, Ordering::Relaxed);
            // Keep the shared map and actual pidfd in the continuation on any
            // later error. The pidfd is for the existing seccomp-copy operation,
            // not a new authenticated signal or wait capability.
            let _entered = runtime.enter();
            register(
                &mut parent.seccomp_copy,
                &mut parent.child.as_mut().unwrap().seccomp_notif,
                SeccompNotif::try_new,
            )?;
        }
        {
            let _entered = runtime.enter();
            let child = parent.child.as_mut().unwrap();
            register(&mut parent.stdin, &mut child.stdin, ChildStdin::try_new)?;
            register(&mut parent.stdout, &mut child.stdout, ChildStdout::try_new)?;
            register(&mut parent.stderr, &mut child.stderr, ChildStderr::try_new)?;
        }
        loop {
            match exec.reader.read(&mut exec.bytes).await {
                Ok(0) => {
                    self.ready = true;
                    return Ok(());
                }
                Ok(8) => return Err(Error::from(exec.bytes).into()),
                Ok(count) => {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!("execve pipe: got unexpected number of bytes {count}"),
                    )
                    .into());
                }
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(error) => return Err(error.into()),
            }
        }
    }
}

fn register<T>(
    raw: &mut Option<Fd>,
    ready: &mut Option<T>,
    convert: impl FnOnce(Fd) -> Result<T, (Fd, io::Error)>,
) -> Result<(), io::Error> {
    if let Some(fd) = raw.take() {
        match convert(fd) {
            Ok(converted) => *ready = Some(converted),
            Err((fd, error)) => {
                *raw = Some(fd);
                return Err(error);
            }
        }
    }
    Ok(())
}

impl ParentContinuation {
    fn close_child_copies(&mut self) {
        for fd in &mut self.child_ends {
            drop(fd.take());
        }
        drop(self.pty.take());
    }

    // Compatibility-only consuming finish. Preserve the existing ordering,
    // blocking wait and error/destructor behavior for unintegrated callers.
    pub(super) fn finish_legacy(mut self) -> Result<Child, Error> {
        self.close_child_copies();
        let pid = self.child.as_ref().unwrap().pid;
        let seccomp_notif = match self.seccomp_fd.take() {
            Some(shared_fd) => {
                let mut targetfd = 0;
                while targetfd == 0 {
                    targetfd = shared_fd.as_ref().load(Ordering::Relaxed);
                    std::thread::yield_now();
                }
                let pidfd = Fd::pidfd_open(pid.into(), 0)?;
                let fd = pidfd.pidfd_getfd(targetfd, 0)?;
                shared_fd.as_ref().store(0, Ordering::Relaxed);
                Some(SeccompNotif::new(fd)?)
            }
            None => None,
        };
        let stdin = self.stdin.take().map(ChildStdin::new).transpose()?;
        let stdout = self.stdout.take().map(ChildStdout::new).transpose()?;
        let stderr = self.stderr.take().map(ChildStderr::new).transpose()?;
        Ok(Child {
            pid,
            exit_status: None,
            seccomp_notif,
            stdin,
            stdout,
            stderr,
        })
    }
}
