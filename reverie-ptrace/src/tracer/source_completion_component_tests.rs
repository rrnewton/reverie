/* Copyright (c) Meta Platforms, Inc. and affiliates. All rights reserved.
 * Licensed under the BSD-style license in the root LICENSE file. */

//! Host completion components; command admission/tree delivery are MODELED.
//! Status comes from an actual owned self-exec child, never a constructed
//! successful terminal. No ptrace, source capture or FollowedHold is exercised.
use std::future::Future;
use std::io::Read as _;
use std::os::unix::process::CommandExt as _;
use std::task::Poll;

use super::*;
use crate::task::source_jobs::current_registry_tests::BYTES;
use crate::task::source_jobs::current_registry_tests::HeldSourceJob;
use crate::task::source_jobs::current_registry_tests::Snapshot;

const CHILD: &str = "tracer::source_completion_component_tests::owned_command_child";
const ROLE: &str = "REVERIE_SOURCE_COMPLETION_CHILD";
const OUT_FD: &str = "REVERIE_SOURCE_COMPLETION_OUT_FD";
const ERR_FD: &str = "REVERIE_SOURCE_COMPLETION_ERR_FD";
const OUT: &[u8] = b"owned-source-command-out\0\xff\n";
const ERR: &[u8] = b"owned-source-command-err\0\xfe\n";

fn owned_pipe() -> std::io::Result<(OwnedFd, OwnedFd)> {
    let mut pair = [-1; 2];
    if unsafe { libc::pipe2(pair.as_mut_ptr(), libc::O_CLOEXEC) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(unsafe { (OwnedFd::from_raw_fd(pair[0]), OwnedFd::from_raw_fd(pair[1])) })
}

struct OwnedCommand {
    child: Option<std::process::Child>,
    gate: Option<std::process::ChildStdin>,
    status: Option<std::process::ExitStatus>,
    pid: Pid,
}

impl OwnedCommand {
    fn spawn() -> std::io::Result<(Self, OwnedFd, OwnedFd)> {
        let (out_read, out_write) = owned_pipe()?;
        let (err_read, err_write) = owned_pipe()?;
        let out_fd = out_write.as_raw_fd();
        let err_fd = err_write.as_raw_fd();
        let mut command = std::process::Command::new(std::env::current_exe()?);
        command
            .args([
                "--exact",
                CHILD,
                "--ignored",
                "--nocapture",
                "--test-threads=1",
            ])
            .env(ROLE, "owned-child-v1")
            .env(OUT_FD, out_fd.to_string())
            .env(ERR_FD, err_fd.to_string())
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        // Of these four pipe FDs, only the writers cross exec. Read ends retain
        // CLOEXEC. The helper installs them as stdout/stderr after libtest's
        // prelude went to /dev/null, so no framework text is filtered out of
        // the production CaptureDrain or its exact-byte oracle.
        unsafe {
            command.pre_exec(move || {
                for fd in [out_fd, err_fd] {
                    if libc::fcntl(fd, libc::F_SETFD, 0) != 0 {
                        return Err(std::io::Error::last_os_error());
                    }
                }
                Ok(())
            });
        }
        let mut child = command.spawn()?;
        let pid = Pid::from_raw(child.id() as i32);
        let gate = child.stdin.take();
        drop(out_write);
        drop(err_write);
        Ok((
            Self {
                child: Some(child),
                gate,
                status: None,
                pid,
            },
            out_read,
            err_read,
        ))
    }

    fn release(&mut self) -> Result<(), String> {
        if let Some(mut gate) = self.gate.take() {
            gate.write_all(b"R")
                .map_err(|e| format!("original child gate: {e}"))?;
        }
        Ok(())
    }

    async fn wait(&mut self, deadline: Instant) -> Result<ExitStatus, String> {
        while self.status.is_none() {
            if Instant::now() >= deadline {
                return Err("original component deadline before child reap".into());
            }
            self.status = self
                .child
                .as_mut()
                .expect("original Child owner")
                .try_wait()
                .map_err(|e| format!("original child wait: {e}"))?;
            if self.status.is_none() {
                tokio::task::yield_now().await;
            }
        }
        if Instant::now() >= deadline {
            return Err("original child result after component deadline".into());
        }
        Ok(self.status.expect("actual Child wait result").into())
    }
}

impl Drop for OwnedCommand {
    fn drop(&mut self) {
        // This can release a blocked helper on failure, but proves no reap.
        drop(self.gate.take());
        if self.status.is_none()
            && let Some(child) = self.child.take()
        {
            // Retain original ownership for the enclosing failed test
            // process's bounded group cleanup. Never signal by a reread PID.
            let _ = Box::leak(Box::new(child));
        }
    }
}

#[test]
#[ignore = "internal self-exec helper; selected only by the owning host component"]
fn owned_command_child() {
    assert_eq!(std::env::var(ROLE).as_deref(), Ok("owned-child-v1"));
    let out: i32 = std::env::var(OUT_FD).unwrap().parse().unwrap();
    let err: i32 = std::env::var(ERR_FD).unwrap().parse().unwrap();
    assert!(out > 2 && err > 2 && out != err);
    let mut gate = [0];
    if std::io::stdin().read_exact(&mut gate).is_err() || gate != *b"R" {
        unsafe {
            libc::_exit(78);
        }
    }
    unsafe {
        if libc::dup2(out, libc::STDOUT_FILENO) < 0 || libc::dup2(err, libc::STDERR_FILENO) < 0 {
            libc::_exit(79);
        }
        libc::close(out);
        libc::close(err);
        if libc::write(libc::STDOUT_FILENO, OUT.as_ptr().cast(), OUT.len()) != OUT.len() as isize
            || libc::write(libc::STDERR_FILENO, ERR.as_ptr().cast(), ERR.len())
                != ERR.len() as isize
        {
            libc::_exit(80);
        }
    }
    // A real normal process exit, observed by its original Child. Do not flush
    // buffered libtest prelude text or run a footer into the two exact pipes.
    unsafe {
        libc::_exit(0);
    }
}

fn driver(
    pid: Pid,
    stdout: OwnedFd,
    stderr: OwnedFd,
) -> (
    CompletionDriver<(), Output>,
    Arc<FatalSession>,
    tokio::sync::oneshot::Sender<ExitStatus>,
) {
    let global = Arc::new(());
    let session = Arc::new(FatalSession::source_completion_component(&global, pid));
    let (send, receive) = tokio::sync::oneshot::channel();
    let tracer = Tracer {
        guest_pid: pid,
        tracer: Box::pin(async move {
            receive
                .await
                .map_err(|e| Error::from(anyhow::anyhow!("owned command status channel: {e}")))
        }),
        gref: global,
        stdin: None,
        stdout: Some(ChildStdout::from(stdout)),
        stderr: Some(ChildStderr::from(stderr)),
        liteinst_cleanup: None,
        liteinst_instrumentation_stats: None,
        backend_stats: None,
        ordinary_session: session.clone(),
        ptracer_thread: std::thread::current().id(),
        ordinary_completion_supported: true, // Explicit modeled admission.
    };
    (
        tracer.completion(1, |status, stdout, stderr| Output {
            status,
            stdout,
            stderr,
        }),
        session,
        send,
    )
}

async fn poll_round(driver: &mut CompletionDriver<(), Output>) -> Poll<bool> {
    let work = &mut driver.work;
    driver
        .local
        .run_until(future::poll_fn(|cx| {
            let round = work.round();
            futures::pin_mut!(round);
            Poll::Ready(round.poll(cx))
        }))
        .await
}

async fn deliver_command(
    command: &mut OwnedCommand,
    sender: &mut Option<tokio::sync::oneshot::Sender<ExitStatus>>,
    deadline: Instant,
) -> Result<ExitStatus, String> {
    command.release()?;
    let actual = command.wait(deadline).await?;
    if let Some(sender) = sender.take() {
        sender
            .send(actual)
            .map_err(|_| "ordinary owner lost actual child status".to_string())?;
    }
    Ok(actual)
}

async fn drain_command(
    driver: &mut CompletionDriver<(), Output>,
    deadline: Instant,
) -> Result<(), String> {
    while !driver.work.tree_done
        || !driver.work.stdout.is_finished()
        || !driver.work.stderr.is_finished()
    {
        if Instant::now() >= deadline {
            return Err("original deadline before command/pipe completion".into());
        }
        let _ = poll_round(driver).await;
        tokio::task::yield_now().await;
    }
    if Instant::now() >= deadline {
        return Err("command pipes completed after original deadline".into());
    }
    Ok(())
}

struct Before {
    pending: bool,
    source: Snapshot,
    actual_child_delivered: bool,
    stdout_eof: bool,
    stderr_eof: bool,
}

async fn completion_case(source_first: bool, drop_observer: bool, deadline: Instant) {
    assert!(
        Instant::now() < deadline,
        "no new child after original component deadline"
    );
    let (mut command, stdout, stderr) = OwnedCommand::spawn().expect("owned self-exec command");
    let (mut driver, session, sender) = driver(command.pid, stdout, stderr);
    let mut sender = Some(sender);
    let mut held = HeldSourceJob::start(&session.source_jobs, false);
    let before: Result<Before, String> = async {
        held.wait_tls(deadline).await?;
        if drop_observer {
            held.drop_observer();
        }
        if source_first {
            held.finish(&session.source_jobs, deadline).await?;
        } else {
            deliver_command(&mut command, &mut sender, deadline).await?;
            drain_command(&mut driver, deadline).await?;
        }
        let pending = poll_round(&mut driver).await.is_pending();
        Ok(Before {
            pending,
            source: held.snapshot(&session.source_jobs, false),
            actual_child_delivered: driver.work.tree_done,
            stdout_eof: driver.work.stdout.is_finished(),
            stderr_eof: driver.work.stderr.is_finished(),
        })
    }
    .await;

    // Always close the same original resources before asserting snapshots,
    // including when a production-predicate mutant returned Ready too soon.
    let source_cleanup = held.finish(&session.source_jobs, deadline).await;
    let command_cleanup = deliver_command(&mut command, &mut sender, deadline).await;
    let actual_result = held.take_result();
    let completion =
        tokio::time::timeout_at(tokio::time::Instant::from_std(deadline), driver.drive()).await;
    let closed_before_deadline = Instant::now() < deadline;
    assert!(
        source_cleanup.is_ok(),
        "original source cleanup: {source_cleanup:?}"
    );
    let actual_status = command_cleanup.expect("original command cleanup");
    assert!(
        command.status.is_some(),
        "actual original Child was not reaped"
    );
    let outcome = completion.expect("original deadline before ordinary completion");
    let ToolRunOutcome::Complete(completed) = outcome else {
        panic!("ordinary completion retained unresolved cleanup");
    };
    let output = completed
        .result
        .expect("actual command/source component failed");
    assert!(
        closed_before_deadline,
        "completion crossed the original component deadline"
    );
    assert_eq!(
        output.status, actual_status,
        "must preserve the real Child result"
    );
    assert!(
        actual_status.success(),
        "self-exec command failed: {actual_status:?}"
    );
    assert_eq!(output.stdout, OUT);
    assert_eq!(output.stderr, ERR);
    assert_eq!(session.source_jobs.pending_jobs(), 0);
    if drop_observer {
        assert!(
            matches!(
                actual_result,
                Some(Err(reverie::syscalls::NativeUserReadError::Refused(
                    reverie::syscalls::NativeUserReadRefusal::TargetState(
                        safeptrace::Errno::ECANCELED
                    )
                )))
            ),
            "dropped observer published successful or different bytes: {actual_result:?}"
        );
    } else {
        assert_eq!(actual_result, Some(Ok(BYTES.to_vec())));
    }
    let before = before.expect("real causal boundary setup");
    assert!(
        before.pending,
        "ordinary completion passed an unmet original owner boundary"
    );
    if source_first {
        assert!(before.source.complete && before.source.jobs == 0 && before.source.drops == 1);
        assert!(!before.actual_child_delivered);
    } else {
        assert!(before.actual_child_delivered && before.stdout_eof && before.stderr_eof);
        assert!(!before.source.complete && before.source.jobs == 1 && before.source.drops == 0);
        assert!(!before.source.has_result);
    }
}

#[tokio::test(flavor = "current_thread")]
async fn ordinary_completion_waits_for_owned_command_status_and_source_join() {
    let deadline = Instant::now() + Duration::from_secs(3);
    completion_case(true, false, deadline).await;
    completion_case(false, false, deadline).await;
}

#[tokio::test(flavor = "current_thread")]
async fn ordinary_completion_waits_after_observer_drop() {
    let deadline = Instant::now() + Duration::from_secs(3);
    completion_case(false, true, deadline).await;
}
