//! Backend lifecycle controls. These observe real callback ordering, not native
//! selected-file or capability evidence. Native tests require the maintained owner.
use std::sync::Arc;
use std::sync::Mutex as SyncMutex;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;

use serde::Deserialize;
use serde::Serialize;

use super::*;

#[derive(Default)]
struct Log {
    ready: SyncMutex<Vec<(i32, usize, usize)>>,
    dispatched: AtomicUsize,
    failures: AtomicUsize,
}
#[derive(Serialize, Deserialize)]
struct Seen {
    pointer: usize,
    phase: usize,
}
#[reverie::global_tool]
impl GlobalTool for Log {
    type Config = ();
    type Request = Seen;
    type Response = ();
    async fn receive_rpc(&self, from: Pid, seen: Seen) {
        assert_eq!(
            self.ready
                .lock()
                .unwrap()
                .iter()
                .rev()
                .find(|row| row.0 == from.as_raw())
                .copied(),
            Some((from.as_raw(), seen.pointer, seen.phase))
        );
        self.dispatched.fetch_add(1, Ordering::SeqCst);
    }
    fn report_backend_failure(&self, _failure: reverie::BackendFailure) {
        self.failures.fetch_add(1, Ordering::SeqCst);
    }
}
#[derive(Default)]
struct ReadyTool;
#[reverie::tool]
impl Tool for ReadyTool {
    type GlobalState = Log;
    type ThreadState = Arc<AtomicUsize>;
    fn subscriptions(_: &()) -> Subscription {
        let mut events = Subscription::none();
        events.syscalls([Sysno::getpgid]);
        events
    }
    async fn handle_thread_start<G: Guest<Self>>(
        &self,
        guest: &mut G,
    ) -> Result<(), reverie::Error> {
        // Deliberately replace the constructor's object: the hook must observe
        // the actual completed Tool state, not a constructor snapshot.
        *guest.thread_state_mut() = Arc::new(AtomicUsize::new(1));
        Ok(())
    }
    async fn handle_post_exec<G: Guest<Self>>(&self, guest: &mut G) -> Result<(), Errno> {
        *guest.thread_state_mut() = Arc::new(AtomicUsize::new(2));
        Ok(())
    }
    fn on_thread_state_ready(
        &self,
        tid: Tid,
        global: &Log,
        state: &Self::ThreadState,
    ) -> Result<(), reverie::Error> {
        let phase = state.load(Ordering::SeqCst);
        if phase == 99 {
            return Err(reverie::Error::Tool(anyhow::anyhow!(
                "ready callback refused"
            )));
        }
        assert!(phase == 1 || phase == 2);
        global
            .ready
            .lock()
            .unwrap()
            .push((tid.as_raw(), Arc::as_ptr(state) as usize, phase));
        Ok(())
    }
    async fn handle_syscall_event<G: Guest<Self>>(
        &self,
        guest: &mut G,
        call: Syscall,
    ) -> Result<i64, reverie::Error> {
        guest
            .send_rpc(Seen {
                pointer: Arc::as_ptr(guest.thread_state()) as usize,
                phase: guest.thread_state().load(Ordering::SeqCst),
            })
            .await;
        Ok(guest.inject(call).await?)
    }
}

#[test]
fn native_ready_observation_precedes_root_fork_and_clone_files_dispatch() {
    let (output, log) = crate::testing::test_fn::<ReadyTool, _>(|| unsafe {
        assert!(libc::syscall(libc::SYS_getpgid, 0) > 0);
        for shared in [false, true] {
            let child = if shared {
                libc::syscall(
                    libc::SYS_clone,
                    libc::CLONE_FILES | libc::SIGCHLD,
                    0usize,
                    0usize,
                    0usize,
                    0usize,
                ) as libc::pid_t
            } else {
                libc::fork()
            };
            assert!(child >= 0);
            if child == 0 {
                let result = libc::syscall(libc::SYS_getpgid, 0);
                libc::_exit(if result > 0 { 0 } else { 31 });
            }
            let mut status = -1;
            assert_eq!(libc::waitpid(child, &mut status, 0), child);
            assert_eq!(status, 0);
        }
    })
    .unwrap();
    assert_eq!(output.status, ExitStatus::Exited(0));
    let ready = log.ready.lock().unwrap();
    assert_eq!(ready.len(), 3);
    assert!(ready.iter().all(|row| row.2 == 1));
    let owners: std::collections::BTreeSet<_> = ready.iter().map(|row| row.0).collect();
    assert_eq!(owners.len(), 3);
    assert_eq!(log.dispatched.load(Ordering::SeqCst), 3);
    assert_eq!(log.failures.load(Ordering::SeqCst), 0);
}
#[test]
fn native_ready_observation_uses_post_exec_replacement_object() {
    let (output, log) = crate::testing::test_cmd::<ReadyTool>("/bin/true", &[]).unwrap();
    assert_eq!(output.status, ExitStatus::Exited(0));
    let ready = log.ready.lock().unwrap();
    assert_eq!(ready.len(), 2);
    assert_eq!((ready[0].2, ready[1].2), (1, 2));
    assert_eq!(ready[0].0, ready[1].0);
    assert_ne!(ready[0].1, ready[1].1);
    assert_eq!(log.failures.load(Ordering::SeqCst), 0);
}
#[test]
fn native_failed_exec_keeps_original_ready_association() {
    let (output, log) = crate::testing::test_fn::<ReadyTool, _>(|| unsafe {
        let path = c"/hermit-ready-control-no-such-executable";
        assert_eq!(
            libc::syscall(
                libc::SYS_execve,
                path.as_ptr(),
                std::ptr::null::<*const libc::c_char>(),
                std::ptr::null::<*const libc::c_char>()
            ),
            -1
        );
        assert_eq!(*libc::__errno_location(), libc::ENOENT);
        assert!(libc::syscall(libc::SYS_getpgid, 0) > 0);
    })
    .unwrap();
    assert_eq!(output.status, ExitStatus::Exited(0));
    let ready = log.ready.lock().unwrap();
    assert_eq!(ready.len(), 1);
    assert_eq!(ready[0].2, 1);
    assert_eq!(log.dispatched.load(Ordering::SeqCst), 1);
    assert_eq!(log.failures.load(Ordering::SeqCst), 0);
}
#[test]
fn ready_callback_failure_returns_eproto_without_dispatch() {
    // Helper-level check of the dynamic LiteInst form only: no native guest or
    // resume is involved. The native refusal tests below cover the FatalSession
    // route and the stop before resume at both ordinary call sites.
    let log = Log::default();
    let state = Arc::new(AtomicUsize::new(99));
    assert_eq!(
        observe_ready_thread_state(&ReadyTool, Pid::from_raw(7), Tid::from_raw(8), &log, &state),
        Err(Errno::EPROTO)
    );
    assert_eq!(log.failures.load(Ordering::SeqCst), 1);
    assert!(log.ready.lock().unwrap().is_empty());
    assert_eq!(log.dispatched.load(Ordering::SeqCst), 0);
    assert_eq!(state.load(Ordering::SeqCst), 99);
}

const REFUSAL: &str = "ready callback refused";
const REFUSAL_MARKER: &[u8] = b"resumed-after-refusal\n";

/// Which ordinary call site of the ready observation the Tool refuses.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
enum RefuseAt {
    #[default]
    ThreadStart,
    PostExec,
}

#[derive(Default)]
struct RefusalLog {
    ready: AtomicUsize,
    dispatched: AtomicUsize,
    failures: SyncMutex<Vec<reverie::BackendFailure>>,
}
#[reverie::global_tool]
impl GlobalTool for RefusalLog {
    type Config = RefuseAt;
    type Request = ();
    type Response = ();
    async fn receive_rpc(&self, _from: Pid, _request: ()) {
        self.dispatched.fetch_add(1, Ordering::SeqCst);
    }
    fn report_backend_failure(&self, failure: reverie::BackendFailure) {
        self.failures.lock().unwrap().push(failure);
    }
}

#[derive(Default)]
struct RefusingTool {
    at: RefuseAt,
}
#[reverie::tool]
impl Tool for RefusingTool {
    type GlobalState = RefusalLog;
    type ThreadState = usize;
    fn new(_pid: Pid, at: &RefuseAt) -> Self {
        Self { at: *at }
    }
    fn subscriptions(_: &RefuseAt) -> Subscription {
        let mut events = Subscription::none();
        events.syscalls([Sysno::getpgid]);
        events
    }
    async fn handle_thread_start<G: Guest<Self>>(
        &self,
        guest: &mut G,
    ) -> Result<(), reverie::Error> {
        *guest.thread_state_mut() = if self.at == RefuseAt::ThreadStart {
            99
        } else {
            1
        };
        Ok(())
    }
    async fn handle_post_exec<G: Guest<Self>>(&self, guest: &mut G) -> Result<(), Errno> {
        if self.at == RefuseAt::PostExec {
            *guest.thread_state_mut() = 99;
        }
        Ok(())
    }
    fn on_thread_state_ready(
        &self,
        _tid: Tid,
        global: &RefusalLog,
        state: &usize,
    ) -> Result<(), reverie::Error> {
        if *state == 99 {
            return Err(reverie::Error::Tool(anyhow::anyhow!(REFUSAL)));
        }
        global.ready.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
    async fn handle_syscall_event<G: Guest<Self>>(
        &self,
        guest: &mut G,
        call: Syscall,
    ) -> Result<i64, reverie::Error> {
        guest.send_rpc(()).await;
        Ok(guest.inject(call).await?)
    }
}

fn refusal_guest(at: RefuseAt) {
    unsafe {
        assert!(libc::syscall(libc::SYS_getpgid, 0) > 0);
        if at == RefuseAt::PostExec {
            // The replacement image prints the marker only if the backend
            // resumes it after the refused post-exec association.
            let args = [
                c"sh".as_ptr(),
                c"-c".as_ptr(),
                c"printf 'resumed-after-refusal\n'".as_ptr(),
                std::ptr::null(),
            ];
            libc::execv(c"/bin/sh".as_ptr(), args.as_ptr());
            libc::_exit(91);
        }
        assert_eq!(
            libc::write(1, REFUSAL_MARKER.as_ptr().cast(), REFUSAL_MARKER.len()),
            REFUSAL_MARKER.len() as isize
        );
        libc::_exit(0);
    }
}

async fn check_ready_refusal(at: RefuseAt) {
    let tracer =
        crate::spawn_fn_with_config::<RefusingTool, _>(move || refusal_guest(at), at, true)
            .await
            .expect("spawn ready-refusal fixture");
    let root = tracer.guest_pid();
    let termination = tracer
        .termination_handle()
        .expect("ordinary termination owner");
    let mut completion = Box::pin(tracer.wait_with_output_completion());
    let outcome = match tokio::time::timeout(std::time::Duration::from_secs(20), &mut completion)
        .await
    {
        Ok(outcome) => outcome,
        Err(_) => {
            // Rescue only: a run that never completes is the failure.
            termination.terminate(reverie::Error::Tool(anyhow::anyhow!("test deadline")));
            let rescued = tokio::time::timeout(std::time::Duration::from_secs(5), &mut completion)
                .await
                .is_ok();
            panic!("ready refusal did not complete; rescued={rescued}");
        }
    };
    let completed = match outcome {
        crate::ToolRunOutcome::Complete(completed) => completed,
        crate::ToolRunOutcome::CleanupPending(pending) => {
            panic!(
                "ready refusal left cleanup pending: {:?}",
                pending.failure()
            )
        }
        crate::ToolRunOutcome::UnsupportedBackend(_) => panic!("ordinary backend unsupported"),
    };
    let origin = reverie::BackendFailure {
        pid: root,
        tid: root,
        phase: "owned Tool state association failed",
    };
    let log = &completed.global_state;
    let failure = completed
        .result
        .as_ref()
        .expect_err("refused ready association became a guest result");
    // The refusal is the session's first failure, with the Tool's own error,
    // not a generic run-loop errno published after the fact.
    assert_eq!(failure.origin(), origin, "{failure:?}");
    assert!(
        matches!(failure.primary(), reverie::Error::Tool(error) if error.to_string() == REFUSAL),
        "Tool refusal lost: {failure:?}"
    );
    // Exactly one report, made by the FatalSession reporter.
    assert_eq!(*log.failures.lock().unwrap(), vec![origin]);
    // The guest was stopped before resume: no guest code ran after the refusal.
    let prefix = failure
        .captured_prefix()
        .expect("requested capture exists even when empty");
    assert_eq!(prefix.stdout(), b"");
    let (ready, dispatched) = match at {
        // The root never ran: neither its getpgid nor its write happened.
        RefuseAt::ThreadStart => (0, 0),
        // The original image ran its getpgid; the replacement never ran.
        RefuseAt::PostExec => (1, 1),
    };
    assert_eq!(log.ready.load(Ordering::SeqCst), ready);
    assert_eq!(log.dispatched.load(Ordering::SeqCst), dispatched);
}

#[tokio::test(flavor = "current_thread")]
async fn native_thread_start_ready_refusal_fails_session_before_resume() {
    check_ready_refusal(RefuseAt::ThreadStart).await;
}

#[tokio::test(flavor = "current_thread")]
async fn native_post_exec_ready_refusal_fails_session_before_resume() {
    check_ready_refusal(RefuseAt::PostExec).await;
}
