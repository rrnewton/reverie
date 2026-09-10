use std::os::fd::AsFd;
use std::os::fd::AsRawFd;
use std::os::fd::OwnedFd;
use std::os::linux::process::PidFd;
use std::os::unix::process::ExitStatusExt;
use std::process::ExitStatus;

use tokio::io::unix::AsyncFd;

use super::*;

mod spawn;

pub(super) struct LoggedChild<'launch>(Child<'launch>);

impl<'launch> LoggedChild<'launch> {
    pub(super) fn spawn(
        command: std::process::Command,
        launch: &'launch mut dyn owned::LaunchLifetime,
        observer: RunObserver,
        handle: LogHandle,
    ) -> io::Result<Self> {
        Child::spawn(command, launch, observer, handle, Kernel).map(Self)
    }

    pub(super) fn adapt(&mut self) -> io::Result<()> {
        self.0.adapt()
    }

    pub(super) fn take_stdout(&mut self) -> Option<tokio::process::ChildStdout> {
        self.0.stdout.take()
    }

    pub(super) fn take_stderr(&mut self) -> Option<tokio::process::ChildStderr> {
        self.0.stderr.take()
    }

    pub(super) fn start_kill(&mut self) -> io::Result<()> {
        self.0.start_kill()
    }

    pub(super) async fn wait(&mut self) -> io::Result<ExitStatus> {
        self.0.wait().await
    }
}

pub(super) trait Operations: Send {
    fn signal(&mut self, pidfd: &PidFd) -> io::Result<()>;
    fn wait(&mut self, pidfd: &PidFd, blocking: bool) -> io::Result<Option<ExitStatus>>;
}

pub(super) struct Kernel;

impl Operations for Kernel {
    fn signal(&mut self, pidfd: &PidFd) -> io::Result<()> {
        pidfd.kill()
    }

    fn wait(&mut self, pidfd: &PidFd, blocking: bool) -> io::Result<Option<ExitStatus>> {
        let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
        let flags = libc::WEXITED | if blocking { 0 } else { libc::WNOHANG };
        let result = unsafe {
            libc::waitid(
                libc::P_PIDFD,
                pidfd.as_raw_fd() as libc::id_t,
                &mut info,
                flags,
            )
        };
        if result == -1 {
            return Err(io::Error::last_os_error());
        }
        if unsafe { info.si_pid() } == 0 {
            return Ok(None);
        }
        let status = unsafe { info.si_status() };
        let raw = match info.si_code {
            libc::CLD_EXITED => status << 8,
            libc::CLD_KILLED => status,
            libc::CLD_DUMPED => status | 0x80,
            _ => return Err(io::Error::other("unexpected pidfd wait status")),
        };
        Ok(Some(ExitStatus::from_raw(raw)))
    }
}

#[derive(Clone, Copy)]
enum State {
    Pending,
    Reaped(ExitStatus),
    ExternallyReaped,
}

pub(super) struct Child<'launch, Ops: Operations = Kernel> {
    native: std::process::Child,
    pidfd: Option<PidFd>,
    launch: Option<&'launch mut dyn owned::LaunchLifetime>,
    observer: RunObserver,
    handle: LogHandle,
    state: State,
    readiness: Option<AsyncFd<OwnedFd>>,
    pub(super) stdout: Option<tokio::process::ChildStdout>,
    pub(super) stderr: Option<tokio::process::ChildStderr>,
    operations: Ops,
}

impl<'launch, Ops: Operations> Child<'launch, Ops> {
    pub(super) fn spawn(
        command: std::process::Command,
        launch: &'launch mut dyn owned::LaunchLifetime,
        observer: RunObserver,
        handle: LogHandle,
        operations: Ops,
    ) -> io::Result<Self> {
        let (native, pidfd, cleanup_error) = spawn::owned(command, launch)?;
        let child = Self {
            native,
            pidfd: Some(pidfd),
            launch: Some(launch),
            observer,
            handle,
            state: State::Pending,
            readiness: None,
            stdout: None,
            stderr: None,
            operations,
        };
        child.observer.spawned(Some(child.native.id()));
        if let Some(error) = cleanup_error {
            child.handle.issue(IssueKind::Cleanup, &error);
            return Err(error);
        }
        Ok(child)
    }

    pub(super) fn adapt(&mut self) -> io::Result<()> {
        self.pidfd()?;
        self.stdout = self
            .native
            .stdout
            .take()
            .map(tokio::process::ChildStdout::from_std)
            .transpose()?;
        self.stderr = self
            .native
            .stderr
            .take()
            .map(tokio::process::ChildStderr::from_std)
            .transpose()?;
        self.readiness = Some(AsyncFd::new(self.pidfd()?.as_fd().try_clone_to_owned()?)?);
        Ok(())
    }

    pub(super) fn pidfd(&self) -> io::Result<&PidFd> {
        self.pidfd
            .as_ref()
            .ok_or_else(|| io::Error::new(io::ErrorKind::Unsupported, "child has no owned pidfd"))
    }

    fn poll_status(&mut self, blocking: bool) -> io::Result<Option<ExitStatus>> {
        match self.state {
            State::Reaped(status) => return Ok(Some(status)),
            State::ExternallyReaped => return Err(io::Error::from_raw_os_error(libc::ECHILD)),
            State::Pending => {}
        }
        let result = loop {
            let pidfd = self.pidfd.as_ref().ok_or_else(|| {
                io::Error::new(io::ErrorKind::Unsupported, "child has no owned pidfd")
            })?;
            let result = self.operations.wait(pidfd, blocking);
            if result
                .as_ref()
                .is_err_and(|error| error.kind() == io::ErrorKind::Interrupted)
            {
                continue;
            }
            break result;
        };
        match &result {
            Ok(Some(status)) => {
                self.state = State::Reaped(*status);
                self.observer.reaped(*status);
            }
            Err(error) if error.raw_os_error() == Some(libc::ECHILD) => {
                self.state = State::ExternallyReaped;
                self.observer.wait_failed(error);
                self.observer.reaped_without_status();
            }
            _ => return result,
        }
        if let Some(launch) = self.launch.as_deref_mut() {
            launch.reaped();
        }
        self.handle.root_reaped();
        result
    }

    pub(super) fn start_kill(&mut self) -> io::Result<()> {
        if !matches!(self.state, State::Pending) {
            return Ok(());
        }
        loop {
            let pidfd = self.pidfd.as_ref().ok_or_else(|| {
                io::Error::new(io::ErrorKind::Unsupported, "child has no owned pidfd")
            })?;
            let result = self.operations.signal(pidfd);
            if result
                .as_ref()
                .is_err_and(|error| error.kind() == io::ErrorKind::Interrupted)
            {
                continue;
            }
            return result;
        }
    }

    pub(super) async fn wait(&mut self) -> io::Result<ExitStatus> {
        self.native.stdin.take();
        loop {
            if let Some(status) = self.poll_status(false)? {
                return Ok(status);
            }
            self.readiness
                .as_ref()
                .ok_or_else(|| io::Error::other("child readiness not registered"))?
                .readable()
                .await?
                .clear_ready();
        }
    }
}

impl<Ops: Operations> Drop for Child<'_, Ops> {
    fn drop(&mut self) {
        if !matches!(self.state, State::Pending) {
            return;
        }
        if let Err(error) = self.start_kill() {
            self.handle.issue(IssueKind::Cleanup, error);
        }
        self.native.stdin.take();
        match self.poll_status(true) {
            Ok(Some(_)) => {}
            Err(error) if error.raw_os_error() == Some(libc::ECHILD) => {}
            result => self.handle.issue(
                IssueKind::Cleanup,
                format!("native child reap unconfirmed: {result:?}"),
            ),
        }
    }
}

#[cfg(test)]
mod tests;
