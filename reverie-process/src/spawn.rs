/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

use std::io;
use std::io::Write;

use super::Child;
use super::Command;
use super::clone::clone;
use super::command_start::ParentContinuation;
use super::container::ChildContext;
use super::error::Context;
use super::error::Error;
use super::fd::Fd;
use super::fd::pipe;
use super::id_map::make_id_map;
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

    /// Spawn the child with helper functions. The `onfail` callback runs in the
    /// child process if an error occurs during execution of the process. The
    /// `wait` function can be used to wait for the child to fully start up and
    /// to transform it into another type.
    pub fn spawn_with<F>(&mut self, mut onfail: F) -> Result<Child, Error>
    where
        F: FnMut(Error) -> i32,
    {
        let mut parent = None;
        self.spawn_parent_into(&mut parent, &mut onfail)?;
        parent
            .expect("successful clone filled the parent slot")
            .finish_legacy()
    }

    // The caller owns this slot before birth. There is no user callback,
    // allocation, fallible conversion or await between clone and filling it.
    pub(super) fn spawn_parent_into<F>(
        &mut self,
        parent: &mut Option<ParentContinuation>,
        onfail: &mut F,
    ) -> Result<(), Error>
    where
        F: FnMut(Error) -> i32,
    {
        assert!(parent.is_none());
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

        *parent = Some(ParentContinuation {
            child: Some(Child {
                pid,
                exit_status: None,
                seccomp_notif: None,
                stdin: None,
                stdout: None,
                stderr: None,
            }),
            stdin,
            stdout,
            stderr,
            child_ends: [child_stdin, child_stdout, child_stderr],
            pty: self.container.pty.take(),
            seccomp_fd,
            seccomp_pidfd: None,
            seccomp_copy: None,
        });
        Ok(())
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
