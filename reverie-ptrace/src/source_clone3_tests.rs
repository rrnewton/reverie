/* Copyright (c) Meta Platforms, Inc. and affiliates. All rights reserved.
 * Licensed under the BSD-style license in the root LICENSE file. */

//! Actual native clone3 outcomes; no fabricated Stop, identity or native receipt.
use std::cell::RefCell;
use std::os::fd::AsRawFd;
use std::os::fd::FromRawFd;
use std::os::fd::IntoRawFd;
use std::os::fd::OwnedFd;
use std::path::PathBuf;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;
use std::time::Duration;
use std::time::Instant;

use reverie::Errno;
use reverie::Error;
use reverie::ExitStatus;
use reverie::GlobalTool;
use reverie::Guest;
use reverie::Pid;
use reverie::Subscription;
use reverie::Tool;
use reverie::process::Command;
use reverie::syscalls::NativeUserReadError;
use reverie::syscalls::NativeUserReadRefusal;
use reverie::syscalls::Syscall;
use reverie::syscalls::SyscallInfo;

use super::*;
use crate::ToolRunOutcome;
use crate::TracerBuilder;

const PRE: usize = 791;
const POST: usize = 792;

#[derive(Default)]
struct NativeLog {
    member: Option<Member>,
    entries: Vec<(u64, bool, Result<(), safeptrace::Errno>)>,
    returns: Vec<(i64, bool, bool)>,
    child_events: usize,
}
thread_local! {
    static ACTIVE: RefCell<Option<Arc<Mutex<NativeLog>>>> = const { RefCell::new(None) };
}
struct Reset;
impl Drop for Reset {
    fn drop(&mut self) {
        ACTIVE.with(|s| {
            s.borrow_mut().take();
        });
    }
}
fn active() -> Option<Arc<Mutex<NativeLog>>> {
    ACTIVE.with(|s| s.borrow().clone())
}

pub(super) fn native(member: &Member, operation: u64, nr: Sysno) {
    if nr != Sysno::clone3 {
        return;
    }
    let Some(log) = active() else {
        return;
    };
    let awaiting = {
        let h = member.history.0.lock().unwrap();
        h.tasks.get(&member.index).is_some_and(|t| {
            t.invocation == Some(operation)
                && t.operations
                    .get(&operation)
                    .is_some_and(|op| op.indirect_birth == Some(IndirectBirth::AwaitingChild))
        })
    };
    // The real acquire operation must refuse before this actual native resume.
    // Store its result; do not assert while an original child is still alive.
    let acquisition = member.acquire().map(drop);
    let mut log = log.lock().unwrap();
    log.member = Some(member.clone());
    log.entries.push((operation, awaiting, acquisition));
}
pub(super) fn returned(member: &Member, nr: Sysno, stopped: &Stopped, raw: i64, typed: bool) {
    if nr != Sysno::clone3 {
        return;
    }
    let Some(log) = active() else {
        return;
    };
    let identity = stopped.terminal_cleanup().task_identity();
    let h = member.history.0.lock().unwrap();
    let same = identity.as_ref().is_ok_and(|id| {
        h.tasks
            .get(&member.index)
            .is_some_and(|t| t.identity.same_generation(id))
    });
    drop(h);
    log.lock().unwrap().returns.push((raw, typed, same));
}
pub(super) fn child_event() {
    if let Some(log) = active() {
        log.lock().unwrap().child_events += 1;
    }
}

struct ReadObservation {
    marker: usize,
    address: usize,
    length: usize,
    legacy: Result<Vec<u8>, NativeUserReadError>,
    followed: Result<Vec<u8>, NativeUserReadError>,
    // Actual post-return metadata, sampled only at the subsequent held callback.
    post: Option<(bool, usize, usize, bool)>,
}
#[derive(Default)]
struct Log {
    reads: Mutex<Vec<ReadObservation>>,
    drops: Arc<AtomicUsize>,
}
#[reverie::global_tool]
impl GlobalTool for Log {
    type Config = u8;
    type Request = ();
    type Response = ();
    async fn receive_rpc(&self, _: Pid, _: ()) {}
}
struct Retention(Arc<AtomicUsize>);
impl Drop for Retention {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}
#[derive(Default)]
struct Probe;
#[reverie::tool]
impl Tool for Probe {
    type GlobalState = Log;
    type ThreadState = ();
    fn subscriptions(_: &u8) -> Subscription {
        // clone3 is deliberately NOT subscribed or emulated by this Tool.
        [Sysno::write].into_iter().collect()
    }
    async fn handle_syscall_event<G: Guest<Self>>(
        &self,
        guest: &mut G,
        call: Syscall,
    ) -> Result<i64, Error> {
        let (_, args) = call.into_parts();
        if !matches!(args.arg0, PRE | POST) {
            return Ok(guest.inject(call).await?);
        }
        let drops = guest.local_global_state().unwrap().drops.clone();
        let legacy = guest
            .read_native_source(args.arg1, args.arg2, Box::new(Retention(drops.clone())))
            .await;
        let followed = guest
            .stage_followed_source(args.arg1, args.arg2, Box::new(Retention(drops)))
            .await;
        let member = active().and_then(|log| log.lock().unwrap().member.clone());
        let post = member.map(|member| {
            let h = member.history.0.lock().unwrap();
            (
                h.failed,
                h.tasks.len(),
                h.tasks.values().map(|t| t.operations.len()).sum(),
                h.tasks.values().any(|t| t.invocation.is_some()),
            )
        });
        guest
            .local_global_state()
            .unwrap()
            .reads
            .lock()
            .unwrap()
            .push(ReadObservation {
                marker: args.arg0,
                address: args.arg1,
                length: args.arg2,
                legacy,
                followed,
                post,
            });
        // Sentinel protocol continues even after wrong publication/refusal;
        // every semantic comparison is after child/root cleanup below.
        Ok(4)
    }
}

async fn case(mode: u8) {
    let fixture = PathBuf::from(
        std::env::var_os("REVERIE_CLONE3_JOIN_FIXTURE")
            .expect("Main must bind the freshly compiled clone3 fixture"),
    );
    assert!(fixture.is_absolute());
    let native = Arc::new(Mutex::new(NativeLog::default()));
    ACTIVE.with(|s| assert!(s.replace(Some(native.clone())).is_none()));
    let _reset = Reset;
    let mut command = Command::new(fixture);
    command.arg(mode.to_string());
    command.stdout(reverie::process::Stdio::piped());
    command.stderr(reverie::process::Stdio::piped());
    let started = Instant::now();
    let deadline = started + Duration::from_secs(5);
    let tracer = TracerBuilder::<Probe>::new(command)
        .config(mode)
        .spawn()
        .await
        .unwrap();
    let root = tracer.guest_pid();
    let termination = tracer
        .termination_handle()
        .expect("existing original Command owner");
    let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, root.as_raw(), 0) };
    let open_error = (fd < 0).then(std::io::Error::last_os_error);
    let pidfd = (fd >= 0).then(|| unsafe { OwnedFd::from_raw_fd(fd as i32) });
    if let Some(error) = &open_error {
        termination.terminate(anyhow::anyhow!("clone3 original pidfd: {error}").into());
    }
    let mut future = Box::pin(tracer.wait_with_output_completion());
    let outcome = tokio::time::timeout(
        deadline.saturating_duration_since(Instant::now()),
        &mut future,
    )
    .await;
    let (complete, original_bound) = match outcome {
        Ok(ToolRunOutcome::Complete(complete)) => (complete, true),
        other => {
            termination.terminate(anyhow::anyhow!("clone3 original deadline/refusal").into());
            let rescue = match other {
                Err(_) => tokio::time::timeout(Duration::from_secs(2), &mut future).await,
                Ok(ToolRunOutcome::CleanupPending(pending)) => {
                    tokio::time::timeout(Duration::from_secs(2), pending.resume_cleanup()).await
                }
                Ok(ToolRunOutcome::UnsupportedBackend(tracer)) => {
                    tokio::time::timeout(
                        Duration::from_secs(2),
                        tracer.wait_with_output_completion(),
                    )
                    .await
                }
                Ok(ToolRunOutcome::Complete(_)) => unreachable!(),
            };
            match rescue {
                Ok(ToolRunOutcome::Complete(complete)) => (complete, false),
                other => {
                    if let Ok(ToolRunOutcome::CleanupPending(pending)) = other {
                        eprintln!(
                            "clone3 cleanup unconfirmed: {:#}",
                            pending.quarantine_source_test()
                        );
                    }
                    drop(pidfd);
                    panic!("clone3 original cleanup unconfirmed; no causal verdict");
                }
            }
        }
    };
    let elapsed = started.elapsed();
    let custody = pidfd.map(|pidfd| {
        let mut poll = libc::pollfd {
            fd: pidfd.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        let polled = unsafe { libc::poll(&mut poll, 1, 0) };
        let mut status = 0;
        let waited = unsafe { libc::waitpid(root.as_raw(), &mut status, libc::WNOHANG) };
        let error = Errno::last();
        let closed = unsafe { libc::close(pidfd.into_raw_fd()) };
        (polled, poll.revents, waited, error, closed)
    });
    assert!(
        open_error.is_none(),
        "original pidfd acquisition: {open_error:?}"
    );
    let (polled, revents, waited, error, closed) = custody.unwrap();
    assert_eq!(closed, 0, "original pidfd close");
    assert_eq!(polled, 1);
    assert_ne!(revents & libc::POLLIN, 0);
    assert_eq!(revents & (libc::POLLERR | libc::POLLNVAL), 0);
    assert_eq!(waited, -1);
    assert_eq!(error, Errno::ECHILD, "original completion owns reap");
    eprintln!("CLONE3_JOIN original_reaped=1 pidfd_closed=1 mode={mode}");
    assert!(
        original_bound && elapsed <= Duration::from_secs(5),
        "original body bound"
    );

    let output = complete.result.expect("actual output/EOF completion");
    eprintln!(
        "CLONE3_JOIN fixture_status={:?} stdout={:?} stderr={:?}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        output.status,
        ExitStatus::Exited(0),
        "real syscall outcome; no unavailable skip"
    );
    assert!(output.stderr.is_empty());
    let native = native.lock().unwrap();
    assert_eq!(
        native.child_events, 0,
        "no actual ptrace birth event for these three cases"
    );
    let expected = if mode == 1 {
        assert_eq!(native.returns.len(), 1);
        let child = native.returns[0].0;
        assert!(child > 0, "actual positive parent EXIT");
        format!(
            "CLONE3_JOIN mode=1 result={child} errno=0 child_result=0 child_pid={child} effect=X live=1 waited={child} status=0 eof=1 pipes_closed=1 absent=ECHILD bytes=ABCD\n"
        )
    } else {
        let result = if mode == 0 {
            -1
        } else {
            i64::from(root.as_raw())
        };
        let error = if mode == 0 { libc::EINVAL } else { 0 };
        format!(
            "CLONE3_JOIN mode={mode} result={result} errno={error} child=none pipes_closed=1 absent=ECHILD bytes=ABCD\n"
        )
    };
    assert_eq!(
        output.stdout,
        expected.as_bytes(),
        "exact native child/result/cleanup receipt"
    );
    let log = complete.global_state;
    assert_eq!(
        log.drops.load(Ordering::SeqCst),
        4,
        "all four source-call retentions retired"
    );
    let reads = log.reads.lock().unwrap();
    assert_eq!(
        reads.len(),
        2,
        "one pre and one post source pair, no retries"
    );
    assert_eq!((reads[0].marker, reads[0].length), (PRE, 4));
    assert_eq!(
        (reads[1].marker, reads[1].address, reads[1].length),
        (POST, reads[0].address, 4)
    );
    assert_eq!(
        reads[0].legacy,
        Ok(b"ABCD".to_vec()),
        "legacy source works before birth attempt"
    );
    assert_eq!(
        reads[0].followed,
        Ok(b"ABCD".to_vec()),
        "followed source works before birth attempt"
    );
    eprintln!(
        "CLONE3_JOIN mode={mode} raw={:?} legacy={:?} followed={:?} post={:?}",
        native.returns, reads[1].legacy, reads[1].followed, reads[1].post
    );
    let refused = |e| {
        Err(NativeUserReadError::Refused(
            NativeUserReadRefusal::TargetState(e),
        ))
    };
    if mode == 2 {
        assert_eq!(reads[1].legacy, Ok(b"ABCD".to_vec()));
        assert_eq!(reads[1].followed, Ok(b"ABCD".to_vec()));
    } else {
        assert_eq!(
            reads[1].legacy,
            refused(Errno::ENOTSUPP),
            "clone3 SourceEpoch remains revoked"
        );
        if mode == 0 {
            assert_eq!(
                reads[1].followed,
                Ok(b"ABCD".to_vec()),
                "final EINVAL permits followed source"
            );
            assert_eq!(
                reads[1].post,
                Some((false, 1, 0, false)),
                "real final negative retires only its debt"
            );
        } else {
            assert_eq!(
                reads[1].post,
                Some((true, 0, 0, false)),
                "positive without event permanently refuses"
            );
            assert_eq!(
                reads[1].followed,
                refused(Errno::ESTALE),
                "hidden live child cannot authorize publication"
            );
        }
    }
    if mode != 2 {
        assert_eq!(native.entries.len(), 1);
        assert!(
            native.entries[0].1,
            "actual unresolved indirect-birth debt installed"
        );
        assert_eq!(
            native.entries[0].2,
            Err(safeptrace::Errno::EBUSY),
            "actual FollowedHold attempt refuses before native effect"
        );
        assert_eq!(native.returns.len(), 1);
        assert!(
            native.returns[0].1 && native.returns[0].2,
            "typed EXIT and original task generation"
        );
        if mode == 0 {
            assert_eq!(native.returns[0].0, -(libc::EINVAL as i64));
        }
    } else {
        assert!(native.entries.is_empty() && native.returns.is_empty());
    }
}

#[tokio::test(flavor = "current_thread")]
async fn final_einval_retires_indirect_birth_but_not_epoch() {
    case(0).await;
}
#[tokio::test(flavor = "current_thread")]
async fn untraced_positive_without_event_permanently_refuses() {
    case(1).await;
}
#[tokio::test(flavor = "current_thread")]
async fn ordinary_getpid_neighbor_keeps_both_sources() {
    case(2).await;
}
