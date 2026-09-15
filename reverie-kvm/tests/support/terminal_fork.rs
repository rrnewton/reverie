use std::cell::RefCell;
use std::sync::Arc;
use std::time::Duration;
use std::time::Instant;

use super::*;

#[derive(Clone, Debug)]
struct Event {
    kind: &'static str,
    pid: i32,
    value: i64,
}

#[derive(Debug, Default)]
struct State {
    events: Vec<Event>,
    parent_ready: bool,
    release_child: bool,
    global_dropped: bool,
}

#[derive(Debug, Default)]
struct Control {
    state: Mutex<State>,
    changed: Condvar,
}

static CONTROL: Mutex<Option<Arc<Control>>> = Mutex::new(None);

fn control() -> Arc<Control> {
    CONTROL.lock().unwrap().as_ref().unwrap().clone()
}

impl Control {
    fn record(&self, kind: &'static str, pid: i32, value: i64) {
        self.state
            .lock()
            .unwrap()
            .events
            .push(Event { kind, pid, value });
        self.changed.notify_all();
    }

    async fn wait_for(&self, condition: impl Fn(&State) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(5);
        futures::future::poll_fn(|cx| {
            let state = self.state.lock().unwrap();
            assert!(
                Instant::now() < deadline,
                "child progress timed out: {state:?}"
            );
            if condition(&state) {
                std::task::Poll::Ready(())
            } else {
                cx.waker().wake_by_ref();
                std::task::Poll::Pending
            }
        })
        .await;
    }
}

struct HostExit(RefCell<Option<(Arc<Control>, i32)>>);
impl Drop for HostExit {
    fn drop(&mut self) {
        if let Some((control, pid)) = self.0.get_mut().take() {
            control.record("host-exit", pid, 0);
        }
    }
}
thread_local! {
    // A child reaches this TLS destructor only after its complete backend
    // closure, including the global child-wait callback, has returned.
    static HOST_EXIT: HostExit = const { HostExit(RefCell::new(None)) };
}

#[derive(Debug, Default)]
struct Log {
    control: Arc<Control>,
    mode: u8,
}
impl Drop for Log {
    fn drop(&mut self) {
        self.control.state.lock().unwrap().global_dropped = true;
        self.control.changed.notify_all();
    }
}
#[reverie::global_tool]
impl GlobalTool for Log {
    type Request = ();
    type Response = ();
    type Config = u8;
    async fn init_global_state(mode: &u8) -> Self {
        Self {
            control: control(),
            mode: *mode,
        }
    }
    async fn receive_rpc(&self, _: Pid, _: ()) {}
    async fn on_backend_child_wait_event(
        &self,
        event: BackendChildWaitEvent,
    ) -> Result<(), reverie::Error> {
        assert_eq!(event.parent.as_raw(), 1);
        assert_eq!(
            event.state,
            BackendChildWaitState::Exited {
                status: ExitStatus::SUCCESS,
                waitable: true,
            }
        );
        self.control.record("wait-event", event.child.as_raw(), 0);
        if self.mode == 2 {
            return Err(if event.child.as_raw() == 2 {
                Errno::EIO
            } else {
                Errno::E2BIG
            }
            .into());
        }
        Ok(())
    }
}

#[derive(Default)]
struct ForkTool {
    pid: i32,
    mode: u8,
    control: Arc<Control>,
}
impl Drop for ForkTool {
    fn drop(&mut self) {
        self.control.record("tool-drop", self.pid, 0);
    }
}
#[reverie::tool]
impl Tool for ForkTool {
    type GlobalState = Log;
    type ThreadState = (i32, bool);
    fn new(pid: Pid, mode: &u8) -> Self {
        Self {
            pid: pid.as_raw(),
            mode: *mode,
            control: control(),
        }
    }
    fn subscriptions(_: &u8) -> Subscription {
        let mut result = Subscription::none();
        result.syscalls([Sysno::getpid, Sysno::gettid, Sysno::write]);
        result
    }
    fn init_thread_state(
        &self,
        tid: Pid,
        _: Option<(Pid, &Self::ThreadState)>,
    ) -> Self::ThreadState {
        (tid.as_raw(), false)
    }
    async fn handle_thread_start<G: Guest<Self>>(
        &self,
        guest: &mut G,
    ) -> Result<(), reverie::Error> {
        assert_eq!(guest.thread_state(), &(self.pid, false));
        guest.thread_state_mut().1 = true;
        self.control.record("start", self.pid, unsafe {
            libc::syscall(libc::SYS_gettid)
        });
        HOST_EXIT.with(|exit| {
            assert!(
                exit.0
                    .borrow_mut()
                    .replace((self.control.clone(), self.pid))
                    .is_none()
            );
        });
        Ok(())
    }
    async fn handle_syscall_event<G: Guest<Self>>(
        &self,
        guest: &mut G,
        syscall: Syscall,
    ) -> Result<i64, reverie::Error> {
        assert_eq!(guest.thread_state(), &(self.pid, true));
        if syscall.number() == Sysno::getpid {
            assert_eq!(self.pid, 1);
            let count = if self.mode >= 2 { 2 } else { 1 };
            let mut children = Vec::new();
            for expected in 2..2 + count {
                let child = guest.inject(Fork::new()).await?;
                assert_eq!(child, expected);
                children.push(child as i32);
            }
            if self.mode == 2 {
                self.control
                    .wait_for(|state| {
                        children.iter().all(|pid| {
                            state
                                .events
                                .iter()
                                .any(|event| event.kind == "host-exit" && event.pid == *pid)
                        })
                    })
                    .await;
                for child in &children {
                    // Completion is already published and the worker has exited.
                    // This actual wait moves its handle into completed_processes.
                    let wait = Syscall::from_raw(
                        Sysno::wait4,
                        SyscallArgs::new(*child as usize, 0, libc::WNOHANG as usize, 0, 0, 0),
                    );
                    assert_eq!(guest.inject(wait).await?, i64::from(*child));
                    self.control.record("wait-collected", *child, 0);
                }
            } else {
                let blocked = *children.last().unwrap();
                self.control
                    .wait_for(|state| {
                        state
                            .events
                            .iter()
                            .any(|event| event.kind == "blocked" && event.pid == blocked)
                            && (self.mode < 3
                                || state
                                    .events
                                    .iter()
                                    .any(|event| event.kind == "host-exit" && event.pid == 2))
                    })
                    .await;
            }
            {
                let mut state = self.control.state.lock().unwrap();
                state.parent_ready = true;
                state.events.push(Event {
                    kind: "parent-ready",
                    pid: self.pid,
                    value: i64::from(self.mode),
                });
                self.control.changed.notify_all();
            }
            if matches!(self.mode, 1 | 5) {
                return Ok(i64::from(children[0]));
            }
            // The same callback has registered its children and now terminates.
            guest.cancel_current_thread().await;
        }
        if syscall.number() == Sysno::gettid {
            assert_ne!(self.pid, 1);
            if self.mode != 2 && (self.mode < 3 || self.pid == 3) {
                self.control.record("blocked", self.pid, 0);
                let state = self.control.state.lock().unwrap();
                let (state, timeout) = self
                    .control
                    .changed
                    .wait_timeout_while(state, Duration::from_secs(5), |state| !state.release_child)
                    .unwrap();
                assert!(!timeout.timed_out(), "child release timed out: {state:?}");
            }
            return Ok(i64::from(self.pid));
        }
        assert_eq!(syscall.number(), Sysno::write);
        let args = syscall.into_parts().1;
        let mut bytes = vec![0; args.arg2];
        guest.memory().read_exact(
            reverie::syscalls::Addr::from_raw(args.arg1).unwrap(),
            &mut bytes,
        )?;
        let expected: &[u8] = if self.pid == 1 {
            b"parent\n"
        } else if args.arg0 == 1 {
            b"child\n"
        } else {
            b"child stderr\n"
        };
        assert_eq!(bytes, expected);
        let result = guest.inject(syscall).await?;
        assert_eq!(result, expected.len() as i64);
        self.control.record("write", self.pid, args.arg0 as i64);
        Ok(result)
    }
    async fn on_exit_thread<G: GlobalRPC<Log>>(
        &self,
        tid: Pid,
        _: &G,
        state: Self::ThreadState,
        status: ExitStatus,
    ) -> Result<(), reverie::Error> {
        assert_eq!(state, (self.pid, true));
        assert_eq!(tid.as_raw(), self.pid);
        assert_eq!(status, ExitStatus::SUCCESS);
        self.control.record("thread-exit", self.pid, 0);
        if self.mode == 4 && self.pid == 1 {
            return Err(Errno::ENOSPC.into());
        }
        Ok(())
    }
    async fn on_exit_process<G: GlobalRPC<Log>>(
        self,
        pid: Pid,
        _: &G,
        status: ExitStatus,
    ) -> Result<(), reverie::Error> {
        assert_eq!(pid.as_raw(), self.pid);
        assert_eq!(status, ExitStatus::SUCCESS);
        self.control.record("process-exit", self.pid, 0);
        if self.mode >= 3 && self.pid == 2 {
            return Err(Errno::EIO.into());
        }
        if self.mode == 4 {
            return Err(if self.pid == 1 {
                Errno::EACCES
            } else {
                Errno::E2BIG
            }
            .into());
        }
        Ok(())
    }
}

fn guest_program() -> Vec<u8> {
    fn write(code: &mut Vec<u8>, fd: u32, message: &[u8]) -> (usize, Vec<u8>) {
        code.extend_from_slice(&[0xb8, 1, 0, 0, 0, 0xbf]);
        code.extend_from_slice(&fd.to_le_bytes());
        code.extend_from_slice(&[0x48, 0xbe]);
        let address = code.len();
        code.extend_from_slice(&[0; 8]);
        code.push(0xba);
        code.extend_from_slice(&(message.len() as u32).to_le_bytes());
        code.extend_from_slice(&[0x0f, 0x05]);
        (address, message.to_vec())
    }
    fn exit(code: &mut Vec<u8>) {
        code.extend_from_slice(&[0xb8, 0xe7, 0, 0, 0, 0x31, 0xff, 0x0f, 0x05, 0x0f, 0x0b]);
    }
    let mut code = vec![
        0xb8, 0x27, 0, 0, 0, 0x0f, 0x05, 0x85, 0xc0, 0x0f, 0x85, 0, 0, 0, 0,
    ];
    let parent_jump = 11;
    code.extend_from_slice(&[0xb8, 0xba, 0, 0, 0, 0x0f, 0x05]);
    let stdout = write(&mut code, 1, b"child\n");
    let stderr = write(&mut code, 2, b"child stderr\n");
    exit(&mut code);
    let parent = code.len();
    code[parent_jump..parent_jump + 4].copy_from_slice(
        &i32::try_from(parent - (parent_jump + 4))
            .unwrap()
            .to_le_bytes(),
    );
    let parent_output = write(&mut code, 1, b"parent\n");
    exit(&mut code);
    for (operand, bytes) in [stdout, stderr, parent_output] {
        let address = LOAD_ADDRESS + code.len() as u64;
        code[operand..operand + 8].copy_from_slice(&address.to_le_bytes());
        code.extend_from_slice(&bytes);
    }
    static_elf(&code)
}

fn task_virtual_memory_size(tid: i64) -> Option<i64> {
    let stat = match std::fs::read_to_string(format!("/proc/self/task/{tid}/stat")) {
        Ok(stat) => stat,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return None,
        Err(error) => panic!("cannot inspect host thread {tid}: {error}"),
    };
    // The final ')' ends comm; vsize is field 23, twenty fields after state.
    Some(
        stat.rsplit_once(") ")
            .unwrap()
            .1
            .split_whitespace()
            .nth(20)
            .unwrap()
            .parse()
            .unwrap(),
    )
}

fn bounded(test: &str) -> bool {
    if !kvm_available(test) {
        return false;
    }
    if std::env::var("REVERIE_LEADER_EXEC_CHILD").as_deref() != Ok(test) {
        // Inherit stdout/stderr so successful child observations remain in the
        // retained Cargo log, under the same bounded subprocess convention.
        let status = std::process::Command::new("timeout")
            .args(["--kill-after=2s", "30s"])
            .arg(std::env::current_exe().unwrap())
            .args(["--exact", test, "--nocapture"])
            .env("REVERIE_LEADER_EXEC_CHILD", test)
            .status()
            .unwrap();
        assert!(status.success(), "{test}: status={status:?}");
        return false;
    }
    true
}

fn snapshot_at_return(control: &Control) -> (Vec<Event>, bool) {
    let state = control.state.lock().unwrap();
    let mut events = state.events.clone();
    for event in &state.events {
        if event.kind == "start"
            && event.pid != 1
            && let Some(bytes) = task_virtual_memory_size(event.value)
        {
            events.push(Event {
                kind: "host-memory-at-return",
                pid: event.pid,
                value: bytes,
            });
        }
    }
    let result = (events, state.global_dropped);
    drop(state);
    result
}

fn run_case(test: &str, mode: u8) {
    if !bounded(test) {
        return;
    }
    let control = Arc::new(Control::default());
    *CONTROL.lock().unwrap() = Some(control.clone());
    let worker_control = control.clone();
    let (sender, receiver) = std::sync::mpsc::channel();
    let worker = std::thread::spawn(move || {
        let mut backend = KvmBackend::new(MEMORY_SIZE).unwrap();
        backend
            .install_static_elf(&guest_program(), "/bin/terminal-fork-test")
            .unwrap();
        let result =
            futures::executor::block_on(backend.run_static_elf_with_tool::<ForkTool>(mode, true));
        let at_return = snapshot_at_return(&worker_control);
        sender.send((result, at_return)).unwrap();
    });
    let state = control.state.lock().unwrap();
    let (state, timeout) = control
        .changed
        .wait_timeout_while(state, Duration::from_secs(5), |state| !state.parent_ready)
        .unwrap();
    assert!(
        !timeout.timed_out(),
        "parent never reached cancellation/exit: {state:?}"
    );
    if mode != 2 {
        let child = if mode < 3 { 2 } else { 3 };
        let tid = state
            .events
            .iter()
            .find(|event| event.kind == "start" && event.pid == child)
            .unwrap()
            .value;
        assert!(
            task_virtual_memory_size(tid).is_some_and(|bytes| bytes > 0),
            "the blocked-worker control must observe its live host address space"
        );
    }
    drop(state);
    // Except for the deliberately completed-child case, the child is held by
    // an explicit gate. No public result is permitted before that release.
    let early = if mode == 2 {
        None
    } else {
        receiver.recv_timeout(Duration::from_millis(100)).ok()
    };
    {
        let mut state = control.state.lock().unwrap();
        state.release_child = true;
        control.changed.notify_all();
    }
    let returned_early = early.is_some();
    let (result, (at_return, global_dropped)) =
        early.unwrap_or_else(|| receiver.recv_timeout(Duration::from_secs(5)).unwrap());
    worker.join().unwrap();
    // Clean up even a broken baseline that detached its child before asserting.
    let child_count = if mode >= 2 { 2 } else { 1 };
    let state = control.state.lock().unwrap();
    let (state, timeout) = control
        .changed
        .wait_timeout_while(state, Duration::from_secs(5), |state| {
            state
                .events
                .iter()
                .filter(|event| event.kind == "host-exit" && event.pid != 1)
                .count()
                != child_count
        })
        .unwrap();
    assert!(
        !timeout.timed_out(),
        "fixture could not finish detached children: {state:?}"
    );
    drop(state);
    eprintln!(
        "terminal fork mode={mode} early={returned_early} result={result:?} at_return={at_return:?} global_dropped={global_dropped}"
    );
    assert!(
        !returned_early,
        "public API returned while an owned child was still blocked"
    );
    for pid in 1..=child_count as i32 + 1 {
        for kind in ["start", "thread-exit", "process-exit", "tool-drop"] {
            assert_eq!(
                at_return
                    .iter()
                    .filter(|event| event.pid == pid && event.kind == kind)
                    .count(),
                1,
                "each owned state must be consumed once before return: pid={pid} kind={kind} events={at_return:?}"
            );
        }
        if pid != 1 {
            // A successfully joined native pthread can briefly remain in
            // /proc during kernel exit bookkeeping, after releasing its mm.
            // Any remaining entry must have no userspace address space; TLS
            // completion and owned-state destruction are required separately.
            assert!(
                !at_return.iter().any(|event| event.pid == pid
                    && event.kind == "host-memory-at-return"
                    && event.value != 0),
                "child retained a host address space after API return: {at_return:?}"
            );
            assert_eq!(
                at_return
                    .iter()
                    .filter(|event| event.pid == pid && event.kind == "host-exit")
                    .count(),
                1,
                "child host worker survived API return: {at_return:?}"
            );
            for fd in [1, 2] {
                assert_eq!(
                    at_return
                        .iter()
                        .filter(|event| event.pid == pid
                            && event.kind == "write"
                            && event.value == fd)
                        .count(),
                    1
                );
            }
        }
    }
    if mode < 2 {
        let (log, status, stdout, stderr) = result.unwrap();
        assert_eq!(status, 0);
        assert_eq!(
            stdout,
            if mode == 1 {
                b"parent\nchild\n".as_slice()
            } else {
                b"child\n".as_slice()
            }
        );
        assert_eq!(stderr, b"child stderr\n");
        assert!(!global_dropped);
        drop(log);
    } else {
        let error = result.unwrap_err().to_string();
        for (name, count) in [
            ("EIO", 1),
            ("E2BIG", usize::from(matches!(mode, 2 | 4))),
            ("ENOSPC", usize::from(mode == 4)),
            ("EACCES", usize::from(mode == 4)),
        ] {
            assert_eq!(
                error.matches(name).count(),
                count,
                "original errors must survive exactly once: {error}"
            );
        }
        assert!(global_dropped, "global Tool state survived error return");
        if mode == 2 {
            assert_eq!(
                at_return
                    .iter()
                    .filter(|event| event.kind == "wait-collected")
                    .count(),
                2
            );
        }
    }
    assert!(control.state.lock().unwrap().global_dropped);
    *CONTROL.lock().unwrap() = None;
}

#[test]
fn cancellation_joins_live_fork_and_captures_output() {
    run_case(
        "terminal_fork::cancellation_joins_live_fork_and_captures_output",
        0,
    );
}
#[test]
fn ordinary_exit_joins_live_fork_and_captures_output() {
    run_case(
        "terminal_fork::ordinary_exit_joins_live_fork_and_captures_output",
        1,
    );
}
#[test]
fn cancellation_preserves_completed_child_errors() {
    run_case(
        "terminal_fork::cancellation_preserves_completed_child_errors",
        2,
    );
}
#[test]
fn cancellation_finishes_later_children_after_first_child_error() {
    run_case(
        "terminal_fork::cancellation_finishes_later_children_after_first_child_error",
        3,
    );
}
#[test]
fn cancellation_consumes_hooks_and_preserves_all_errors() {
    run_case(
        "terminal_fork::cancellation_consumes_hooks_and_preserves_all_errors",
        4,
    );
}
#[test]
fn ordinary_exit_finishes_later_children_after_first_child_error() {
    run_case(
        "terminal_fork::ordinary_exit_finishes_later_children_after_first_child_error",
        5,
    );
}

#[derive(Default)]
struct ExecForkTool {
    pid: i32,
    mode: u8,
    control: Arc<Control>,
}
impl Drop for ExecForkTool {
    fn drop(&mut self) {
        self.control.record("tool-drop", self.pid, 0);
    }
}
#[reverie::tool]
impl Tool for ExecForkTool {
    type GlobalState = Log;
    type ThreadState = (i32, bool);
    fn new(pid: Pid, mode: &u8) -> Self {
        Self {
            pid: pid.as_raw(),
            mode: *mode,
            control: control(),
        }
    }
    fn subscriptions(mode: &u8) -> Subscription {
        let mut result = Subscription::all_syscalls();
        if mode % 2 == 1 {
            result.disable_syscalls([Sysno::execve, Sysno::execveat]);
        }
        result
    }
    fn init_thread_state(
        &self,
        tid: Pid,
        _: Option<(Pid, &Self::ThreadState)>,
    ) -> Self::ThreadState {
        (tid.as_raw(), false)
    }
    async fn handle_thread_start<G: Guest<Self>>(
        &self,
        guest: &mut G,
    ) -> Result<(), reverie::Error> {
        let tid = guest.tid().as_raw();
        assert_eq!(guest.thread_state(), &(tid, false));
        guest.thread_state_mut().1 = true;
        self.control
            .record("start", tid, unsafe { libc::syscall(libc::SYS_gettid) });
        HOST_EXIT.with(|exit| {
            assert!(
                exit.0
                    .borrow_mut()
                    .replace((self.control.clone(), tid))
                    .is_none()
            );
        });
        Ok(())
    }
    async fn handle_post_exec<G: Guest<Self>>(&self, guest: &mut G) -> Result<(), Errno> {
        self.control.record("post-exec", guest.tid().as_raw(), 0);
        Ok(())
    }
    async fn handle_syscall_event<G: Guest<Self>>(
        &self,
        guest: &mut G,
        syscall: Syscall,
    ) -> Result<i64, reverie::Error> {
        if syscall.number() == Sysno::fork {
            let child = guest.inject(syscall).await?;
            assert_eq!(child, 2);
            return Ok(child);
        }
        let args = syscall.into_parts().1;
        if syscall.number() == Sysno::gettid && self.pid == 2 && args.arg0 == 0x74666f72 {
            self.control.record("blocked", 2, 0);
            let state = self.control.state.lock().unwrap();
            let (state, timeout) = self
                .control
                .changed
                .wait_timeout_while(state, Duration::from_secs(5), |state| !state.release_child)
                .unwrap();
            assert!(
                !timeout.timed_out(),
                "fork child release timed out: {state:?}"
            );
            return Ok(2);
        }
        if syscall.number() == Sysno::getpid && args.arg0 == 0x74666f72 {
            assert_eq!(guest.tid().as_raw(), 1);
            self.control
                .wait_for(|state| {
                    state
                        .events
                        .iter()
                        .any(|event| event.kind == "blocked" && event.pid == 2)
                        && state
                            .events
                            .iter()
                            .any(|event| event.kind == "start" && event.pid == 3)
                })
                .await;
            return Ok(1);
        }
        if syscall.number() == Sysno::write {
            assert_eq!(
                self.pid, 2,
                "failed exec must not enter the replacement image"
            );
            let mut bytes = vec![0; args.arg2];
            guest.memory().read_exact(
                reverie::syscalls::Addr::from_raw(args.arg1).unwrap(),
                &mut bytes,
            )?;
            let expected: &[u8] = if args.arg0 == 1 {
                b"child\n"
            } else {
                b"child stderr\n"
            };
            assert_eq!(bytes, expected);
            let result = guest.inject(syscall).await?;
            assert_eq!(result, expected.len() as i64);
            self.control.record("write", self.pid, args.arg0 as i64);
            return Ok(result);
        }
        guest.tail_inject(syscall).await
    }
    async fn on_exit_thread<G: GlobalRPC<Log>>(
        &self,
        tid: Pid,
        _: &G,
        state: Self::ThreadState,
        status: ExitStatus,
    ) -> Result<(), reverie::Error> {
        let tid = tid.as_raw();
        assert_eq!(state, (tid, true));
        assert_eq!(
            status,
            if tid == 1 {
                ExitStatus::Exited(255)
            } else {
                ExitStatus::SUCCESS
            }
        );
        self.control.record("thread-exit", tid, 0);
        if tid == 3 {
            // This is the exec teardown failure itself, after its sibling
            // cancellation has begun while the fork child remains blocked.
            self.control.state.lock().unwrap().parent_ready = true;
            self.control.changed.notify_all();
            return Err(Errno::EIO.into());
        }
        if tid == 1 && self.mode >= 8 {
            return Err(Errno::ENOSPC.into());
        }
        Ok(())
    }
    async fn on_exit_process<G: GlobalRPC<Log>>(
        self,
        pid: Pid,
        _: &G,
        status: ExitStatus,
    ) -> Result<(), reverie::Error> {
        assert_eq!(pid.as_raw(), self.pid);
        assert_eq!(
            status,
            if self.pid == 1 {
                ExitStatus::Exited(255)
            } else {
                ExitStatus::SUCCESS
            }
        );
        self.control.record("process-exit", self.pid, 0);
        if self.mode >= 8 {
            return Err(if self.pid == 1 {
                Errno::EACCES
            } else {
                Errno::E2BIG
            }
            .into());
        }
        Ok(())
    }
}

const EXEC_FORK_GUEST: &str = r#"
#define _GNU_SOURCE
#include <pthread.h>
#include <sched.h>
#include <stdatomic.h>
#include <sys/syscall.h>
#include <unistd.h>
static _Atomic int entered;
static void *worker(void *unused) {
  (void)unused;
  atomic_store(&entered, 1);
  for (;;) sched_yield();
  return NULL;
}
int main(int argc, char **argv) {
  if (argc == 2) { write(1, "replacement\n", 12); return 0; }
  long child = syscall(SYS_fork);
  if (child < 0) return 20;
  if (!child) {
    syscall(SYS_gettid, 0x74666f72);
    if (write(1, "child\n", 6) != 6) return 21;
    if (write(2, "child stderr\n", 13) != 13) return 22;
    syscall(SYS_exit_group, 0);
  }
  pthread_t thread;
  if (pthread_create(&thread, NULL, worker, NULL)) return 23;
  while (!atomic_load(&entered)) sched_yield();
  syscall(SYS_getpid, 0x74666f72);
  char *next[] = {argv[0], "replacement", NULL};
  execv(argv[0], next);
  return 24;
}
"#;

fn run_exec_case(test: &str, mode: u8) {
    if !bounded(test) {
        return;
    }
    let directory = TestDirectory::new();
    let executable = compile_c_program(&directory.0, "terminal-fork-exec", EXEC_FORK_GUEST);
    let control = Arc::new(Control::default());
    *CONTROL.lock().unwrap() = Some(control.clone());
    let worker_control = control.clone();
    let cwd = directory.0.clone();
    let (sender, receiver) = std::sync::mpsc::channel();
    let worker = std::thread::spawn(move || {
        let mut backend = KvmBackend::new(256 * 1024 * 1024).unwrap();
        backend
            .install_static_elf_file_with_context(
                std::fs::File::open(&executable).unwrap(),
                &[executable.to_str().unwrap()],
                &[],
                &cwd,
            )
            .unwrap();
        let result = futures::executor::block_on(
            backend.run_static_elf_with_tool::<ExecForkTool>(mode, true),
        );
        sender
            .send((result, snapshot_at_return(&worker_control)))
            .unwrap();
    });
    let state = control.state.lock().unwrap();
    let (state, timeout) = control
        .changed
        .wait_timeout_while(state, Duration::from_secs(5), |state| !state.parent_ready)
        .unwrap();
    assert!(!timeout.timed_out(), "parent never reached exec: {state:?}");
    let child_tid = state
        .events
        .iter()
        .find(|event| event.kind == "start" && event.pid == 2)
        .unwrap()
        .value;
    assert!(task_virtual_memory_size(child_tid).is_some_and(|bytes| bytes > 0));
    drop(state);
    let early = receiver.recv_timeout(Duration::from_millis(100)).ok();
    control.state.lock().unwrap().release_child = true;
    control.changed.notify_all();
    let returned_early = early.is_some();
    let (result, (events, global_dropped)) =
        early.unwrap_or_else(|| receiver.recv_timeout(Duration::from_secs(5)).unwrap());
    worker.join().unwrap();
    let state = control.state.lock().unwrap();
    let (state, timeout) = control
        .changed
        .wait_timeout_while(state, Duration::from_secs(5), |state| {
            state
                .events
                .iter()
                .filter(|event| event.kind == "host-exit" && event.pid != 1)
                .count()
                != 2
        })
        .unwrap();
    assert!(
        !timeout.timed_out(),
        "fixture could not finish child workers: {state:?}"
    );
    drop(state);
    eprintln!(
        "terminal fork exec mode={mode} early={returned_early} result={result:?} at_return={events:?} global_dropped={global_dropped}"
    );
    assert!(
        !returned_early,
        "failed exec returned while an owned fork child was still blocked"
    );
    assert!(global_dropped, "failed exec retained global Tool state");
    for tid in [1, 2, 3] {
        for kind in ["start", "thread-exit"] {
            assert_eq!(
                events
                    .iter()
                    .filter(|event| event.pid == tid && event.kind == kind)
                    .count(),
                1,
                "exactly one consuming hook for each thread: {events:?}"
            );
        }
        if tid != 1 {
            assert_eq!(
                events
                    .iter()
                    .filter(|event| event.pid == tid && event.kind == "host-exit")
                    .count(),
                1
            );
            assert!(!events.iter().any(|event| event.pid == tid
                && event.kind == "host-memory-at-return"
                && event.value != 0));
        }
    }
    for pid in [1, 2] {
        for kind in ["process-exit", "tool-drop"] {
            assert_eq!(
                events
                    .iter()
                    .filter(|event| event.pid == pid && event.kind == kind)
                    .count(),
                1,
                "each process state must be consumed: {events:?}"
            );
        }
    }
    for fd in [1, 2] {
        assert_eq!(
            events
                .iter()
                .filter(|event| event.kind == "write" && event.pid == 2 && event.value == fd)
                .count(),
            1
        );
    }
    assert_eq!(
        events
            .iter()
            .filter(|event| event.kind == "post-exec" && event.pid == 1)
            .count(),
        1,
        "the failed exec must not reach a replacement post-exec hook"
    );
    let error = result.unwrap_err().to_string();
    for name in ["EIO", "E2BIG", "ENOSPC", "EACCES"] {
        assert_eq!(
            error.matches(name).count(),
            usize::from(name == "EIO" || mode >= 8),
            "{error}"
        );
    }
    if mode < 8 {
        assert_eq!(
            error,
            "unexpected vCPU exit: KVM worker cleanup failed: thread 3: Reverie tool failed: -5 EIO (I/O error)"
        );
    }
    *CONTROL.lock().unwrap() = None;
}

#[test]
fn injected_exec_failure_joins_live_fork() {
    run_exec_case("terminal_fork::injected_exec_failure_joins_live_fork", 6);
}
#[test]
fn backend_exec_failure_joins_live_fork() {
    run_exec_case("terminal_fork::backend_exec_failure_joins_live_fork", 7);
}
#[test]
fn injected_exec_failure_preserves_child_and_owner_errors() {
    run_exec_case(
        "terminal_fork::injected_exec_failure_preserves_child_and_owner_errors",
        8,
    );
}
#[test]
fn backend_exec_failure_preserves_child_and_owner_errors() {
    run_exec_case(
        "terminal_fork::backend_exec_failure_preserves_child_and_owner_errors",
        9,
    );
}
