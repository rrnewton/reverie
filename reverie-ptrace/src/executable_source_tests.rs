/* Copyright (c) Meta Platforms, Inc. and affiliates. All rights reserved.
 * Licensed under the BSD-style license in the root LICENSE file. */
//! Real capture/worker/native memory evidence with a CONTROLLED unsafe backing
//! issuer. This is not actual provider or production executable qualification.
use std::cell::RefCell;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;
use std::time::Duration;

use futures::FutureExt;
use reverie::GlobalTool;
use reverie::Guest;
use reverie::Pid;
use reverie::Subscription;
use reverie::Tool;
use reverie::syscalls::ArmedExecutableSource;
use reverie::syscalls::ExecutableBackingGeometry;
use reverie::syscalls::ExecutableBackingProof;
use reverie::syscalls::ExecutableCaptureIssuer;
use reverie::syscalls::ExecutableCaptureRequest;
use reverie::syscalls::ExecutableSourceArmer;
use reverie::syscalls::ExecutableSourceCapture;
use reverie::syscalls::ExecutableSourceChallenge;
use reverie::syscalls::NativeUserReadError;
use reverie::syscalls::NativeUserReadRefusal;
use reverie::syscalls::Syscall;
use reverie::syscalls::SyscallInfo;

use super::*;

#[derive(Default)]
struct Probe {
    arrived: AtomicUsize,
    step: AtomicUsize,
    done: AtomicBool,
    caught: AtomicBool,
    installed: AtomicBool,
    collected: AtomicBool,
    capture_fault_applied: AtomicUsize,
    released: AtomicBool,
    drops: AtomicUsize,
    request: Mutex<Option<ExecutableCaptureRequest>>,
    allocation: Mutex<Option<Weak<dyn Send + Sync>>>,
    retirement_pause: Mutex<Option<crate::task::source_jobs::RetirementPause>>,
    bytes: Mutex<Vec<u8>>,
    session: Mutex<Weak<super::super::FatalSession>>,
    terminals: Mutex<Vec<Arc<crate::tracer::FatalTaskStop>>>,
}
thread_local! { static ACTIVE: RefCell<Weak<Probe>> = const { RefCell::new(Weak::new()) }; }
pub(crate) fn prepared(plan: &safeptrace::FollowedExecutableSourceReadPlan) {
    ACTIVE.with(|slot| {
        let probe = slot.borrow().upgrade().unwrap();
        *probe.allocation.lock().unwrap() = Some(plan.capture_allocation_observation());
        // This is the real executable plan, after the prerequisite anonymous
        // refusal has joined; target only this next actual registry submission.
        let pause = probe.retirement_pause.lock().unwrap().take();
        if let Some(pause) = pause {
            probe
                .session
                .lock()
                .unwrap()
                .upgrade()
                .unwrap()
                .source_jobs
                .pause_next_retirement(pause);
        }
    });
}

fn error() -> NativeUserReadError {
    NativeUserReadError::Refused(NativeUserReadRefusal::TargetState(safeptrace::Errno::EIO))
}
struct Armer {
    probe: Arc<Probe>,
    kind: u8,
}
struct Collector {
    probe: Arc<Probe>,
    kind: u8,
}
#[derive(Debug)]
struct DeliberateArmerPanic;
impl ExecutableSourceArmer for Armer {
    fn arm(
        self: Box<Self>,
        challenge: &ExecutableSourceChallenge<'_>,
    ) -> Result<Box<dyn ArmedExecutableSource>, NativeUserReadError> {
        let request = challenge.request();
        assert_eq!(
            unsafe { libc::syscall(libc::SYS_gettid) } as i32,
            request.ptracer_tid
        );
        assert_eq!(
            (
                request.ptrace_request(),
                request.register_note(),
                request.register_bytes()
            ),
            (
                libc::PTRACE_GETREGSET as usize,
                libc::NT_PRSTATUS as usize,
                216
            )
        );
        let iovec = unsafe { std::slice::from_raw_parts(request.iovec_address as *const usize, 2) };
        assert_eq!(iovec, &[request.register_buffer_address, 216]);
        let registers = unsafe {
            std::slice::from_raw_parts(request.register_buffer_address as *const u64, 27)
        };
        assert!(
            registers.iter().all(|r| *r == 0),
            "ARM must precede actual capture"
        );
        assert_eq!(self.probe.step.fetch_add(1, Ordering::SeqCst), 0);
        *self.probe.request.lock().unwrap() = Some(request);
        if self.kind == 1 {
            return Err(error());
        }
        self.probe.installed.store(true, Ordering::SeqCst);
        if self.kind == 2 {
            return Err(error());
        } // modeled installed but ACK lost
        if self.kind == 3 {
            std::panic::panic_any(DeliberateArmerPanic);
        }
        if matches!(self.kind, 4 | 5) {
            // Controlled corruption of the actual allocated iovec, not a fake GET.
            let word = if self.kind == 4 { 0 } else { 1 };
            unsafe {
                *((request.iovec_address as *mut usize).add(word)) = 0;
            }
        }
        if (13..=16).contains(&self.kind) {
            safeptrace::FollowedExecutableSourceReadPlan::set_capture_fault_for_test(
                self.kind - 12,
            );
        }
        Ok(Box::new(Collector {
            probe: self.probe,
            kind: self.kind,
        }))
    }
}
fn geometry(request: ExecutableCaptureRequest) -> ExecutableBackingGeometry {
    let maps = std::fs::read_to_string(format!("/proc/{}/maps", request.target_tid)).unwrap();
    let line = maps
        .lines()
        .find(|line| {
            let range = line.split_ascii_whitespace().next().unwrap();
            let (start, end) = range.split_once('-').unwrap();
            usize::from_str_radix(start, 16).unwrap() <= request.source_address
                && request.source_address < usize::from_str_radix(end, 16).unwrap()
        })
        .unwrap();
    let fields: Vec<_> = line.split_ascii_whitespace().collect();
    let (start, end) = fields[0].split_once('-').unwrap();
    let (major, minor) = fields[3].split_once(':').unwrap();
    assert_eq!(fields[1].as_bytes()[1], b'-');
    let stat = std::fs::metadata(format!("/proc/{}/exe", request.target_tid)).unwrap();
    ExecutableBackingGeometry {
        vma_start: usize::from_str_radix(start, 16).unwrap(),
        vma_end: usize::from_str_radix(end, 16).unwrap(),
        file_offset: usize::from_str_radix(fields[2], 16).unwrap(),
        device_major: usize::from_str_radix(major, 16).unwrap(),
        device_minor: usize::from_str_radix(minor, 16).unwrap(),
        inode: fields[4].parse().unwrap(),
        file_size: stat.len() as usize,
    }
}
impl ArmedExecutableSource for Collector {
    fn collect(
        self: Box<Self>,
        capture: ExecutableSourceCapture,
    ) -> Result<ExecutableBackingProof, NativeUserReadError> {
        let request = capture.request();
        assert_ne!(
            unsafe { libc::syscall(libc::SYS_gettid) } as i32,
            request.ptracer_tid
        );
        assert_eq!(Some(request), *self.probe.request.lock().unwrap());
        assert_eq!(self.probe.step.fetch_add(1, Ordering::SeqCst), 1);
        let iovec = unsafe { std::slice::from_raw_parts(request.iovec_address as *const usize, 2) };
        assert_eq!(iovec, &[request.register_buffer_address, 216]);
        let registers = unsafe {
            std::slice::from_raw_parts(request.register_buffer_address as *const u64, 27)
        };
        assert_eq!(
            registers[17], 0x33,
            "actual native full capture must precede collect"
        );
        self.probe.collected.store(true, Ordering::SeqCst);
        if self.kind == 6 {
            return Err(error());
        }
        if self.kind == 7 {
            panic!("controlled collector panic on registered worker");
        }
        if self.kind == 11 {
            let start = std::time::Instant::now();
            while !self.probe.released.load(Ordering::SeqCst) {
                assert!(start.elapsed() < Duration::from_secs(3));
                std::thread::yield_now();
            }
        }
        let mut geometry = geometry(request);
        if self.kind == 9 {
            geometry.inode += 1;
        }
        if self.kind == 10 {
            geometry.vma_end += 4096;
        }
        if self.kind == 8 {
            // Deliberately synthetic independent identity with equal scalars.
            let issuer = unsafe { ExecutableCaptureIssuer::new_backend(request) };
            let (foreign, _) = unsafe { issuer.completed() };
            return Ok(unsafe { foreign.certify_direct_executable(geometry) });
        }
        // Controlled unsafe premise: only this backend test supplies backing.
        // Actual ABI11 provider/deny-write/image/ACK evidence is NOT claimed.
        Ok(unsafe { capture.certify_direct_executable(geometry) })
    }
}
struct Retention(Arc<Probe>);
impl Drop for Retention {
    fn drop(&mut self) {
        self.0.drops.fetch_add(1, Ordering::SeqCst);
    }
}
#[derive(Default)]
struct Global(Arc<Probe>);
#[reverie::global_tool]
impl GlobalTool for Global {
    type Config = (bool, u8);
    type Request = ();
    type Response = ();
    async fn receive_rpc(&self, _: Pid, _: ()) {}
}
#[derive(Default)]
struct Reader;
#[reverie::tool]
impl Tool for Reader {
    type GlobalState = Global;
    type ThreadState = ();
    fn subscriptions(_: &(bool, u8)) -> Subscription {
        [
            Sysno::write,
            Sysno::clone,
            Sysno::clone3,
            Sysno::exit,
            Sysno::exit_group,
        ]
        .into_iter()
        .collect()
    }
    async fn handle_syscall_event<G: Guest<Self>>(
        &self,
        guest: &mut G,
        call: Syscall,
    ) -> Result<i64, reverie::Error> {
        let (nr, args) = call.into_parts();
        if nr != Sysno::write || args.arg0 != 754 {
            return Ok(guest.inject(call).await?);
        }
        let probe = Arc::clone(&guest.local_global_state().unwrap().0);
        probe.arrived.fetch_add(1, Ordering::SeqCst);
        while probe.arrived.load(Ordering::SeqCst) != 2 {
            tokio::task::yield_now().await;
        }
        let (child, kind) = *guest.config();
        if guest.is_root_thread() == child {
            while !probe.done.load(Ordering::SeqCst) {
                tokio::task::yield_now().await;
            }
            return Ok(8);
        }
        // Neither revoked legacy history nor anonymous file-backed refusal is reset.
        assert!(
            guest
                .read_native_source(args.arg1, args.arg2, Box::new(()))
                .await
                .is_err()
        );
        assert!(
            guest
                .stage_followed_source(args.arg1, args.arg2, Box::new(()))
                .await
                .is_err()
        );
        let armer = Box::new(Armer {
            probe: probe.clone(),
            kind,
        });
        let result = std::panic::AssertUnwindSafe(guest.stage_followed_executable_source(
            args.arg1,
            args.arg2,
            Box::new(Retention(probe.clone())),
            armer,
        ))
        .catch_unwind()
        .await;
        if (13..=16).contains(&kind) {
            assert_eq!(
                safeptrace::FollowedExecutableSourceReadPlan::take_applied_capture_fault_for_test(),
                kind - 12
            );
            let expected = match kind {
                13 | 15 => error(),
                14 => NativeUserReadError::Refused(NativeUserReadRefusal::RegisterShape(215)),
                16 => NativeUserReadError::Refused(NativeUserReadRefusal::UnsupportedPlatform),
                _ => unreachable!(),
            };
            assert!(matches!(&result, Ok(Err(actual)) if *actual == expected));
            probe
                .capture_fault_applied
                .store((kind - 12) as usize, Ordering::SeqCst);
        }
        match result {
            Ok(Ok(bytes)) => {
                *probe.bytes.lock().unwrap() = bytes;
                probe.done.store(true, Ordering::SeqCst);
            }
            Ok(Err(actual)) => {
                let expected = match kind {
                    1 | 2 | 6 | 7 => Some(error()),
                    4 | 5 | 8 => Some(NativeUserReadError::Refused(
                        NativeUserReadRefusal::TargetState(safeptrace::Errno::ESTALE),
                    )),
                    9 | 10 => Some(NativeUserReadError::Refused(
                        NativeUserReadRefusal::UnsupportedBacking,
                    )),
                    3 => panic!("deliberate armer panic was lost"),
                    _ => None,
                };
                if let Some(expected) = expected {
                    assert_eq!(
                        actual, expected,
                        "exact negative capture boundary kind={kind}"
                    );
                }
                probe.caught.store(true, Ordering::SeqCst);
            }
            Err(payload) => {
                if kind != 3 || !payload.is::<DeliberateArmerPanic>() {
                    std::panic::resume_unwind(payload);
                }
                probe.caught.store(true, Ordering::SeqCst);
            }
        }
        Ok(8) // Tool catches every error; backend obligation must still fail.
    }
    fn on_backend_thread_terminal(
        &self,
        tid: Pid,
        global: &Global,
        _: &mut (),
        _: reverie::ExitStatus,
    ) {
        let session = global.0.session.lock().unwrap().upgrade().unwrap();
        let task = session
            .tree
            .lock()
            .unwrap()
            .tasks
            .iter()
            .find(|t| t.tid == tid)
            .unwrap()
            .clone();
        global.0.terminals.lock().unwrap().push(task);
    }
}
// Preserve a real TLS release even when a test assertion unwinds. This is only
// fixture cleanup; no failed assertion, completion or join is counted as pass.
struct ReleaseJoin(Option<std::sync::mpsc::Sender<()>>);
impl ReleaseJoin {
    fn release(&mut self) {
        self.0.take().unwrap().send(()).unwrap();
    }
}
impl Drop for ReleaseJoin {
    fn drop(&mut self) {
        if let Some(release) = self.0.take() {
            let _ = release.send(());
        }
    }
}

async fn case(child: bool, kind: u8) {
    let mut command =
        reverie::process::Command::new(crate::testing::fixture_path("COHORT_BRIDGE_FIXTURE"));
    command.arg("executable-source-754");
    let tracer = crate::TracerBuilder::<Reader>::new(command)
        .config((child, kind))
        .spawn()
        .await
        .unwrap();
    let (session, global) = tracer.followed_source_test_context();
    let probe = Arc::clone(&global.0);
    *probe.session.lock().unwrap() = Arc::downgrade(&session);
    drop(global);
    ACTIVE.with(|slot| *slot.borrow_mut() = Arc::downgrade(&probe));
    let terminate = tracer.termination_handle().unwrap();
    let joined = Arc::new(AtomicBool::new(false));
    let (release_join, join_gate) = std::sync::mpsc::channel();
    let mut release_join = ReleaseJoin(Some(release_join));
    if matches!(kind, 0 | 12) {
        *probe.retirement_pause.lock().unwrap() = Some(crate::task::source_jobs::RetirementPause {
            entered: joined.clone(),
            release: join_gate,
        });
    }
    let completion = tracer.wait_completion();
    futures::pin_mut!(completion);
    if matches!(kind, 0 | 11 | 12) {
        tokio::time::timeout(Duration::from_secs(3),async{
            while !(if kind==11{probe.collected.load(Ordering::SeqCst)}else{joined.load(Ordering::SeqCst)}){
                tokio::select!{_=&mut completion=>panic!("completion before actual worker boundary"),_=tokio::time::sleep(Duration::from_millis(1))=>{}}
            }
        }).await.unwrap();
        assert_eq!(session.source_jobs.pending_jobs(), 1);
        assert_eq!(probe.drops.load(Ordering::SeqCst), 0);
        assert!(
            probe
                .allocation
                .lock()
                .unwrap()
                .as_ref()
                .unwrap()
                .upgrade()
                .is_some(),
            "real capture frame must survive through true join"
        );
        {
            let h = session.source_cohort.0.lock().unwrap();
            assert_eq!(h.tasks.len(), 2);
            assert!(h.hold.is_some());
            for task in h.tasks.values() {
                let (_, checks) = task
                    .stop
                    .as_ref()
                    .unwrap()
                    .probe_held_controls(probe.request.lock().unwrap().unwrap().source_address)
                    .unwrap();
                assert!(checks.into_iter().all(|x| x));
            }
        }
        if matches!(kind, 11 | 12) {
            assert!(
                terminate
                    .terminate(anyhow::anyhow!("controlled executable worker cancellation").into())
            );
        }
        if kind == 11 {
            probe.released.store(true, Ordering::SeqCst);
        } else {
            release_join.release();
        }
    }
    let outcome = tokio::time::timeout(Duration::from_secs(3), &mut completion)
        .await
        .unwrap();
    let crate::ToolRunOutcome::Complete(completed) = outcome else {
        panic!("original executable cleanup unconfirmed")
    };
    assert_eq!(
        completed.result.is_ok(),
        kind == 0,
        "{:?}",
        completed.result
    );
    assert_eq!(probe.done.load(Ordering::SeqCst), kind == 0);
    if kind == 0 {
        assert_eq!(
            &*probe.bytes.lock().unwrap(),
            if child { b"child754" } else { b"root-754" }
        );
    } else if !(11..13).contains(&kind) {
        assert!(probe.caught.load(Ordering::SeqCst));
    }
    assert_eq!(
        probe.step.load(Ordering::SeqCst),
        if matches!(kind, 1..=5 | 13..=16) {
            1
        } else {
            2
        }
    );
    assert_eq!(
        probe.installed.load(Ordering::SeqCst),
        kind != 1,
        "modeled installed command debt remains retained after ACK loss/error"
    );
    assert_eq!(
        probe.collected.load(Ordering::SeqCst),
        !matches!(kind, 1..=5 | 13..=16)
    );
    assert_eq!(
        probe.capture_fault_applied.load(Ordering::SeqCst),
        if kind >= 13 { (kind - 12) as usize } else { 0 }
    );
    assert_eq!(probe.drops.load(Ordering::SeqCst), 1);
    assert_eq!(session.source_jobs.pending_jobs(), 0);
    if let Some(allocation) = probe.allocation.lock().unwrap().as_ref() {
        assert!(allocation.upgrade().is_none());
    }
    let owners = std::mem::take(&mut *probe.terminals.lock().unwrap());
    assert_eq!(owners.len(), 2);
    for owner in owners {
        let worker = owner
            .terminal
            .take_final_test_worker()
            .expect("original notifier");
        tokio::time::timeout(Duration::from_secs(1), async {
            while !worker.is_finished() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert!(worker.join().is_ok());
        assert!(owner.terminal.final_test_activity().2);
    }
}
#[tokio::test(flavor = "current_thread")]
async fn native_executable_source_actual_capture_read_and_true_join() {
    for child in [false, true] {
        case(child, 0).await;
    }
}
#[tokio::test(flavor = "current_thread")]
async fn native_executable_source_caught_arm_and_worker_failures_cannot_resume() {
    for kind in 1..=10 {
        case(false, kind).await;
    }
}
#[tokio::test(flavor = "current_thread")]
async fn native_executable_source_cancel_collection_and_true_join() {
    for child in [false, true] {
        for kind in [11, 12] {
            case(child, kind).await;
        }
    }
}

#[tokio::test(flavor = "current_thread")]
async fn native_executable_source_controlled_capture_errors_remain_fatal() {
    // Actual ARM/capture syscalls and original fatal cleanup; failure/short
    // transport results are explicitly injected, not native kernel observations.
    for child in [false, true] {
        for kind in 13..=16 {
            case(child, kind).await;
        }
    }
}
