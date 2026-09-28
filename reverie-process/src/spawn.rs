/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

use std::io;
use std::io::Read;
use std::io::Write;
use std::os::fd::AsRawFd;

use syscalls::Errno;

use super::Child;
use super::Command;
use super::ControllerLaunch;
use super::ControllerLaunchId;
use super::ControllerSpawnError;
use super::ControllerSpawnFailure;
use super::ControllerSpawnToken;
use super::ControllerStartupPublisher;
use super::PendingControllerLaunch;
use super::clone::child_stack;
use super::clone::clone;
use super::clone::clone_with_stack_owned;
use super::container::ChildContext;
use super::controller_launch::ControllerStartupRecord;
use super::controller_launch::decode_startup_record;
use super::controller_launch::startup_record_len;
use super::error::Context;
use super::error::Error;
use super::fd::Fd;
use super::fd::pipe;
use super::id_map::make_id_map;
use super::seccomp::SeccompNotif;
use super::stdio::ChildStderr;
use super::stdio::ChildStdin;
use super::stdio::ChildStdout;
use super::util::CStringArray;
use super::util::SharedValue;

impl Command {
    /// Executes the command as a child process, returning a handle to it.
    ///
    /// By default, stdin, stdout and stderr are inherited from the parent.
    pub fn spawn(&mut self) -> Result<Child, Error> {
        // Create a pipe to send back errors to the parent process if `execve`
        // fails.
        let (reader, mut writer) = pipe()?;

        let child = self.spawn_with(|err| {
            send_error(&mut writer, err);
            1
        })?;

        // Close the writer end. Otherwise, the following read will hang
        // forever.
        drop(writer);

        recv_error(reader)?;

        Ok(child)
    }

    /// Spawns the single controller-owned root with an atomic clone-time pidfd
    /// and an exact child-published controller-ownership milestone.
    ///
    /// This deliberately does not return [`Child`] and is not a reversible
    /// command option: ordinary spawn, wait, and signal semantics remain
    /// unchanged. Any error after clone retains exact pidfd ownership in
    /// [`ControllerSpawnError::AfterClone`]. `controller_init` runs after all
    /// ordinary pre-exec callbacks and immediately before seccomp installation;
    /// it must publish READY immediately after establishing controller ownership
    /// and before entering its initial stop.
    ///
    /// The supported kernel contract is upstream Linux 5.4 or newer. A vendor
    /// kernel which implements all probed pidfd operations while silently
    /// ignoring legacy `clone(CLONE_PIDFD)` is contained behind the pre-exec
    /// gate and reported as an unsupported-kernel contract violation.
    ///
    /// # Safety
    ///
    /// `controller_init` runs in the child after a clone with fork-like memory
    /// semantics and before `execve`. It must use only operations that are safe
    /// in that context: in particular, it must not allocate or deallocate,
    /// acquire locks that another parent thread may hold, unwind, or violate
    /// invariants of duplicated file descriptors or memory mappings. Captured
    /// state must remain valid in the child's copy of the address space. The
    /// closure must establish controller ownership before publishing READY,
    /// publish READY immediately before its initial stop, and return `Ok(())`
    /// only after that stop has been resumed by the controller.
    pub unsafe fn spawn_controller_with<F>(
        &mut self,
        mut controller_init: F,
    ) -> Result<ControllerLaunch, ControllerSpawnError>
    where
        F: FnMut(&mut ControllerStartupPublisher) -> Result<(), Errno>,
    {
        let before_clone =
            |error, context| ControllerSpawnError::BeforeClone(Error::new(error, context));
        if self.container.seccomp_notify {
            return Err(before_clone(Errno::EINVAL, Context::Seccomp));
        }
        let launch = ControllerLaunchId::allocate().map_err(ControllerSpawnError::BeforeClone)?;
        let env = self.container.env.array();

        let (stdin, child_stdin) = self
            .container
            .stdin
            .pipes(true)
            .map_err(|error| before_clone(error, Context::Stdio))?;
        let (stdout, child_stdout) = self
            .container
            .stdout
            .pipes(false)
            .map_err(|error| before_clone(error, Context::Stdio))?;
        let (stderr, child_stderr) = self
            .container
            .stderr
            .pipes(false)
            .map_err(|error| before_clone(error, Context::Stdio))?;
        let stdin = stdin
            .map(ChildStdin::new)
            .transpose()
            .map_err(|error| before_clone(error, Context::Stdio))?;
        let stdout = stdout
            .map(ChildStdout::new)
            .transpose()
            .map_err(|error| before_clone(error, Context::Stdio))?;
        let stderr = stderr
            .map(ChildStderr::new)
            .transpose()
            .map_err(|error| before_clone(error, Context::Stdio))?;
        let (startup_reader, startup_writer_unreserved) =
            pipe().map_err(|error| before_clone(error, Context::Stdio))?;
        // Container setup may replace descriptors 0, 1, and 2. Reserve the
        // fixed startup protocol writer outside that set before clone rather
        // than inferring or closing any descriptor-number range in the child.
        let startup_writer_fd = Errno::result(unsafe {
            libc::fcntl(
                startup_writer_unreserved.as_raw_fd(),
                libc::F_DUPFD_CLOEXEC,
                3,
            )
        })
        .map_err(|error| before_clone(error, Context::Stdio))?;
        drop(startup_writer_unreserved);
        let startup_writer = Fd::new(startup_writer_fd);
        let (mut gate_reader, gate_writer) =
            pipe().map_err(|error| before_clone(error, Context::Stdio))?;
        let (mut liveness_reader, liveness_writer) =
            pipe().map_err(|error| before_clone(error, Context::Stdio))?;
        let gate_writer_fd = gate_writer.as_raw_fd();
        let liveness_reader_fd = liveness_reader.as_raw_fd();
        let liveness_writer_fd = liveness_writer.as_raw_fd();
        let startup_writer_fd = startup_writer.as_raw_fd();

        let mut stack = child_stack();
        let namespaces = self.container.namespace;
        let uid_map = &make_id_map(&self.container.uid_map);
        let gid_map = &make_id_map(&self.container.gid_map);
        let context = ChildContext {
            stdin: child_stdin.as_ref(),
            stdout: child_stdout.as_ref(),
            stderr: child_stderr.as_ref(),
            uid_map,
            gid_map,
            seccomp_fd: None,
        };
        let child_main = || {
            let mut publisher = ControllerStartupPublisher::new(startup_writer_fd);
            if unsafe { libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL, 0, 0, 0) } != 0 {
                publisher.publish_error(Error::new(Errno::last(), Context::PreExec));
                return 1;
            }
            // The child inherited the write end through clone. Close that copy
            // before reading so a parent-side containment close produces EOF.
            let _ = unsafe { libc::close(gate_writer_fd) };
            // A PID-namespace init sees getppid()==0 while its real parent is
            // outside the namespace, so parent identity cannot be validated by
            // numeric PID. Instead, close the child's copy of the acknowledgment
            // reader and publish one byte only after PDEATHSIG is armed. If the
            // parent died before the prctl, no reader remains and this write
            // cannot succeed; if it dies afterward, PDEATHSIG terminates us.
            let _ = unsafe { libc::close(liveness_reader_fd) };
            let armed = [1u8; 1];
            let (armed_written, armed_error) = loop {
                let written =
                    unsafe { libc::write(liveness_writer_fd, armed.as_ptr().cast(), armed.len()) };
                let error = (written == -1).then(Errno::last);
                if error == Some(Errno::EINTR) {
                    continue;
                }
                break (written, error);
            };
            let _ = unsafe { libc::close(liveness_writer_fd) };
            if armed_written != 1 {
                publisher.publish_error(Error::new(
                    armed_error.unwrap_or(Errno::EIO),
                    Context::PreExec,
                ));
                return 1;
            }
            let mut release = [0u8; 1];
            loop {
                match gate_reader.read(&mut release) {
                    Ok(1) if release[0] == 1 => break,
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                    Ok(_) | Err(_) => {
                        publisher.close_without_record();
                        return 127;
                    }
                }
            }
            let error =
                self.do_exec_controller(&context, &env, &mut publisher, &mut controller_init);
            let code = 1;
            let _ = error;
            unsafe { libc::_exit(code) }
        };

        let owned = clone_with_stack_owned(child_main, namespaces, &mut stack)
            .map_err(|error| before_clone(error, Context::Clone))?;
        // The child is the only writer whose byte proves PDEATHSIG is armed.
        drop(liveness_writer);
        let pidfd = match owned.pidfd {
            Some(pidfd) => pidfd,
            None => {
                // The callback is still blocked on `gate_reader`. Closing the
                // writer makes that exact, unreaped direct child exit without
                // calling do_exec. Its numeric PID cannot be reused until this
                // synchronous containment wait reaps it; this is not authority
                // recovery and is the sole non-pidfd wait in this launch path.
                drop(gate_writer);
                drop(startup_writer);
                drop(child_stdin);
                drop(child_stdout);
                drop(child_stderr);
                reap_unsupported_gated_child(owned.pid);
                return Err(ControllerSpawnError::UnsupportedKernelContract(Error::new(
                    Errno::EOPNOTSUPP,
                    Context::Clone,
                )));
            }
        };
        // `OwnedClone` retains the exact clone-time pidfd across callback-box
        // teardown. Convert it immediately into the controller's linear token
        // before performing descriptor validation or releasing the child.
        let token = ControllerSpawnToken::new(launch, owned.pid, pidfd);
        if let Err(error) = token.validate_pidfd_cloexec() {
            let pending = PendingControllerLaunch::new(
                token,
                stdin,
                stdout,
                stderr,
                gate_writer,
                startup_reader,
            );
            drop(startup_writer);
            drop(child_stdin);
            drop(child_stdout);
            drop(child_stderr);
            drop(self.container.pty.take());
            return Err(ControllerSpawnError::AfterClone {
                source: ControllerSpawnFailure::PidfdValidation(error),
                authority: Box::new(pending),
            });
        }

        // No fallible operation, allocation, conversion, or child release
        // occurs between the validated exact authority and this fixed pending
        // owner.
        let mut pending =
            PendingControllerLaunch::new(token, stdin, stdout, stderr, gate_writer, startup_reader);

        drop(child_stdin);
        drop(child_stdout);
        drop(child_stderr);
        drop(self.container.pty.take());
        let mut armed = [0u8; 1];
        loop {
            match liveness_reader.read(&mut armed) {
                Ok(1) if armed[0] == 1 => break,
                Ok(bytes) => {
                    drop(startup_writer);
                    return Err(ControllerSpawnError::AfterClone {
                        source: ControllerSpawnFailure::LivenessHandshakeShortRead(bytes),
                        authority: Box::new(pending),
                    });
                }
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) => {
                    drop(startup_writer);
                    return Err(ControllerSpawnError::AfterClone {
                        source: ControllerSpawnFailure::LivenessHandshakeRead(error),
                        authority: Box::new(pending),
                    });
                }
            }
        }
        if let Err(error) = pending.release_gate() {
            drop(startup_writer);
            return Err(ControllerSpawnError::AfterClone {
                source: ControllerSpawnFailure::GateWrite(error),
                authority: Box::new(pending),
            });
        }
        drop(startup_writer);

        let mut startup_record = [0u8; 16];
        debug_assert_eq!(startup_record.len(), startup_record_len());
        match pending.startup_reader_mut().read(&mut startup_record) {
            Ok(bytes) if bytes == startup_record.len() => {
                match decode_startup_record(startup_record) {
                    Some(ControllerStartupRecord::Ready) => {
                        pending.mark_ready();
                        Ok(ControllerLaunch::from_pending(pending))
                    }
                    Some(ControllerStartupRecord::Error(error)) => {
                        Err(ControllerSpawnError::AfterClone {
                            source: ControllerSpawnFailure::ChildStartup(error),
                            authority: Box::new(pending),
                        })
                    }
                    None => Err(ControllerSpawnError::AfterClone {
                        source: ControllerSpawnFailure::StartupRecordMalformed,
                        authority: Box::new(pending),
                    }),
                }
            }
            Ok(bytes) => Err(ControllerSpawnError::AfterClone {
                source: ControllerSpawnFailure::StartupRecordShortRead(bytes),
                authority: Box::new(pending),
            }),
            Err(error) => Err(ControllerSpawnError::AfterClone {
                source: ControllerSpawnFailure::StartupRecordRead(error),
                authority: Box::new(pending),
            }),
        }
    }

    /// Spawn the child with helper functions. The `onfail` callback runs in the
    /// child process if an error occurs during execution of the process. The
    /// `wait` function can be used to wait for the child to fully start up and
    /// to transform it into another type.
    pub fn spawn_with<F>(&mut self, mut onfail: F) -> Result<Child, Error>
    where
        F: FnMut(Error) -> i32,
    {
        let env = self.container.env.array();

        // Set up IO pipes
        let (stdin, child_stdin) = self.container.stdin.pipes(true)?;
        let (stdout, child_stdout) = self.container.stdout.pipes(false)?;
        let (stderr, child_stderr) = self.container.stdout.pipes(false)?;

        let clone_flags = self.container.namespace.bits() | libc::SIGCHLD;

        let uid_map = &make_id_map(&self.container.uid_map);
        let gid_map = &make_id_map(&self.container.gid_map);

        let seccomp_fd = if self.container.seccomp_notify {
            Some(SharedValue::new(core::sync::atomic::AtomicI32::new(0))?)
        } else {
            None
        };

        let context = ChildContext {
            stdin: child_stdin.as_ref(),
            stdout: child_stdout.as_ref(),
            stderr: child_stderr.as_ref(),
            uid_map,
            gid_map,
            seccomp_fd: seccomp_fd.as_ref().map(|x| x.as_ref()),
        };

        let pid = clone(
            || {
                let code = onfail(self.do_exec(&context, &env));
                unsafe { libc::_exit(code) }
            },
            clone_flags,
        )?;

        drop(child_stdin);
        drop(child_stdout);
        drop(child_stderr);
        drop(self.container.pty.take());

        let seccomp_notif = match seccomp_fd {
            Some(shared_fd) => {
                use core::sync::atomic::Ordering;

                // Spin until the value changes in the child.
                let mut targetfd = 0;
                while targetfd == 0 {
                    targetfd = shared_fd.as_ref().load(Ordering::Relaxed);
                    std::thread::yield_now();
                }

                // Use pidfd_getfd to copy the file descriptor
                let pidfd = Fd::pidfd_open(pid.into(), 0)?;
                let fd = pidfd.pidfd_getfd(targetfd, 0)?;

                // We've successfully duplicated the file descriptor. Let the
                // child continue on to execve.
                shared_fd.as_ref().store(0, Ordering::Relaxed);

                Some(SeccompNotif::new(fd)?)
            }
            None => None,
        };

        let stdin = stdin.map(ChildStdin::new).transpose()?;
        let stdout = stdout.map(ChildStdout::new).transpose()?;
        let stderr = stderr.map(ChildStderr::new).transpose()?;

        Ok(Child {
            pid,
            exit_status: None,
            seccomp_notif,
            stdin,
            stdout,
            stderr,
        })
    }

    /// Note: This function MUST NOT allocate or deallocate any memory. Doing so
    /// can cause deadlocks.
    ///
    /// Only returns if an error occurs, thus it is only possible for it to
    /// return an error.
    fn do_exec(&mut self, context: &ChildContext, env: &CStringArray) -> Error {
        if let Err(err) = self.container.setup(context, &mut self.pre_exec) {
            return err;
        }

        Error::result(
            unsafe { libc::execvpe(self.program.as_ptr(), self.args.as_ptr(), env.as_ptr()) },
            Context::Exec,
        )
        .unwrap_err()
    }

    fn do_exec_controller<F>(
        &mut self,
        context: &ChildContext,
        env: &CStringArray,
        publisher: &mut ControllerStartupPublisher,
        controller_init: &mut F,
    ) -> Error
    where
        F: FnMut(&mut ControllerStartupPublisher) -> Result<(), Errno>,
    {
        if let Err(error) =
            self.container
                .setup_with_final_pre_seccomp(context, &mut self.pre_exec, || {
                    controller_init(publisher)?;
                    publisher.is_published().then_some(()).ok_or(Errno::EPROTO)
                })
        {
            publisher.publish_error(error);
            return error;
        }
        debug_assert!(publisher.is_published());
        Error::result(
            unsafe { libc::execvpe(self.program.as_ptr(), self.args.as_ptr(), env.as_ptr()) },
            Context::Exec,
        )
        .unwrap_err()
    }
}

fn reap_unsupported_gated_child(pid: super::Pid) {
    let mut status = 0;
    loop {
        match Errno::result(unsafe { libc::waitpid(pid.as_raw(), &mut status, 0) }) {
            Ok(reaped)
                if reaped == pid.as_raw()
                    && (libc::WIFEXITED(status) || libc::WIFSIGNALED(status)) =>
            {
                return;
            }
            Err(Errno::EINTR) => continue,
            Ok(_) | Err(_) => {
                // This is the impossible-contract containment path. Returning
                // could leak a live/zombie child with no pidfd authority, so
                // fail-stop rather than misreport a recoverable launch error.
                std::process::abort();
            }
        }
    }
}

/// Sends an error and closes the pipe. Ignore any errors if this fails.
pub fn send_error(fd: &mut Fd, err: Error) {
    // Writes up to PIPE_BUF (4096) should be atomic. There's also nothing we
    // can do with an error if this fails.
    let bytes: [u8; 8] = err.into();
    let _ = fd.write(&bytes);
}

/// Tries to receive an error code from the pipe. If the other end of the
/// pipe is closed before sending an error, then `Ok(())` is returned.
pub fn recv_error(mut fd: Fd) -> Result<(), Error> {
    use std::io::Read;
    let mut err = [0u8; 8];
    loop {
        match fd.read(&mut err) {
            Ok(0) => return Ok(()),
            Ok(8) => return Err(Error::from(err)),
            Ok(n) => {
                // Sends up to PIPE_BUF (4096) should be atomic.
                panic!("execve pipe: got unexpected number of bytes {}", n);
            }
            Err(err) if err.kind() == io::ErrorKind::Interrupted => {}
            Err(err) => {
                panic!("execve pipe: read returned unexpected error {}", err);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::os::fd::AsRawFd;
    use std::os::fd::OwnedFd;
    use std::time::Duration;
    use std::time::Instant;

    use super::*;
    use crate::ControllerLaunchParts;
    use crate::ControllerLaunchPhase;
    use crate::Namespace;
    use crate::Pid;

    struct ExactTestChild {
        pid: Pid,
        pidfd: OwnedFd,
        reaped: bool,
    }

    impl ExactTestChild {
        fn from_parts(parts: ControllerLaunchParts) -> (ControllerLaunchPhase, Self) {
            let ControllerLaunchParts {
                token,
                stdin,
                stdout,
                stderr,
                phase,
            } = parts;
            drop((stdin, stdout, stderr));
            let (_, pid, pidfd) = token.into_parts();
            (
                phase,
                Self {
                    pid,
                    pidfd,
                    reaped: false,
                },
            )
        }

        fn next_status(&mut self, deadline: Instant) -> Result<i32, String> {
            loop {
                let mut status = 0;
                match Errno::result(unsafe {
                    libc::waitpid(self.pid.as_raw(), &mut status, libc::WNOHANG)
                }) {
                    Ok(0) => {}
                    Ok(reaped) if reaped == self.pid.as_raw() => {
                        if libc::WIFEXITED(status) || libc::WIFSIGNALED(status) {
                            self.reaped = true;
                        }
                        return Ok(status);
                    }
                    Ok(other) => {
                        return Err(format!(
                            "waitpid returned unexpected child {other} for {}",
                            self.pid.as_raw()
                        ));
                    }
                    Err(Errno::EINTR) => continue,
                    Err(error) => return Err(format!("waitpid failed: {error}")),
                }
                if Instant::now() >= deadline {
                    return Err(format!(
                        "timed out waiting for controller child {}",
                        self.pid.as_raw()
                    ));
                }
                std::thread::sleep(Duration::from_millis(5));
            }
        }

        fn continue_tracee(&self) -> Result<(), Errno> {
            Errno::result(unsafe {
                libc::ptrace(
                    libc::PTRACE_CONT,
                    self.pid.as_raw(),
                    std::ptr::null_mut::<libc::c_void>(),
                    std::ptr::null_mut::<libc::c_void>(),
                )
            })
            .map(drop)
        }
    }

    impl Drop for ExactTestChild {
        fn drop(&mut self) {
            if self.reaped {
                return;
            }
            let signal = unsafe {
                libc::syscall(
                    libc::SYS_pidfd_send_signal,
                    self.pidfd.as_raw_fd(),
                    libc::SIGKILL,
                    std::ptr::null::<libc::siginfo_t>(),
                    0,
                )
            };
            if signal == -1 && Errno::last() != Errno::ESRCH {
                std::process::abort();
            }
            loop {
                let mut status = 0;
                match Errno::result(unsafe { libc::waitpid(self.pid.as_raw(), &mut status, 0) }) {
                    Ok(reaped)
                        if reaped == self.pid.as_raw()
                            && (libc::WIFEXITED(status) || libc::WIFSIGNALED(status)) =>
                    {
                        self.reaped = true;
                        return;
                    }
                    Err(Errno::EINTR) => continue,
                    Ok(_) | Err(_) => std::process::abort(),
                }
            }
        }
    }

    #[test]
    fn controller_ready_and_cleanup_work_in_user_pid_namespace() {
        let mut command = Command::new("/bin/sh");
        command
            .arg("-c")
            .arg("test \"$$\" -eq 1")
            .map_root()
            .unshare(Namespace::USER | Namespace::PID);
        // SAFETY: this child callback uses only raw syscalls and the fixed
        // allocation-free publisher. It publishes READY immediately after
        // TRACEME and before its initial stop, then clears PDEATHSIG only after
        // the parent has resumed that stop.
        let launched = unsafe {
            command.spawn_controller_with(|publisher| {
                Errno::result(libc::ptrace(
                    libc::PTRACE_TRACEME,
                    0,
                    std::ptr::null_mut::<libc::c_void>(),
                    std::ptr::null_mut::<libc::c_void>(),
                ))?;
                publisher.publish_ready()?;
                Errno::result(libc::kill(libc::getpid(), libc::SIGSTOP))?;
                Errno::result(libc::prctl(libc::PR_SET_PDEATHSIG, 0, 0, 0, 0))?;
                Ok(())
            })
        };

        let launch = match launched {
            Ok(launch) => launch,
            Err(ControllerSpawnError::BeforeClone(error))
                if error.errno() == Errno::EPERM && error.context() == Context::Clone =>
            {
                eprintln!("skipping USER|PID controller test: clone returned EPERM");
                return;
            }
            Err(ControllerSpawnError::AfterClone {
                source: ControllerSpawnFailure::ChildStartup(error),
                authority,
            }) if error.errno() == Errno::EPERM
                && matches!(error.context(), Context::MapUid | Context::MapGid) =>
            {
                let (_, mut child) = ExactTestChild::from_parts((*authority).into_parts());
                let status = child
                    .next_status(Instant::now() + Duration::from_secs(2))
                    .unwrap_or_else(|failure| panic!("namespace skip cleanup failed: {failure}"));
                assert!(libc::WIFEXITED(status));
                assert_eq!(libc::WEXITSTATUS(status), 1);
                eprintln!(
                    "skipping USER|PID controller test: {} returned EPERM",
                    error.context()
                );
                return;
            }
            Err(ControllerSpawnError::AfterClone { source, authority }) => {
                let (_, _child) = ExactTestChild::from_parts((*authority).into_parts());
                panic!("unexpected post-clone controller failure: {source}");
            }
            Err(error) => panic!("unexpected controller launch failure: {error}"),
        };

        let (phase, mut child) = ExactTestChild::from_parts(launch.into_parts());
        assert_eq!(phase, ControllerLaunchPhase::TracerOwnershipReady);
        let deadline = Instant::now() + Duration::from_secs(3);
        let initial = child
            .next_status(deadline)
            .unwrap_or_else(|failure| panic!("initial controller stop failed: {failure}"));
        assert!(libc::WIFSTOPPED(initial));
        assert_eq!(libc::WSTOPSIG(initial), libc::SIGSTOP);
        child
            .continue_tracee()
            .unwrap_or_else(|error| panic!("continue initial controller stop: {error}"));
        let exec = child
            .next_status(deadline)
            .unwrap_or_else(|failure| panic!("controller exec stop failed: {failure}"));
        assert!(libc::WIFSTOPPED(exec));
        assert_eq!(libc::WSTOPSIG(exec), libc::SIGTRAP);
        child
            .continue_tracee()
            .unwrap_or_else(|error| panic!("continue controller exec stop: {error}"));
        let exited = child
            .next_status(deadline)
            .unwrap_or_else(|failure| panic!("controller completion failed: {failure}"));
        assert!(libc::WIFEXITED(exited));
        assert_eq!(libc::WEXITSTATUS(exited), 0);
    }
}
