/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! `Tracer` type, plus ways to spawn it and retrieve its output.

use std::collections::HashMap;
use std::collections::HashSet;
use std::collections::VecDeque;
use std::fs;
use std::io::Write;
use std::net::SocketAddr;
use std::os::fd::AsRawFd;
use std::os::fd::BorrowedFd;
use std::os::fd::FromRawFd;
use std::os::fd::OwnedFd;
use std::os::unix::fs::MetadataExt;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex as StdMutex;
use std::thread::ThreadId;
use std::time::Duration;
use std::time::Instant;

use anyhow::Context;
use close_err::Closable;
use futures::future;
use futures::future::BoxFuture;
use futures::future::Either;
use futures::stream::StreamExt;
use nix::sys::ptrace;
use nix::sys::signal;
use nix::sys::signal::Signal;
use nix::unistd;
use nix::unistd::ForkResult;
use reverie::Errno;
use reverie::Error;
use reverie::ExitStatus;
use reverie::GlobalTool;
use reverie::Pid;
use reverie::Subscription;
use reverie::Tool;
use reverie::process::ChildStderr;
use reverie::process::ChildStdin;
use reverie::process::ChildStdout;
use reverie::process::Command;
use reverie::process::Output;
use reverie::process::seccomp;
use reverie::syscalls::Sysno;
use safeptrace::Error as TraceError;
use safeptrace::Event;
use safeptrace::Running;
use safeptrace::Stopped;
use safeptrace::TerminalCleanup;
use tokio::sync::broadcast;
use tokio::sync::mpsc;

use crate::cp;
use crate::gdbstub::GdbServer;
use crate::task::Child;
use crate::task::InjectedSyscallProvenance;
use crate::task::InjectedSyscallTrap;
use crate::task::LiteinstRuntimeConfig;
use crate::task::TracedTask;
use crate::task::TracedTaskOptions;

/// Represents the tracer.
///
/// We need to simultaneously capture stderr/stdout while handling events. These
/// can be two separate futures. The stderr/stdout future will finish when the
/// pipes are closed.
///
/// The stderr/stdout capture can be a `Stream<Item = Either<Bytes, Bytes>>`
/// where each item is either a chunk of stderr bytes or stdout bytes. Zipping
/// together the two streams like this preserves ordering.
pub struct Tracer<G> {
    /// PID of the root guest process.
    guest_pid: Pid,

    // Future of the running handler.
    tracer: BoxFuture<'static, Result<ExitStatus, Error>>,

    // A reference to the global state.
    gref: Arc<G>,

    stdin: Option<ChildStdin>,
    stdout: Option<ChildStdout>,
    stderr: Option<ChildStderr>,

    // Present only for the single-process dynamic LiteInst host. Ordinary
    // ptrace and e9patch lifecycles retain their existing teardown behavior.
    liteinst_cleanup: Option<LiteinstTraceeCleanup>,
}

struct LiteinstTraceeCleanup {
    identity: ProcessIdentity,
    newborn_tracees: Arc<StdMutex<HashSet<Pid>>>,
    armed: bool,
    terminal: Option<TerminalCleanup>,
    notifier_owner: Option<ThreadId>,
}

struct ProcessIdentity {
    pid: Pid,
    pidfd: OwnedFd,
    start_time: u64,
}

impl ProcessIdentity {
    fn open(pid: Pid) -> Result<Self, Errno> {
        let start_time = process_start_time(pid)
            .map_err(|error| Errno::new(error.raw_os_error().unwrap_or(libc::EIO)))?;
        let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid.as_raw(), 0) };
        if fd == -1 {
            return Err(Errno::last());
        }
        let identity = Self {
            pid,
            pidfd: unsafe { OwnedFd::from_raw_fd(fd as i32) },
            start_time,
        };
        if !identity.matches_proc() {
            return Err(Errno::ESRCH);
        }
        Ok(identity)
    }

    fn send_signal(&self, signal: Signal) -> Result<(), Errno> {
        self.send_raw_signal(signal as i32)
    }

    fn is_alive(&self) -> bool {
        self.send_raw_signal(0).is_ok()
    }

    fn matches_proc(&self) -> bool {
        process_start_time(self.pid).ok() == Some(self.start_time)
    }

    fn send_raw_signal(&self, signal: i32) -> Result<(), Errno> {
        let result = unsafe {
            libc::syscall(
                libc::SYS_pidfd_send_signal,
                self.pidfd.as_raw_fd(),
                signal,
                std::ptr::null::<libc::siginfo_t>(),
                0,
            )
        };
        if result == -1 {
            Err(Errno::last())
        } else {
            Ok(())
        }
    }
}

struct RegisteredTraceeCleanup {
    identity: ProcessIdentity,
    terminal: TerminalCleanup,
}

impl LiteinstTraceeCleanup {
    fn new(pid: Pid, newborn_tracees: Arc<StdMutex<HashSet<Pid>>>) -> Result<Self, Errno> {
        Ok(Self {
            identity: ProcessIdentity::open(pid)?,
            newborn_tracees,
            armed: true,
            terminal: None,
            notifier_owner: None,
        })
    }

    fn pid(&self) -> Pid {
        self.identity.pid
    }

    fn register_notifier(&mut self) {
        debug_assert!(self.terminal.is_none());
        self.notifier_owner = Some(std::thread::current().id());
        self.terminal = Some(Running::new(self.pid()).terminal_cleanup());
    }

    fn disarm(&mut self) {
        self.armed = false;
    }

    fn confirm_reaped(&mut self) -> std::io::Result<()> {
        if !self.armed {
            return Ok(());
        }
        let notifier_finished = self
            .terminal
            .as_ref()
            .is_some_and(|terminal| terminal.wait(Duration::ZERO));
        let identity_absent = !self.identity.matches_proc();
        let unregistered_absent = self.terminal.is_none() && identity_absent;
        if (notifier_finished && identity_absent) || unregistered_absent {
            self.armed = false;
            Ok(())
        } else {
            Err(std::io::Error::new(
                std::io::ErrorKind::WouldBlock,
                "LiteInst root still exists after typed task cleanup",
            ))
        }
    }

    fn terminate_and_confirm(&mut self) -> std::io::Result<()> {
        if self.confirm_reaped().is_ok() {
            return Ok(());
        }

        if self.terminal.is_none() {
            terminate_and_reap_new_child_with_identity(Running::new(self.pid()), &self.identity)
                .map_err(|error| {
                    std::io::Error::other(format!("pre-registration LiteInst cleanup: {error}"))
                })?;
            self.armed = false;
            return Ok(());
        }

        let mut descendants = HashMap::new();
        let mut terminal_descendants = HashMap::new();
        self.discover_descendants(&mut descendants, &terminal_descendants)?;
        match self.identity.send_signal(Signal::SIGKILL) {
            Ok(()) | Err(Errno::ESRCH) => {}
            Err(error) => return Err(std::io::Error::from_raw_os_error(error.into_raw())),
        }
        for tracee in descendants.values() {
            send_identity_sigkill(&tracee.identity)?;
        }

        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            self.discover_descendants(&mut descendants, &terminal_descendants)?;
            for tracee in descendants.values() {
                send_identity_sigkill(&tracee.identity)?;
            }

            let root_done = self
                .terminal
                .as_ref()
                .is_some_and(|terminal| terminal.wait(Duration::ZERO));
            let completed = descendants
                .iter()
                .filter_map(|(pid, tracee)| tracee.terminal.wait(Duration::ZERO).then_some(*pid))
                .collect::<Vec<_>>();
            for pid in completed {
                let tracee = descendants
                    .remove(&pid)
                    .expect("completed descendant must remain registered");
                if tracee.identity.matches_proc() {
                    terminal_descendants.insert(pid, tracee.identity);
                }
            }
            terminal_descendants.retain(|_, identity| identity.matches_proc());
            let root_absent = !self.identity.matches_proc();
            if root_done && root_absent && descendants.is_empty() && terminal_descendants.is_empty()
            {
                self.armed = false;
                return Ok(());
            }

            if self.notifier_owner == Some(std::thread::current().id()) {
                // The pidfd-bound SIGKILL is already pending. Numeric ptrace
                // operations only advance an extant ptrace relationship and
                // never inject a signal into a potentially reused PID.
                // Preserve parentage until every descendant is terminal and
                // reaped. Otherwise an auto-attached child can be reparented
                // before its notifier consumes the final wait status.
                if !root_done && descendants.is_empty() && self.identity.is_alive() {
                    let _ = ptrace::cont(self.pid().into(), None);
                }
                for tracee in descendants.values() {
                    if tracee.identity.is_alive() {
                        let _ = ptrace::cont(tracee.identity.pid.into(), None);
                    }
                }
            }
            if let Some(terminal) = self.terminal.as_ref() {
                terminal.wait(Duration::from_millis(1));
            }
        }

        Err(std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            format!(
                "notifier did not acknowledge terminal cleanup for LiteInst tracee {}",
                self.pid()
            ),
        ))
    }

    fn discover_descendants(
        &self,
        descendants: &mut HashMap<Pid, RegisteredTraceeCleanup>,
        terminal_descendants: &HashMap<Pid, ProcessIdentity>,
    ) -> std::io::Result<()> {
        let mut queue = VecDeque::from([self.pid()]);
        queue.extend(descendants.keys().copied());
        let newborn_tracees = self
            .newborn_tracees
            .lock()
            .unwrap()
            .iter()
            .copied()
            .collect::<Vec<_>>();
        for child in newborn_tracees {
            queue.push_back(child);
            if child == self.pid()
                || descendants.contains_key(&child)
                || terminal_descendants.contains_key(&child)
            {
                continue;
            }
            let Ok(identity) = ProcessIdentity::open(child) else {
                continue;
            };
            let terminal = Running::new(child).terminal_cleanup();
            descendants.insert(child, RegisteredTraceeCleanup { identity, terminal });
        }
        let mut visited = HashSet::new();
        while let Some(parent) = queue.pop_front() {
            if !visited.insert(parent) {
                continue;
            }
            for child in direct_children(parent)? {
                queue.push_back(child);
                if child == self.pid()
                    || descendants.contains_key(&child)
                    || terminal_descendants.contains_key(&child)
                {
                    continue;
                }
                let Ok(identity) = ProcessIdentity::open(child) else {
                    continue;
                };
                let terminal = Running::new(child).terminal_cleanup();
                descendants.insert(child, RegisteredTraceeCleanup { identity, terminal });
            }
        }
        Ok(())
    }
}

fn send_identity_sigkill(identity: &ProcessIdentity) -> std::io::Result<()> {
    match identity.send_signal(Signal::SIGKILL) {
        Ok(()) | Err(Errno::ESRCH) => Ok(()),
        Err(error) => Err(std::io::Error::from_raw_os_error(error.into_raw())),
    }
}

impl Drop for LiteinstTraceeCleanup {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        // Cancellation cannot await an orderly drain, so synchronously request
        // termination and wait for the notifier-owned final reap. Before async
        // registration, the bounded raw-wait fallback owns cleanup instead.
        if let Err(error) = self.terminate_and_confirm() {
            tracing::error!(pid = %self.pid(), %error, "LiteInst cancellation cleanup failed");
        }
    }
}

fn process_start_time(pid: Pid) -> std::io::Result<u64> {
    let stat = fs::read_to_string(format!("/proc/{pid}/stat"))?;
    let fields = stat
        .rsplit_once(") ")
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidData, "malformed stat"))?
        .1;
    fields
        .split_ascii_whitespace()
        .nth(19)
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidData, "missing starttime"))?
        .parse()
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))
}

fn direct_children(pid: Pid) -> std::io::Result<Vec<Pid>> {
    let task_dir = match fs::read_dir(format!("/proc/{pid}/task")) {
        Ok(task_dir) => task_dir,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error),
    };
    let mut children = Vec::new();
    for task in task_dir {
        let task = task?;
        let contents = match fs::read_to_string(task.path().join("children")) {
            Ok(contents) => contents,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error),
        };
        children.extend(
            contents
                .split_ascii_whitespace()
                .filter_map(|pid| pid.parse::<i32>().ok())
                .map(Pid::from_raw),
        );
    }
    Ok(children)
}

pub(crate) fn terminate_and_reap_new_child(task: Running) -> Result<(), TraceError> {
    let pid = task.pid();
    let identity = ProcessIdentity::open(pid)?;
    terminate_and_reap_new_child_with_identity(task, &identity)
}

fn terminate_and_reap_new_child_with_identity(
    task: Running,
    identity: &ProcessIdentity,
) -> Result<(), TraceError> {
    match identity.send_signal(Signal::SIGKILL) {
        Ok(()) | Err(Errno::ESRCH) => {}
        Err(error) => return Err(error.into()),
    }
    drain_unregistered_child(task)
}

fn drain_unregistered_child(task: Running) -> Result<(), TraceError> {
    let pid = task.pid();
    for _ in 0..2_000 {
        let mut status = 0;
        let waited =
            unsafe { libc::waitpid(pid.as_raw(), &mut status, libc::__WALL | libc::WNOHANG) };
        if waited == 0 {
            std::thread::sleep(std::time::Duration::from_millis(1));
            continue;
        }
        if waited == -1 {
            let errno = Errno::last();
            match errno {
                Errno::EINTR => continue,
                Errno::ECHILD
                    if unsafe { libc::kill(pid.as_raw(), 0) } == -1
                        && Errno::last() == Errno::ESRCH =>
                {
                    return Ok(());
                }
                Errno::ECHILD => {
                    std::thread::sleep(std::time::Duration::from_millis(1));
                    continue;
                }
                _ => return Err(errno.into()),
            }
        }
        if libc::WIFEXITED(status) || libc::WIFSIGNALED(status) {
            return Ok(());
        }
        if libc::WIFSTOPPED(status) {
            let stopped = Stopped::new_unchecked(pid);
            match stopped.resume(None) {
                Ok(_) | Err(TraceError::Died(_)) | Err(TraceError::Errno(Errno::ESRCH)) => {}
                Err(error) => return Err(error),
            }
        }
    }
    Err(Errno::ETIMEDOUT.into())
}

impl<G: Default> Tracer<G> {
    /// Returns the PID of the root guest process.
    pub fn guest_pid(&self) -> Pid {
        self.guest_pid
    }

    /// Simultaneously waits for the tracee to exit and collect all remaining
    /// output on the stdout/stderr handles, returning an `Output` instance.
    ///
    /// The stdin handle to the child process, if any, will be closed before
    /// waiting. This helps avoid deadlock: it ensures that the child does not
    /// block waiting for input from the parent, while the parent waits for the
    /// child to exit.
    ///
    /// By default, stdin, stdout and stderr are inherited from the parent. In
    /// order to capture the output it is necessary to create new pipes between
    /// parent and child. Use `stdout(Stdio::piped())` or
    /// `stderr(Stdio::piped())`, respectively.
    pub async fn wait_with_output(mut self) -> Result<(Output, G), Error> {
        use tokio::io::AsyncRead;
        use tokio::io::AsyncReadExt;

        async fn read_to_end<A: AsyncRead + Unpin>(io: Option<A>) -> Result<Vec<u8>, Error> {
            let mut vec = Vec::new();
            if let Some(mut io) = io {
                io.read_to_end(&mut vec).await?;
            }
            Ok(vec)
        }

        drop(self.stdin.take());

        let stdout = read_to_end(self.stdout.take());
        let stderr = read_to_end(self.stderr.take());

        let ((status, state), stdout, stderr) =
            future::try_join3(self.wait(), stdout, stderr).await?;

        Ok((
            Output {
                status,
                stdout,
                stderr,
            },
            state,
        ))
    }

    /// Waits for the tracee to exit and returns its exit status and global
    /// state.
    pub async fn wait(mut self) -> Result<(ExitStatus, G), Error> {
        // Note: The usage of LocalSet is *very* important here. Once polled,
        // the `tracer` future drives all tracees to completion. The `fork` for
        // the root tracee and all subsequent ptrace operations *MUST* be done
        // on the same thread. Thus, we use `LocalSet` in combination with
        // `tokio::task::spawn_local` to ensure that everything happens on the
        // same thread. Otherwise, ptrace operations will start returning
        // `ESRCH` errors and they will be (incorrectly) interpretted to mean
        // that the tracee has died unexpectedly.
        let local_set = tokio::task::LocalSet::new();
        let exit_status = match local_set.run_until(self.tracer).await {
            Ok(status) => {
                if let Some(cleanup) = self.liteinst_cleanup.as_mut() {
                    cleanup.disarm();
                }
                status
            }
            Err(error) => {
                if let Some(cleanup) = self.liteinst_cleanup.as_mut()
                    && let Err(cleanup_error) = cleanup.terminate_and_confirm()
                {
                    return Err(anyhow::anyhow!(
                        "LiteInst tracee cleanup failed after {error}: {cleanup_error}"
                    )
                    .into());
                }
                return Err(error);
            }
        };

        let g = Arc::try_unwrap(self.gref).unwrap_or_else(|_| {
            panic!("Reverie internal invariant broken. Arc::try_unwrap on global state failed.")
        });

        Ok((exit_status, g))
    }
}

fn from_nix_error(err: nix::Error) -> Errno {
    Errno::new(err as i32)
}

async fn initialization_error(pid: Pid, err: TraceError) -> Error {
    match err {
        TraceError::Errno(errno) => {
            anyhow::anyhow!("failed to initialize ptrace for tracee {pid}: {errno}").into()
        }
        TraceError::Died(zombie) => {
            let exit_status = zombie.reap().await;
            tracing::error!(
                target: "reverie_ptrace::lifecycle",
                %pid,
                ?exit_status,
                "guest exited during ptrace initialization"
            );
            anyhow::anyhow!("tracee {pid} exited during ptrace initialization with {exit_status:?}")
                .into()
        }
    }
}

fn report_pre_exec_capability_error(message: &'static [u8]) -> Errno {
    let errno = Errno::last();
    // SAFETY: write is async-signal-safe and message has static storage. This
    // runs after fork, where tracing and allocation are not safe.
    let _ = unsafe { libc::write(libc::STDERR_FILENO, message.as_ptr().cast(), message.len()) };
    errno
}

/// Sets up the child process for ptracing right before execve is called.
fn init_tracee(intercept_rdtsc: bool) -> Result<(), Errno> {
    // NOTE: There should be *NO* allocations along the happy path here.
    // Allocating between a fork() and execve() can cause deadlocks in glibc
    // when using jemalloc.

    // hardcoded because `libc` does not export these.
    const PER_LINUX: u64 = 0x0;
    const ADDR_NO_RANDOMIZE: u64 = 0x0004_0000;

    if intercept_rdtsc {
        // Intercepting rdtsc is only possible on x86
        #[cfg(target_arch = "x86_64")]
        unsafe {
            if libc::prctl(libc::PR_SET_TSC, libc::PR_TSC_SIGSEGV, 0, 0, 0) != 0 {
                return Err(report_pre_exec_capability_error(
                    b"ERROR: Reverie could not enable RDTSC interception with prctl(PR_SET_TSC)\n",
                ));
            }
        };
    }

    unsafe {
        if libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0 {
            return Err(report_pre_exec_capability_error(
                b"ERROR: Reverie could not enable PR_SET_NO_NEW_PRIVS for seccomp interception\n",
            ));
        }
        if libc::personality(PER_LINUX | ADDR_NO_RANDOMIZE) == -1 {
            return Err(report_pre_exec_capability_error(
                b"ERROR: Reverie could not disable address-space randomization with personality(2)\n",
            ));
        }
    }

    // FIXME: This is a hacky workaround for `std::process::Command::spawn`
    // getting stuck in a deadlock because of the SIGSTOP below.
    // `Command::spawn` uses a pipe to communicate the error code to the parent
    // process if the `execve` fails. The idea is that the write end of the pipe
    // will be closed upon a successful call to `execve` and the parent will
    // abort the blocking read on the read end of the pipe. We don't know
    // exactly which file descriptor the pipe uses, so we attempt to close the
    // first N file descriptors hoping it is among those. Unfortunately, in
    // doing so, we lose the ability to capture `execve` failures.
    //
    // There are a couple options for a better implementation:
    //  1. Recreate the entire `std::process` module to provide better ptrace
    //     support. (A lot of work!)
    //  2. Don't raise a SIGSTOP, but instead let the ptracer stop on the call to
    //     `execve` and have the parent set the ptrace options at that point.
    for i in 3..256 {
        unsafe {
            libc::close(i);
        }
    }

    safeptrace::traceme_and_stop()?;

    unsafe {
        signal::sigaction(
            signal::SIGTTIN,
            &signal::SigAction::new(
                signal::SigHandler::SigIgn,
                signal::SaFlags::SA_RESTART,
                signal::SigSet::empty(),
            ),
        )
        .map_err(from_nix_error)?;

        signal::sigaction(
            signal::SIGTTOU,
            &signal::SigAction::new(
                signal::SigHandler::SigIgn,
                signal::SaFlags::SA_RESTART,
                signal::SigSet::empty(),
            ),
        )
        .map_err(from_nix_error)?;
    }

    Ok(())
}

async fn run_orphaned(orphans: mpsc::Receiver<Child>) {
    tokio_stream::wrappers::ReceiverStream::new(orphans)
        .for_each_concurrent(None, |orphan| async {
            let pid = orphan.id();
            let Some(mut daemonizer) = orphan.daemonizer_rx else {
                tracing::error!(
                    %pid,
                    "orphan is missing its daemonization channel; waiting for exit"
                );
                let status = orphan.handle.await;
                tracing::debug!(%pid, ?status, "orphan exited");
                return;
            };

            let daemonizer = daemonizer.recv();
            futures::pin_mut!(daemonizer);

            match future::select(Box::pin(orphan.handle), daemonizer).await {
                Either::Left((exit_status, _)) => {
                    tracing::debug!(
                        "[reverie] Orphan {} exited with status {:?}",
                        pid,
                        exit_status
                    );
                }
                Either::Right((kill_switch, handle)) => {
                    tracing::debug!("[reverie] pid {} daemonized", pid);
                    if let Some(mut kill_switch) = kill_switch {
                        let kill_switch = kill_switch.recv();
                        futures::pin_mut!(kill_switch);
                        match future::select(Box::pin(handle), kill_switch).await {
                            Either::Left((exit_status, _)) => {
                                tracing::debug!(
                                    "[reverie] Daemon {} exited with status {:?}",
                                    pid,
                                    exit_status
                                );
                            }
                            Either::Right((_, handle)) => {
                                tracing::debug!("sending sigkill {}", pid);
                                unsafe {
                                    libc::kill(pid.as_raw(), libc::SIGKILL);
                                }
                                let status = handle.await;
                                tracing::debug!(
                                    "[reverie] Daemon {} exited with status {:?}",
                                    pid,
                                    status
                                );
                            }
                        }
                    }
                }
            }
        })
        .await;
}

/// Runs the task tree to completion and returns the exit status of the root
/// task.
async fn run_task_tree<T: Tool + 'static>(
    root: TracedTask<T>,
    child: Stopped,
    orphanage: mpsc::Receiver<Child>,
) -> Result<ExitStatus, Error> {
    future::join(
        // Run the root task to completion
        root.run(child),
        // ...and wait for all orphans simultaneously.
        run_orphaned(orphanage),
    )
    .await
    .0
}

/// Helper function for everything after the child is spawned.
#[tracing::instrument(
    target = "reverie_ptrace::lifecycle",
    name = "tracee.attach",
    level = "debug",
    skip_all,
    fields(pid = %child.pid())
)]
async fn postspawn<L: Tool + 'static>(
    child: Running,
    gref: Arc<L::GlobalState>,
    config: <L::GlobalState as GlobalTool>::Config,
    events: &Subscription,
    injected_syscall_trap: Option<InjectedSyscallTrap>,
    liteinst_runtime: Option<LiteinstRuntimeConfig>,
    gdbserver: Option<GdbServer>,
) -> Result<BoxFuture<'static, Result<ExitStatus, Error>>, TraceError> {
    let pid = child.pid();

    // Wait for the child to enter a stopped state. The child will enter a
    // stopped state immediately after ptrace::traceme is called.
    //
    // NOTE: We may rarely get spurious signals here, like SIGWINCH, so we must
    // skip past them.
    let (mut child, event) = child
        .wait_for_signal(Signal::SIGSTOP)
        .await?
        .assume_stopped();
    assert_eq!(event, Event::Signal(Signal::SIGSTOP));

    child.setoptions(
        ptrace::Options::PTRACE_O_TRACEEXEC
            | ptrace::Options::PTRACE_O_EXITKILL
            | ptrace::Options::PTRACE_O_TRACECLONE
            | ptrace::Options::PTRACE_O_TRACEFORK
            | ptrace::Options::PTRACE_O_TRACEVFORK
            | ptrace::Options::PTRACE_O_TRACEVFORKDONE
            | ptrace::Options::PTRACE_O_TRACEEXIT
            | ptrace::Options::PTRACE_O_TRACESECCOMP
            | ptrace::Options::PTRACE_O_TRACESYSGOOD,
    )?;

    let (orphan_sender, orphan_receiver) = mpsc::channel(1);
    let (daemon_kill, _) = broadcast::channel(1);

    // This is the root task, so there's no reason to make run its init routine
    // asynchronously, as there isn't any other work to do.
    let mut tracer = TracedTask::<L>::new(
        pid,
        config,
        gref,
        TracedTaskOptions {
            events,
            injected_syscall_trap,
            liteinst_runtime,
        },
        orphan_sender,
        daemon_kill,
        gdbserver,
    );

    child = tracer.tracee_preinit(child).await?;

    let tracer = Box::pin(run_task_tree(tracer, child, orphan_receiver));
    Ok(tracer)
}

/// Creates the seccomp filter. This lets us control which syscalls are traced
/// and which ones are allowed through.
fn seccomp_filter(events: &Subscription) -> seccomp::Filter {
    use reverie::process::seccomp::Action;

    seccomp::FilterBuilder::new()
        // By default, all syscalls are allowed through untraced. Then, we can
        // intercept only the syscalls we are interested in.
        .default_action(Action::Allow)
        .syscalls(
            events
                .iter_syscalls()
                .map(|syscall| (syscall, Action::Trace(0))),
        )
        // Always allow these syscalls to pass through untraced.
        .syscall(Sysno::restart_syscall, Action::Allow)
        .syscall(Sysno::rt_sigreturn, Action::Allow)
        // Allow untraced syscalls through without tracing them.
        .ip_range(
            (cp::TRAMPOLINE_BASE + cp::SYSCALL_INSTR_SIZE) as u64,
            (cp::TRAMPOLINE_BASE + cp::SYSCALL_INSTR_SIZE + cp::UD_INSTR_SIZE) as u64,
            Action::Allow,
        )
        .build()
}

/// Specifies *how* the GDB server should listen for incoming connections.
pub enum GdbConnection {
    /// The server shall bind to and listen on the given socket address.
    Addr(SocketAddr),

    /// The server shall bind to and listen on the given unix domain socket. This
    /// path must not exist, otherwise the bind will fail with `EADDRINUSE`.
    Path(PathBuf),
}

impl From<SocketAddr> for GdbConnection {
    fn from(addr: SocketAddr) -> Self {
        Self::Addr(addr)
    }
}

impl From<PathBuf> for GdbConnection {
    fn from(path: PathBuf) -> Self {
        Self::Path(path)
    }
}

impl From<u16> for GdbConnection {
    fn from(port: u16) -> Self {
        Self::Addr(([127, 0, 0, 1], port).into())
    }
}

/// A builder for creating a tracer.
pub struct TracerBuilder<T: Tool + 'static> {
    /// The program to execute that will be traced.
    command: Command,

    /// The global state static config.
    config: Option<<T::GlobalState as GlobalTool>::Config>,

    /// Set to `Some` if we should spawn a GDB server.
    gdbserver: Option<GdbConnection>,

    /// Indicates that the guest's scheduling will be serialized by the Reverie
    /// tool. This is only relevant for the GDB server.
    sequentialized_guest: bool,

    /// Marker and exact RIP identifying an injected syscall trap, when enabled.
    injected_syscall_trap: Option<InjectedSyscallTrap>,

    /// Dynamic LiteInst runtime handshake and hot-site configuration.
    liteinst_runtime: Option<LiteinstRuntimeConfig>,
}

impl<T: Tool + 'static> TracerBuilder<T> {
    /// Creates the builder with the given command.
    pub fn new(command: Command) -> Self {
        Self {
            command,
            config: None,
            gdbserver: None,
            sequentialized_guest: false,
            injected_syscall_trap: None,
            liteinst_runtime: None,
        }
    }

    /// Returns a reference to the command to be traced.
    pub fn command(&self) -> &Command {
        &self.command
    }

    /// Sets the static configuration that will be made available to the tool.
    pub fn config(mut self, config: <T::GlobalState as GlobalTool>::Config) -> Self {
        self.config = Some(config);
        self
    }

    /// Configures the tracer to create a GDB server and listen for incoming
    /// connections. The tracer will start in a stopped state and will not
    /// proceed until a connection is made. This allows the GDB client to observe
    /// the full execution of the guest.
    pub fn gdbserver<C: Into<GdbConnection>>(mut self, connection: C) -> Self {
        self.gdbserver = Some(connection.into());
        self
    }

    /// Make the GDB server aware that guest threads are sequentialized. This is
    /// needed when the Reverie tool has full control of scheduling and already
    /// sequentializes thread execution. This helps avoid deadlocks.
    pub fn sequentialized_guest(mut self) -> Self {
        self.sequentialized_guest = true;
        self
    }

    /// Routes matching `SIGTRAP` stops through `Tool::handle_syscall_event`.
    ///
    /// A binary rewriter must place `marker` in RAX, an e9tool-compatible
    /// writable `state` frame pointer in RDI, and execute `int3` at `rip - 1`.
    /// All other traps retain their normal signal/debugger semantics.
    // TODO-HUMAN-REVIEW(PR-103): Review the injected syscall event provenance API.
    pub fn injected_syscall_trap(mut self, marker: u64, rip: u64) -> Self {
        self.injected_syscall_trap = Some(InjectedSyscallTrap {
            marker,
            rip,
            provenance: None,
        });
        self
    }

    /// Enables the dynamic LiteInst runtime handshake and injected hot-site path.
    ///
    /// The preload path validates handshake instruction pointers against the
    /// expected executable mapping. Distinct markers, exact return sites, and
    /// mapping generations reject accidental collisions; they are not a
    /// security boundary against arbitrary code already running in the tracee.
    /// Dynamic mode currently fails closed if the tracee forks or adds a thread.
    // TODO-HUMAN-REVIEW(PR-270): Review dynamic LiteInst provenance API.
    pub fn liteinst_runtime(
        mut self,
        preload: impl Into<PathBuf>,
        begin_marker: u64,
        ready_marker: u64,
        helper_return_marker: u64,
        syscall_marker: u64,
    ) -> Self {
        self.liteinst_runtime = Some(LiteinstRuntimeConfig {
            preload: preload.into(),
            begin_marker,
            ready_marker,
            helper_return_marker,
            syscall_marker,
            newborn_tracees: Arc::new(StdMutex::new(HashSet::new())),
            #[cfg(test)]
            fail_preinit: false,
            #[cfg(test)]
            pause_new_task: None,
        });
        self
    }

    #[cfg(test)]
    fn fail_liteinst_preinit_for_test(mut self) -> Self {
        self.liteinst_runtime
            .as_mut()
            .expect("LiteInst runtime must be configured before preinit failure injection")
            .fail_preinit = true;
        self
    }

    #[cfg(test)]
    fn pause_liteinst_new_task_for_test(mut self, sender: mpsc::UnboundedSender<Pid>) -> Self {
        self.liteinst_runtime
            .as_mut()
            .expect("LiteInst runtime must be configured before child-event pause")
            .pause_new_task = Some(sender);
        self
    }

    /// Filters a binary-rewriter trap unless its logical instruction address
    /// names an ahead-of-time patched site in the configured executable's
    /// canonical pathname/inode identity.
    ///
    /// This rejects accidental marker/frame collisions; it is not a security
    /// boundary against guest code that deliberately forges a real site.
    // AUTONOMOUS-BOT-IMPLEMENTED
    // TODO-HUMAN-REVIEW(PR-271): Review site-validated binary-rewriter trap API.
    pub fn site_validated_injected_syscall_trap(
        mut self,
        marker: u64,
        rip: u64,
        image: impl Into<PathBuf>,
        image_entry_address: u64,
        patched_site_addresses: impl IntoIterator<Item = u64>,
    ) -> Result<Self, Error> {
        let image = std::fs::canonicalize(image.into())?;
        let image_metadata = std::fs::metadata(&image)?;
        let mut patched_site_addresses = patched_site_addresses.into_iter().collect::<Vec<_>>();
        patched_site_addresses.sort_unstable();
        patched_site_addresses.dedup();
        if patched_site_addresses.is_empty() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "site-validated injected-syscall traps require at least one patched site",
            )
            .into());
        }
        self.injected_syscall_trap = Some(InjectedSyscallTrap {
            marker,
            rip,
            provenance: Some(InjectedSyscallProvenance {
                image,
                image_inode: image_metadata.ino(),
                image_entry_address,
                patched_site_addresses: patched_site_addresses.into(),
            }),
        });
        Ok(self)
    }

    /// Spawns the tracer.
    pub async fn spawn(self) -> Result<Tracer<T::GlobalState>, Error> {
        let mut command = self.command;
        let config = self.config.unwrap_or_default();
        let liteinst_fail_closed = self.liteinst_runtime.is_some();

        // Because this ptrace backend is CENTRALIZED, it can keep all the
        // tool's state here in a single address space.
        let global_state = <T::GlobalState as GlobalTool>::init_global_state(&config).await;
        let events = T::subscriptions(&config);
        let mut traced_events = events.clone();
        if self.liteinst_runtime.is_some() {
            // Mapping operations are controller-only lifecycle observations:
            // trace them so successful VMA churn can invalidate patched-site
            // provenance, without adding them to the Tool's subscription set.
            traced_events.syscalls([
                Sysno::mmap,
                Sysno::munmap,
                Sysno::mremap,
                Sysno::mprotect,
                Sysno::pkey_mprotect,
            ]);
        }
        let gref = Arc::new(global_state);

        // Get the full path to the program and change the command to use it. This
        // also checks that the path exists and provides an early exit just in case
        // it doesn't.
        //
        // Normally, we'd rely upon the `exit(1)` following a failed call to
        // `execve`, but that is tricky when ptracing the `execve` call.
        resolve_program(&mut command)?;

        // Disable sanitizers that use ptrace from running on tracer.
        command.env("LSAN_OPTIONS", "detect_leaks=0");
        command.env("ASAN_OPTIONS", "detect_leaks=0");

        let intercept_rdtsc = events.has_rdtsc();
        unsafe {
            command.pre_exec(move || init_tracee(intercept_rdtsc));
        }

        command.seccomp(seccomp_filter(&traced_events));

        let mut child = command.spawn().context("Failed to spawn tracee")?;
        let guest_pid = child.id();
        let running_child = Running::new(guest_pid);
        let liteinst_newborn_tracees = self
            .liteinst_runtime
            .as_ref()
            .map(|runtime| Arc::clone(&runtime.newborn_tracees));
        let mut liteinst_cleanup = if liteinst_fail_closed {
            match LiteinstTraceeCleanup::new(
                guest_pid,
                liteinst_newborn_tracees.expect("LiteInst runtime config must exist"),
            ) {
                Ok(cleanup) => Some(cleanup),
                Err(error) => {
                    // pidfd is a required LiteInst cleanup capability. The
                    // just-spawned, unreaped PID cannot have been reused yet,
                    // so a one-time numeric kill is safe only on this setup
                    // failure path; all active guards signal through pidfd.
                    unsafe {
                        libc::kill(guest_pid.as_raw(), libc::SIGKILL);
                    }
                    let _ = drain_unregistered_child(Running::new(guest_pid));
                    return Err(anyhow::anyhow!(
                        "failed to open pidfd for LiteInst tracee {guest_pid}: {error}"
                    )
                    .into());
                }
            }
        } else {
            None
        };

        // Configure the gdb server (if any).
        let gdbserver = match self.gdbserver {
            None => None,
            Some(connection) => {
                let server = match connection {
                    GdbConnection::Addr(addr) => GdbServer::from_addr(addr).await,
                    GdbConnection::Path(path) => GdbServer::from_path(&path).await,
                };

                let mut server = server.with_context(|| {
                    format!("failed to start GDB server for tracee {guest_pid}")
                })?;

                if self.sequentialized_guest {
                    server.sequentialized_guest();
                }

                Some(server)
            }
        };

        // From this point on, every wait status belongs to safeptrace's
        // notifier. Cancellation and initialization errors must request
        // termination through the guard and await notifier unregistration;
        // they must never call raw waitpid for this PID.
        if let Some(cleanup) = liteinst_cleanup.as_mut() {
            cleanup.register_notifier();
        }

        let tracer = match postspawn::<T>(
            running_child,
            gref.clone(),
            config,
            &events,
            self.injected_syscall_trap,
            self.liteinst_runtime,
            gdbserver,
        )
        .await
        {
            Ok(tracer) => tracer,
            Err(err) => {
                let error = initialization_error(guest_pid, err).await;
                if let Some(cleanup) = liteinst_cleanup.as_mut()
                    && let Err(cleanup_error) = cleanup.terminate_and_confirm()
                {
                    return Err(anyhow::anyhow!(
                        "LiteInst tracee cleanup failed after {error}: {cleanup_error}"
                    )
                    .into());
                }
                return Err(error);
            }
        };

        let stdin = child.stdin.take();
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();

        // Don't let the drop logic run for the child. Tokio will add the child to a
        // "orphan queue" that will try to call `waitpid` on the process when a
        // `SIGCHLD` signal is received. This interferes with our own process
        // handling where we need full control over the lifetime of the child
        // process.
        core::mem::forget(child);

        Ok(Tracer {
            guest_pid,
            tracer,
            gref,
            stdin,
            stdout,
            stderr,
            liteinst_cleanup,
        })
    }
}

fn resolve_program(command: &mut Command) -> Result<(), Error> {
    let arg0 = command.get_arg0().to_owned();
    let program = command
        .find_program()
        .with_context(|| format!("Could not execute {:?}", command.get_program()))?;
    command.program(program).arg0(arg0);
    Ok(())
}

/// Spawn a *function* to be executed under instrumentation instrumentation
/// (rather than a subprocess indicated with a Command).
///
/// This still creates a fresh child process and runs it under ptrace. However,
/// the child process is a fork of the current process, and is used to run the
/// indicated function.
pub async fn spawn_fn<L, F>(fun: F) -> Result<Tracer<L::GlobalState>, Error>
where
    L: Tool + 'static,
    F: FnOnce(),
{
    spawn_fn_with_config::<L, F>(fun, Default::default(), true).await
}

/// Spawn a function with instrumentation rather than a subprocess indicated with
/// a Command. This still creates a fresh child process and runs it under ptrace.
/// However, the child process is a fork of the current process, and is used to
/// run the indicated function.
///
/// The main use case for this entrypoint into the library is testing.
pub async fn spawn_fn_with_config<L, F>(
    fun: F,
    config: <L::GlobalState as GlobalTool>::Config,
    capture_output: bool,
) -> Result<Tracer<L::GlobalState>, Error>
where
    L: Tool + 'static,
    F: FnOnce(),
{
    // Because this ptrace backend is CENTRALIZED, it can keep all the
    // tool's state here in a single address space.
    let global_state = <L::GlobalState as GlobalTool>::init_global_state(&config).await;
    let events = L::subscriptions(&config);
    let gref = Arc::new(global_state);

    let seccomp_filter = seccomp_filter(&events);

    let (read1, write1) = unistd::pipe().map_err(from_nix_error)?;
    let (read2, write2) = unistd::pipe().map_err(from_nix_error)?;

    // Disable io redirection just before forking. We want the child process to
    // be able to call `println!()` and have that output go to stdout.
    //
    // See: https://github.com/rust-lang/rust/issues/35136
    let output_capture = std::io::set_output_capture(None);

    // Warning: fork is wildely unsafe in Rust because of runtime issues (printing,
    // panicking, etc).  We make a best-effort attempt to solve some of these issues.
    match unsafe { unistd::fork() }.expect("unistd::fork failed") {
        ForkResult::Child => {
            read1.close()?;
            read2.close()?;
            if capture_output {
                unistd::dup2_stdout(&write1).map_err(from_nix_error)?;
                unistd::dup2_stderr(&write2).map_err(from_nix_error)?;
                write1.close()?;
                write2.close()?;
            }

            init_tracee(events.has_rdtsc()).expect("init_tracee failed");

            seccomp_filter.load().expect("Failed to set seccomp filter");

            match std::panic::catch_unwind(std::panic::AssertUnwindSafe(fun)) {
                Ok(()) => {
                    std::io::stdout().flush()?;
                    std::process::exit(0);
                }
                Err(e) => {
                    std::io::stdout().flush()?;
                    let _ = nix::unistd::write(
                        unsafe { BorrowedFd::borrow_raw(2) },
                        format!("Forked Rust process panicked, cause: {:?}", e).as_ref(),
                    );
                    std::process::exit(1);
                }
            };
        }
        ForkResult::Parent { child } => {
            std::io::set_output_capture(output_capture);

            let guest_pid = Pid::from(child);
            let child = Running::new(guest_pid);
            write1.close()?;
            write2.close()?;

            let stdout = read1.into();
            let stderr = read2.into();
            let tracer = match postspawn::<L>(
                child,
                gref.clone(),
                config,
                &events,
                None,
                None,
                None,
            )
            .await
            {
                Ok(tracer) => tracer,
                Err(err) => return Err(initialization_error(guest_pid, err).await),
            };

            Ok(Tracer {
                guest_pid,
                tracer,
                gref,
                stdin: None,
                stdout: Some(stdout),
                stderr: Some(stderr),
                liteinst_cleanup: None,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fork_paused_child() -> Pid {
        match unsafe { unistd::fork() }.expect("fork test child") {
            ForkResult::Child => loop {
                unsafe { libc::pause() };
            },
            ForkResult::Parent { child } => Pid::from(child),
        }
    }

    fn assert_reaped(role: &str, pid: Pid) {
        assert!(
            !std::path::Path::new(&format!("/proc/{pid}")).exists(),
            "{role} tracee {pid} remains in procfs"
        );
        let mut status = 0;
        assert_eq!(
            unsafe { libc::waitpid(pid.as_raw(), &mut status, libc::WNOHANG) },
            -1
        );
        assert_eq!(Errno::last(), Errno::ECHILD);
    }

    #[derive(Default)]
    struct InitFailureTool;

    #[reverie::tool]
    impl Tool for InitFailureTool {
        type GlobalState = ();
        type ThreadState = ();

        fn subscriptions(_config: &()) -> Subscription {
            Subscription::none()
        }
    }

    #[test]
    fn resolving_program_preserves_explicit_arg0() {
        let mut command = Command::new("/bin/echo");
        command.arg0("chosen-name");
        resolve_program(&mut command).unwrap();
        assert_eq!(command.get_program(), "/bin/echo");
        assert_eq!(command.get_arg0(), "chosen-name");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn liteinst_preinit_failure_reaps_and_unregisters_root() {
        let error = match TracerBuilder::<InitFailureTool>::new(Command::new("/bin/true"))
            .liteinst_runtime(PathBuf::from("/not/used.so"), 1, 2, 3, 4)
            .fail_liteinst_preinit_for_test()
            .spawn()
            .await
        {
            Ok(_) => panic!("injected LiteInst preinit failure unexpectedly succeeded"),
            Err(error) => error,
        };
        let message = error.to_string();
        let pid = message
            .split("tracee ")
            .nth(1)
            .and_then(|suffix| suffix.split(':').next())
            .and_then(|pid| pid.parse::<i32>().ok())
            .unwrap_or_else(|| panic!("preinit error omitted tracee PID: {message}"));

        assert!(
            !std::path::Path::new(&format!("/proc/{pid}")).exists(),
            "failed LiteInst preinit left tracee {pid} in procfs: {message}"
        );
        let mut status = 0;
        assert_eq!(
            unsafe { libc::waitpid(pid, &mut status, libc::WNOHANG) },
            -1
        );
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::ECHILD),
            "failed LiteInst preinit left tracee {pid} waitable"
        );
    }

    #[test]
    fn stale_pidfd_identity_never_signals_reused_numeric_pid() {
        let old_pid = fork_paused_child();
        let mut identity = ProcessIdentity::open(old_pid).expect("open old child pidfd");
        identity
            .send_signal(Signal::SIGKILL)
            .expect("kill old child");
        Running::new(old_pid).wait().expect("reap old child");

        let unrelated_pid = fork_paused_child();
        identity.pid = unrelated_pid;
        assert_eq!(identity.send_signal(Signal::SIGKILL), Err(Errno::ESRCH));
        assert_eq!(unsafe { libc::kill(unrelated_pid.as_raw(), 0) }, 0);

        unsafe { libc::kill(unrelated_pid.as_raw(), libc::SIGKILL) };
        Running::new(unrelated_pid)
            .wait()
            .expect("reap unrelated child");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn child_death_before_initialization_error_does_not_hang() {
        let pid = match unsafe { unistd::fork() }.expect("fork dying child") {
            ForkResult::Child => std::process::exit(42),
            ForkResult::Parent { child } => Pid::from(child),
        };
        assert!(matches!(
            Running::new(pid).next_state().await.unwrap(),
            safeptrace::Wait::Exited(_, ExitStatus::Exited(42))
        ));
        let died = Stopped::new_unchecked(pid)
            .resume(None)
            .expect_err("resuming a reaped child must report Died");
        assert!(matches!(died, TraceError::Died(_)));

        tokio::time::timeout(Duration::from_secs(1), initialization_error(pid, died))
            .await
            .expect("initialization_error hung reaping an already terminal child");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn cancelling_at_new_child_event_reaps_root_and_child() {
        let (child_tx, mut child_rx) = mpsc::unbounded_channel();
        let mut command = Command::new("/bin/sh");
        command.args(["-c", "sleep 60 & wait"]);
        let tracer = TracerBuilder::<InitFailureTool>::new(command)
            .liteinst_runtime(PathBuf::from("/not/used.so"), 1, 2, 3, 4)
            .pause_liteinst_new_task_for_test(child_tx)
            .spawn()
            .await
            .expect("spawn fork-cancellation tracee");
        let root_pid = tracer.guest_pid();
        let mut wait = Box::pin(tracer.wait());
        let child_pid = tokio::time::timeout(Duration::from_secs(3), async {
            tokio::select! {
                result = &mut wait => panic!("tracee completed before cancellation: {result:?}"),
                child = child_rx.recv() => child.expect("new-child hook closed"),
            }
        })
        .await
        .expect("tracee did not reach new-child cancellation window");

        drop(wait);
        assert_reaped("root", root_pid);
        assert_reaped("child", child_pid);
        for (role, pid) in [("root", root_pid), ("child", child_pid)] {
            assert_eq!(
                tokio::time::timeout(Duration::from_secs(1), Running::new(pid).next_state())
                    .await
                    .unwrap_or_else(|_| panic!("late {role} notifier wait hung")),
                Err(TraceError::Errno(Errno::ECHILD))
            );
        }
    }
}
