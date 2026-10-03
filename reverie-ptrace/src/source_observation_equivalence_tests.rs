/* Copyright (c) Meta Platforms, Inc. and affiliates. All rights reserved.
 * Licensed under the BSD-style license in the root LICENSE file. */

//! Actual pre-read, native no-content-change exposure, and post-read controls.
//! A semantic assertion never interrupts the original child's cleanup.
use std::os::fd::AsRawFd;
use std::os::fd::FromRawFd;
use std::os::fd::IntoRawFd;
use std::os::fd::OwnedFd;

use reverie::InjectedSyscallEvent;
use reverie::Tid;
use reverie::syscalls::NativeUserReadError;
use reverie::syscalls::NativeUserReadRefusal;

use super::*;

const PRE_READ: usize = 781;
const POST_READ: usize = 782;
const INJECT_MADVISE: usize = 783;

type ReadObservation = (usize, usize, usize, Result<Vec<u8>, NativeUserReadError>);

#[derive(Default)]
struct ObservationLog {
    reads: StdMutex<Vec<ReadObservation>>,
    followed_reads: StdMutex<Vec<Result<Vec<u8>, NativeUserReadError>>>,
    injected: StdMutex<Vec<(SyscallArgs, InjectedSyscallEvent)>>,
    injection_results: StdMutex<Vec<Result<i64, Errno>>>,
    signals: StdMutex<Vec<Signal>>,
    retention_drops: Arc<AtomicUsize>,
}
#[reverie::global_tool]
impl GlobalTool for ObservationLog {
    type Config = u8;
    type Request = ();
    type Response = ();
    async fn receive_rpc(&self, _: Pid, _: ()) {}
}

#[derive(Default)]
struct ObservationTool {
    mode: u8,
    classifications: AtomicUsize,
}
#[reverie::tool]
impl Tool for ObservationTool {
    type GlobalState = ObservationLog;
    type ThreadState = ();
    fn new(_: Pid, mode: &u8) -> Self {
        Self {
            mode: *mode,
            classifications: AtomicUsize::new(0),
        }
    }
    fn subscriptions(mode: &u8) -> Subscription {
        // Preserve all original O/P/I and generic ioctl bodies unchanged.
        if *mode >= 6 {
            [Sysno::write, Sysno::ioctl].into_iter().collect()
        } else {
            [Sysno::write].into_iter().collect()
        }
    }
    fn classify_original_source_ioctl(
        &self,
        _: &ObservationLog,
        entry: &reverie::OriginalIoctlEntry,
    ) -> Option<reverie::OriginalIoctlEffect> {
        let attempt = self.classifications.fetch_add(1, Ordering::SeqCst);
        if self.mode == 7 || (self.mode == 14 && attempt == 0) || entry.args().arg0 != 0 {
            return None;
        }
        // Controlled native fixture: Command::stdin(null) is this backend's
        // own normal open of /dev/null, inherited as fd0, and the fixture has
        // performed no close/dup/exec/descriptor transfer before this call.
        // On the qualified kernel memory_open installs null_fops (no handler).
        // This tests consumption, not Hermit's separate provider proof issuer.
        if self.mode == 9 {
            let stale = unsafe {
                reverie::OriginalIoctlEntry::from_original_backend_entry(entry.tid(), entry.args())
            };
            return unsafe {
                stale.certify_dispatch(reverie::OriginalIoctlDispatch::NullFileOperations)
            };
        }
        unsafe { entry.certify_dispatch(reverie::OriginalIoctlDispatch::NullFileOperations) }
    }
    fn observe_injected_syscalls(_: &u8) -> bool {
        true
    }
    fn on_injected_syscall_observed(
        &self,
        _: Tid,
        log: &ObservationLog,
        _: &mut (),
        nr: Sysno,
        args: SyscallArgs,
        event: InjectedSyscallEvent,
    ) {
        if self.mode == 12
            && nr == Sysno::ioctl
            && event == InjectedSyscallEvent::Returned(-(libc::ENOTTY as i64))
        {
            crate::task::source_epoch::omit_next_ioctl_completion_for_test();
        }
        if nr == Sysno::madvise {
            log.injected.lock().unwrap().push((args, event));
        }
    }
    async fn handle_signal_event<G: Guest<Self>>(
        &self,
        guest: &mut G,
        signal: Signal,
    ) -> Result<Option<Signal>, Errno> {
        guest
            .local_global_state()
            .unwrap()
            .signals
            .lock()
            .unwrap()
            .push(signal);
        Ok(Some(signal)) // deliver the original signal, including the real private UD2
    }
    async fn handle_syscall_event<G: Guest<Self>>(
        &self,
        guest: &mut G,
        call: Syscall,
    ) -> Result<i64, Error> {
        let (nr, args) = call.into_parts();
        if self.mode == 13 && nr == Sysno::ioctl {
            // This private syscall consumes/skips the original pending entry.
            // A later numerically equal private ioctl must not borrow its proof.
            guest
                .inject(Syscall::Other(
                    Sysno::getpid,
                    SyscallArgs::new(0, 0, 0, 0, 0, 0),
                ))
                .await?;
        }
        if self.mode == 11 && nr == Sysno::ioctl {
            let rewrite = SyscallArgs::new(
                args.arg0, 0x5402, args.arg2, args.arg3, args.arg4, args.arg5,
            );
            return Ok(guest.inject(Syscall::Other(nr, rewrite)).await?);
        }
        if matches!(args.arg0, PRE_READ | POST_READ) {
            let retention = Retention(guest.local_global_state().unwrap().retention_drops.clone());
            let result = guest
                .read_native_source(args.arg1, args.arg2, Box::new(retention))
                .await;
            guest
                .local_global_state()
                .unwrap()
                .reads
                .lock()
                .unwrap()
                .push((args.arg0, args.arg1, args.arg2, result));
            if self.mode >= 6 {
                let followed = guest
                    .stage_followed_source(args.arg1, args.arg2, Box::new(()))
                    .await;
                guest
                    .local_global_state()
                    .unwrap()
                    .followed_reads
                    .lock()
                    .unwrap()
                    .push(followed);
            }
            // This is the test's sentinel protocol, not a guest syscall result
            // inferred from source success. Even bad publication reaches reap.
            return Ok(4);
        }
        if args.arg0 == INJECT_MADVISE {
            let result = guest
                .inject(Syscall::Other(
                    Sysno::madvise,
                    SyscallArgs::new(args.arg1, args.arg2, libc::MADV_NORMAL as usize, 0, 0, 0),
                ))
                .await;
            guest
                .local_global_state()
                .unwrap()
                .injection_results
                .lock()
                .unwrap()
                .push(result);
            return Ok(result?);
        }
        Ok(guest.inject(call).await?)
    }
}

fn observation_fixture() -> PathBuf {
    let path = PathBuf::from(crate::testing::fixture_path(
        "REVERIE_SOURCE_OBSERVATION_FIXTURE",
    ));
    assert!(path.is_absolute(), "no PATH fixture or compiler lookup");
    path
}

async fn observation_case(mode: u8) {
    let mut command = Command::new(observation_fixture());
    command.arg(mode.to_string());
    if mode >= 6 {
        command.stdin(reverie::process::Stdio::null());
    }
    command.stdout(reverie::process::Stdio::piped());
    command.stderr(reverie::process::Stdio::piped());
    let started = Instant::now();
    let deadline = started + Duration::from_secs(5);
    let tracer = TracerBuilder::<ObservationTool>::new(command)
        .config(mode)
        .spawn()
        .await
        .unwrap();
    let root = tracer.guest_pid();
    let termination = tracer
        .termination_handle()
        .expect("ordinary original owner");
    let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, root.as_raw(), 0) };
    let open_error = (fd < 0).then(std::io::Error::last_os_error);
    let pidfd = (fd >= 0).then(|| unsafe { OwnedFd::from_raw_fd(fd as i32) });
    if let Some(error) = &open_error {
        termination.terminate(anyhow::anyhow!("observation original pidfd: {error}").into());
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
            termination.terminate(anyhow::anyhow!("observation original deadline/refusal").into());
            // One original two-second cleanup interval, never a semantic retry.
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
                            "observation cleanup unconfirmed: {:#}",
                            pending.quarantine()
                        );
                    }
                    drop(pidfd);
                    panic!("original observation fixture cleanup unconfirmed; no causal verdict");
                }
            }
        }
    };
    let elapsed = started.elapsed();
    // Capture the actual original-pidfd readiness and reap checks, then close
    // the FD before asserting anything about source policy or native results.
    let custody = pidfd.map(|pidfd| {
        let mut poll = libc::pollfd {
            fd: pidfd.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        let polled = unsafe { libc::poll(&mut poll, 1, 0) };
        let mut status = 0;
        let waited = unsafe { libc::waitpid(root.as_raw(), &mut status, libc::WNOHANG) };
        let wait_error = Errno::last();
        let closed = unsafe { libc::close(pidfd.into_raw_fd()) };
        (polled, poll.revents, waited, wait_error, closed)
    });
    assert!(
        open_error.is_none(),
        "original pidfd acquisition: {open_error:?}"
    );
    let (polled, revents, waited, wait_error, closed) = custody.unwrap();
    assert_eq!(closed, 0, "original pidfd close");
    assert_eq!(polled, 1);
    assert_ne!(revents & libc::POLLIN, 0);
    assert_eq!(revents & (libc::POLLERR | libc::POLLNVAL), 0);
    assert_eq!(waited, -1);
    assert_eq!(
        wait_error,
        Errno::ECHILD,
        "original completion owns the reap"
    );
    eprintln!("OBSERVATION_EQ original_reaped=1 pidfd_closed=1 mode={mode}");
    assert!(
        original_bound && elapsed <= Duration::from_secs(5),
        "original five-second body bound"
    );

    let output = complete
        .result
        .expect("actual output/EOF completion, not outer containment");
    assert_eq!(
        output.status,
        ExitStatus::Exited(0),
        "actual native operation and fixture checks"
    );
    assert!(
        output.stderr.is_empty(),
        "unexpected guest stderr: {:?}",
        output.stderr
    );
    let native_result = if mode == 3 {
        i64::from(root.as_raw())
    } else if mode == 4 || mode >= 6 {
        -1
    } else {
        0
    };
    if mode >= 6 {
        let request = if mode == 8 {
            0x5413
        } else if mode == 10 {
            0x5402
        } else {
            0x5401
        };
        let expected = format!(
            "OBSERVATION_TERMINAL mode={mode} native_result=-1 errno={} request={request} bytes=ABCD close=0 status=0\n",
            libc::ENOTTY
        );
        assert_eq!(
            output.stdout,
            expected.as_bytes(),
            "original ioctl, unchanged argument/source, explicit close"
        );
    } else if mode >= 4 {
        let (errno, request, argument, flag_delta) = if mode == 4 {
            (libc::EBADF, 0x541b, 123456, 0)
        } else {
            (0, 0x5421, 1, libc::O_NONBLOCK)
        };
        let expected = format!(
            "OBSERVATION_IOCTL mode={mode} native_result={native_result} errno={errno} request={request} argument={argument} read_flags_delta={flag_delta} write_flags_delta=0 bytes=ABCD closes=0,0 status=0\n"
        );
        assert_eq!(
            output.stdout,
            expected.as_bytes(),
            "actual ioctl result, unchanged input/other flags and both original closes"
        );
    } else {
        let expected = format!(
            "OBSERVATION_EQ mode={mode} native_result={native_result} errno=0 bytes=ABCD private_returns={}\n",
            usize::from(mode == 1)
        );
        assert_eq!(
            output.stdout,
            expected.as_bytes(),
            "unchanged native result and bytes"
        );
    }
    let log = complete.global_state;
    assert_eq!(
        log.retention_drops.load(Ordering::SeqCst),
        2,
        "both source-call retentions retired"
    );
    assert_eq!(
        *log.signals.lock().unwrap(),
        if mode == 1 {
            vec![Signal::SIGILL]
        } else {
            vec![]
        }
    );
    let reads = log.reads.lock().unwrap();
    assert_eq!(reads.len(), 2, "one pre-read and one post-read, no retries");
    assert_eq!((reads[0].0, reads[0].2), (PRE_READ, 4));
    assert_eq!(
        (reads[1].0, reads[1].1, reads[1].2),
        (POST_READ, reads[0].1, 4)
    );
    assert_eq!(
        reads[0].3,
        Ok(b"ABCD".to_vec()),
        "actual source must work BEFORE exposure"
    );
    if mode >= 6 {
        let followed = log.followed_reads.lock().unwrap();
        assert_eq!(followed.len(), 2);
        assert_eq!(
            followed[0],
            Ok(b"ABCD".to_vec()),
            "followed source works before query"
        );
        assert_eq!(
            followed[1],
            if matches!(mode, 6 | 8) {
                Ok(b"ABCD".to_vec())
            } else {
                Err(NativeUserReadError::Refused(
                    NativeUserReadRefusal::TargetState(Errno::ESTALE),
                ))
            },
            "same native query must preserve or close BOTH source histories"
        );
    }
    let args = SyscallArgs::new(reads[0].1, 4096, libc::MADV_NORMAL as usize, 0, 0, 0);
    assert_eq!(
        *log.injected.lock().unwrap(),
        if mode == 2 {
            vec![
                (args, InjectedSyscallEvent::Entered),
                (args, InjectedSyscallEvent::Returned(0)),
            ]
        } else {
            vec![]
        },
        "only Tool route uses its actual injected ENTRY and EXIT"
    );
    assert_eq!(
        *log.injection_results.lock().unwrap(),
        if mode == 2 { vec![Ok(0)] } else { vec![] }
    );
    eprintln!(
        "OBSERVATION_EQ mode={mode} pre={:?} post={:?} native_result={native_result}",
        reads[0].3, reads[1].3
    );
    if matches!(mode, 3 | 5 | 6 | 8) {
        assert_eq!(
            reads[1].3,
            Ok(b"ABCD".to_vec()),
            "ordinary neighbor source remains valid"
        );
    } else if mode == 4 {
        assert_eq!(
            reads[1].3,
            Err(NativeUserReadError::Refused(
                NativeUserReadRefusal::TargetState(Errno::ENOTSUPP)
            )),
            "actual failed FIONREAD exposure must revoke publication after successful pre-read"
        );
    } else {
        assert_eq!(
            reads[1].3,
            Err(NativeUserReadError::Refused(
                NativeUserReadRefusal::TargetState(Errno::ENOTSUPP)
            )),
            "unclassified exposure or missing completion must revoke publication after successful pre-read"
        );
    }
}

#[tokio::test(flavor = "current_thread")]
async fn ordinary_madv_normal_revokes_after_valid_source() {
    observation_case(0).await;
}
#[tokio::test(flavor = "current_thread")]
async fn private_madv_normal_revokes_after_valid_source() {
    observation_case(1).await;
}
#[tokio::test(flavor = "current_thread")]
async fn injected_madv_normal_revokes_after_valid_source() {
    observation_case(2).await;
}
#[tokio::test(flavor = "current_thread")]
async fn ordinary_getpid_keeps_source_valid_twice() {
    observation_case(3).await;
}

#[tokio::test(flavor = "current_thread")]
async fn ordinary_failed_fionread_revokes_after_valid_source() {
    observation_case(4).await;
}
#[tokio::test(flavor = "current_thread")]
async fn ordinary_fionbio_changes_owned_pipe_and_keeps_source_valid() {
    observation_case(5).await;
}

#[tokio::test(flavor = "current_thread")]
async fn original_null_tcgets_dispatch_keeps_source_valid() {
    observation_case(6).await;
}
#[tokio::test(flavor = "current_thread")]
async fn original_null_tcgets_without_dispatch_proof_revokes_source() {
    observation_case(7).await;
}
#[tokio::test(flavor = "current_thread")]
async fn original_null_tiocgwinsz_dispatch_keeps_source_valid() {
    observation_case(8).await;
}
#[tokio::test(flavor = "current_thread")]
async fn original_null_tcgets_stale_attempt_proof_revokes_source() {
    observation_case(9).await;
}
#[tokio::test(flavor = "current_thread")]
async fn original_null_tcsets_dispatch_remains_unclassified() {
    observation_case(10).await;
}
#[tokio::test(flavor = "current_thread")]
async fn original_null_tcgets_rewrite_cannot_borrow_proof() {
    observation_case(11).await;
}

#[tokio::test(flavor = "current_thread")]
async fn original_null_tcgets_missing_completion_closes_both_source_histories() {
    observation_case(12).await;
}

#[tokio::test(flavor = "current_thread")]
async fn private_ioctl_cannot_borrow_original_equal_tuple_proof() {
    observation_case(13).await;
}
#[tokio::test(flavor = "current_thread")]
async fn later_certified_ioctl_cannot_reset_revoked_histories() {
    observation_case(14).await;
}
