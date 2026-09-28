use super::*;

#[derive(Debug, Default)]
struct ExitLog(Mutex<Vec<(u8, i32, ExitStatus)>>);
#[reverie::global_tool]
impl GlobalTool for ExitLog {
    type Request = (u8, i32, ExitStatus);
    type Response = ();
    type Config = ();
    async fn receive_rpc(&self, _: Pid, event: Self::Request) {
        self.0.lock().unwrap().push(event);
    }
}

#[derive(Debug, Default)]
struct ExitTool;
#[reverie::tool]
impl Tool for ExitTool {
    type GlobalState = ExitLog;
    type ThreadState = bool;
    fn subscriptions(_: &()) -> Subscription {
        Subscription::none()
    }
    async fn handle_thread_start<G: Guest<Self>>(
        &self,
        guest: &mut G,
    ) -> Result<(), reverie::Error> {
        assert!(!guest.thread_state());
        *guest.thread_state_mut() = true;
        Ok(())
    }
    async fn on_exit_thread<G: GlobalRPC<Self::GlobalState>>(
        &self,
        tid: Pid,
        global: &G,
        started: bool,
        status: ExitStatus,
    ) -> Result<(), reverie::Error> {
        assert!(started);
        global.send_rpc((0, tid.as_raw(), status)).await;
        Ok(())
    }
    async fn on_exit_process<G: GlobalRPC<Self::GlobalState>>(
        self,
        pid: Pid,
        global: &G,
        status: ExitStatus,
    ) -> Result<(), reverie::Error> {
        global.send_rpc((1, pid.as_raw(), status)).await;
        Ok(())
    }
}

fn guest(directory: &std::path::Path, worker_group: bool) -> PathBuf {
    let source = directory.join("leader-exit.s");
    let executable = directory.join("leader-exit");
    std::fs::write(&source, include_str!("../fixtures/leader_exit.s")).unwrap();
    let output = std::process::Command::new("/usr/bin/gcc")
        .args(["-nostdlib", "-static", "-no-pie", "-Wl,--build-id=none"])
        .arg(format!(
            "-Wa,--defsym,WORKER_EXIT={}",
            if worker_group {
                libc::SYS_exit_group
            } else {
                libc::SYS_exit
            }
        ))
        .arg(&source)
        .arg("-o")
        .arg(&executable)
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    executable
}

fn run(test: &str, worker_group: bool, with_tool: bool) {
    if !leader_self_exec_bounded(test) {
        return;
    }
    let directory = TestDirectory::new();
    let executable = guest(&directory.0, worker_group);
    let native = std::process::Command::new("timeout")
        .args(["--kill-after=2s", "5s"])
        .arg(&executable)
        .output()
        .unwrap();
    assert_eq!(native.status.code(), Some(73), "{native:?}");
    assert_eq!(native.stdout, b"worker continued after leader exit\n");
    assert!(native.stderr.is_empty(), "{native:?}");
    let mut backend = KvmBackend::new(16 * 1024 * 1024).unwrap();
    backend
        .install_static_elf_file_with_context(
            std::fs::File::open(&executable).unwrap(),
            &[executable.to_str().unwrap()],
            &[],
            &directory.0,
        )
        .unwrap();
    let (status, stdout, stderr) = if with_tool {
        let (log, status, stdout, stderr) =
            futures::executor::block_on(backend.run_static_elf_with_tool::<ExitTool>((), true))
                .unwrap();
        let events = log.0.into_inner().unwrap();
        assert_eq!(
            events,
            vec![
                (0, 2, ExitStatus::Exited(73)),
                (0, 1, ExitStatus::Exited(73)),
                (1, 1, ExitStatus::Exited(73)),
            ],
            "worker hook, delayed leader hook and process hook match actual ptrace",
        );
        (status, stdout, stderr)
    } else {
        backend.run_static_elf_captured().unwrap()
    };
    assert_eq!(status, 73);
    assert_eq!(stdout, native.stdout);
    assert_eq!(stderr, native.stderr);
}

#[test]
fn raw_exit_preserves_worker_direct() {
    run(
        "leader_exit::raw_exit_preserves_worker_direct",
        false,
        false,
    );
}
#[test]
fn raw_exit_preserves_worker_tool() {
    run("leader_exit::raw_exit_preserves_worker_tool", false, true);
}
#[test]
fn later_group_exit_preserves_worker_direct() {
    run(
        "leader_exit::later_group_exit_preserves_worker_direct",
        true,
        false,
    );
}
#[test]
fn later_group_exit_preserves_worker_tool() {
    run(
        "leader_exit::later_group_exit_preserves_worker_tool",
        true,
        true,
    );
}

#[derive(Debug, Default)]
struct OrderedLog {
    events: Mutex<Vec<(u8, i32, ExitStatus)>>,
    first_exited: AtomicBool,
    second_exited: AtomicBool,
}
#[reverie::global_tool]
impl GlobalTool for OrderedLog {
    type Request = (u8, i32, ExitStatus);
    type Response = ();
    type Config = u8;
    async fn receive_rpc(&self, _: Pid, (kind, tid, status): Self::Request) {
        if kind < 2 {
            self.events.lock().unwrap().push((kind, tid, status));
            if kind == 0 && tid == 2 {
                self.first_exited.store(true, Ordering::Release);
            }
            if kind == 0 && tid == 3 {
                self.second_exited.store(true, Ordering::Release);
            }
            return;
        }
        let flag = if kind == 2 {
            &self.second_exited
        } else {
            &self.first_exited
        };
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        futures::future::poll_fn(|cx| {
            assert!(
                std::time::Instant::now() < deadline,
                "other worker exit hook did not run"
            );
            if flag.load(Ordering::Acquire) {
                std::task::Poll::Ready(())
            } else {
                cx.waker().wake_by_ref();
                std::task::Poll::Pending
            }
        })
        .await;
    }
}
#[derive(Debug, Default)]
struct OrderedTool {
    mode: u8,
}
#[reverie::tool]
impl Tool for OrderedTool {
    type GlobalState = OrderedLog;
    type ThreadState = bool;
    fn new(_: Pid, mode: &u8) -> Self {
        Self { mode: *mode }
    }
    fn subscriptions(mode: &u8) -> Subscription {
        let mut result = Subscription::none();
        if *mode == 2 {
            result.syscalls([Sysno::getpid]);
        }
        result
    }
    async fn handle_thread_start<G: Guest<Self>>(
        &self,
        guest: &mut G,
    ) -> Result<(), reverie::Error> {
        assert!(!guest.thread_state());
        *guest.thread_state_mut() = true;
        if self.mode == 1 && guest.tid().as_raw() == 3 {
            // Establish the asserted worker-hook order explicitly. Linux clear-TID
            // wakes the nested guest before its creator's consuming hook runs.
            guest.send_rpc((3, 3, ExitStatus::SUCCESS)).await;
        }
        Ok(())
    }
    async fn handle_syscall_event<G: Guest<Self>>(
        &self,
        guest: &mut G,
        call: Syscall,
    ) -> Result<i64, reverie::Error> {
        if guest.tid().as_raw() == 2 {
            guest.send_rpc((2, 2, ExitStatus::SUCCESS)).await;
        }
        Ok(guest.inject(call).await?)
    }
    async fn on_exit_thread<G: GlobalRPC<Self::GlobalState>>(
        &self,
        tid: Pid,
        global: &G,
        started: bool,
        status: ExitStatus,
    ) -> Result<(), reverie::Error> {
        assert!(started);
        global.send_rpc((0, tid.as_raw(), status)).await;
        if self.mode == 2 && tid.as_raw() == 3 {
            global.send_rpc((3, 3, status)).await;
        }
        Ok(())
    }
    async fn on_exit_process<G: GlobalRPC<Self::GlobalState>>(
        self,
        pid: Pid,
        global: &G,
        status: ExitStatus,
    ) -> Result<(), reverie::Error> {
        global.send_rpc((1, pid.as_raw(), status)).await;
        Ok(())
    }
}
fn run_pthread(test: &str, mode: &str, with_tool: bool) {
    if !leader_self_exec_bounded(test) {
        return;
    }
    let directory = TestDirectory::new();
    let executable = compile_c_program(
        &directory.0,
        "leader-pthread-exit",
        include_str!("../fixtures/leader_pthread_exit.c"),
    );
    let native = std::process::Command::new("timeout")
        .args(["--kill-after=2s", "5s"])
        .arg(&executable)
        .arg(mode)
        .output()
        .unwrap();
    assert_eq!(native.status.code(), Some(73), "{native:?}");
    assert_eq!(native.stdout, b"worker continued after leader exit\n");
    assert!(native.stderr.is_empty(), "{native:?}");
    let mut backend = KvmBackend::new(256 * 1024 * 1024).unwrap();
    backend
        .install_static_elf_file_with_context(
            std::fs::File::open(&executable).unwrap(),
            &[executable.to_str().unwrap(), mode],
            &[],
            &directory.0,
        )
        .unwrap();
    let (status, stdout, stderr) = if with_tool {
        let (log, status, stdout, stderr) = futures::executor::block_on(
            backend.run_static_elf_with_tool::<OrderedTool>(mode.parse().unwrap(), true),
        )
        .unwrap();
        let events = log.events.into_inner().unwrap();
        let expected = match mode {
            "0" => vec![
                (0, 2, ExitStatus::Exited(73)),
                (0, 1, ExitStatus::Exited(73)),
                (1, 1, ExitStatus::Exited(73)),
            ],
            "1" => vec![
                (0, 2, ExitStatus::Exited(61)),
                (0, 3, ExitStatus::Exited(73)),
                (0, 1, ExitStatus::Exited(73)),
                (1, 1, ExitStatus::Exited(73)),
            ],
            "3" => vec![
                (0, 2, ExitStatus::Exited(61)),
                (0, 3, ExitStatus::Exited(73)),
                (0, 1, ExitStatus::Exited(73)),
                (1, 1, ExitStatus::Exited(73)),
            ],
            "2" => vec![
                (0, 3, ExitStatus::Exited(61)),
                (0, 2, ExitStatus::Exited(73)),
                (0, 1, ExitStatus::Exited(73)),
                (1, 1, ExitStatus::Exited(73)),
            ],
            _ => unreachable!(),
        };
        assert_eq!(events, expected);
        (status, stdout, stderr)
    } else {
        backend.run_static_elf_captured().unwrap()
    };
    assert_eq!(status, 73);
    assert_eq!(stdout, native.stdout);
    assert_eq!(stderr, native.stderr);
}
#[test]
fn pthread_exit_preserves_worker_direct() {
    run_pthread(
        "leader_exit::pthread_exit_preserves_worker_direct",
        "0",
        false,
    );
}
#[test]
fn pthread_exit_preserves_worker_tool() {
    run_pthread("leader_exit::pthread_exit_preserves_worker_tool", "0", true);
}
#[test]
fn raw_exit_drains_nested_worker_direct() {
    run_pthread(
        "leader_exit::raw_exit_drains_nested_worker_direct",
        "1",
        false,
    );
}
#[test]
fn raw_exit_drains_nested_worker_tool() {
    run_pthread("leader_exit::raw_exit_drains_nested_worker_tool", "1", true);
}
#[test]
fn process_status_precedes_host_join_order() {
    run_pthread(
        "leader_exit::process_status_precedes_host_join_order",
        "2",
        true,
    );
}

#[derive(Default)]
struct PendingStartControl {
    entered: AtomicBool,
    release: AtomicBool,
}
static PENDING_START: Mutex<Option<std::sync::Arc<PendingStartControl>>> = Mutex::new(None);
#[derive(Debug, Default)]
struct PendingStartTool;
#[reverie::tool]
impl Tool for PendingStartTool {
    type GlobalState = ExitLog;
    type ThreadState = ();
    fn subscriptions(_: &()) -> Subscription {
        Subscription::none()
    }
    async fn handle_thread_start<G: Guest<Self>>(
        &self,
        guest: &mut G,
    ) -> Result<(), reverie::Error> {
        if guest.tid().as_raw() == 2 {
            let control = PENDING_START.lock().unwrap().as_ref().unwrap().clone();
            control.entered.store(true, Ordering::Release);
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            futures::future::poll_fn(|cx| {
                assert!(
                    std::time::Instant::now() < deadline,
                    "leader did not retire before pending start was released"
                );
                if control.release.load(Ordering::Acquire) {
                    std::task::Poll::Ready(())
                } else {
                    cx.waker().wake_by_ref();
                    std::task::Poll::Pending
                }
            })
            .await;
        }
        Ok(())
    }
    async fn on_exit_thread<G: GlobalRPC<Self::GlobalState>>(
        &self,
        tid: Pid,
        global: &G,
        _: (),
        status: ExitStatus,
    ) -> Result<(), reverie::Error> {
        global.send_rpc((0, tid.as_raw(), status)).await;
        Ok(())
    }
    async fn on_exit_process<G: GlobalRPC<Self::GlobalState>>(
        self,
        pid: Pid,
        global: &G,
        status: ExitStatus,
    ) -> Result<(), reverie::Error> {
        global.send_rpc((1, pid.as_raw(), status)).await;
        Ok(())
    }
}
#[test]
fn leader_exit_preserves_pending_worker_start_callback() {
    if !leader_self_exec_bounded("leader_exit::leader_exit_preserves_pending_worker_start_callback")
    {
        return;
    }
    let directory = TestDirectory::new();
    let executable = guest(&directory.0, false);
    let image = std::fs::read(&executable).unwrap();
    let elf = goblin::elf::Elf::parse(&image).unwrap();
    let clear_tid = elf
        .syms
        .iter()
        .find(|symbol| elf.strtab.get_at(symbol.st_name) == Some("clear_tid"))
        .unwrap()
        .st_value;
    let mut backend = KvmBackend::new(16 * 1024 * 1024).unwrap();
    backend
        .install_static_elf_file_with_context(
            std::fs::File::open(&executable).unwrap(),
            &[executable.to_str().unwrap()],
            &[],
            &directory.0,
        )
        .unwrap();
    let memory = backend.memory().clone();
    let control = std::sync::Arc::new(PendingStartControl::default());
    *PENDING_START.lock().unwrap() = Some(control.clone());
    let running = std::thread::spawn(move || {
        futures::executor::block_on(backend.run_static_elf_with_tool::<PendingStartTool>((), true))
    });
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        assert!(
            std::time::Instant::now() < deadline,
            "leader did not clear its TID while worker start was pending"
        );
        let mut word = [0; 4];
        memory.read(clear_tid, &mut word).unwrap();
        if control.entered.load(Ordering::Acquire) && word == [0; 4] {
            break;
        }
        std::thread::yield_now();
    }
    assert!(
        !running.is_finished(),
        "process completed while a committed worker was pending"
    );
    control.release.store(true, Ordering::Release);
    let (log, status, stdout, stderr) = running.join().unwrap().unwrap();
    assert_eq!(status, 73);
    assert_eq!(stdout, b"worker continued after leader exit\n");
    assert!(stderr.is_empty());
    assert_eq!(
        log.0.into_inner().unwrap(),
        vec![
            (0, 2, ExitStatus::Exited(73)),
            (0, 1, ExitStatus::Exited(73)),
            (1, 1, ExitStatus::Exited(73))
        ]
    );
    *PENDING_START.lock().unwrap() = None;
}

#[test]
fn exec_rearms_worker_survival_direct() {
    run_pthread(
        "leader_exit::exec_rearms_worker_survival_direct",
        "3",
        false,
    );
}
#[test]
fn exec_rearms_worker_survival_tool() {
    run_pthread("leader_exit::exec_rearms_worker_survival_tool", "3", true);
}

fn run_wait_status(test: &str, with_tool: bool) {
    if !leader_self_exec_bounded(test) {
        return;
    }
    let directory = TestDirectory::new();
    let executable = compile_c_program(
        &directory.0,
        "leader-wait-status",
        include_str!("../fixtures/leader_wait_status.c"),
    );
    let native = std::process::Command::new("timeout")
        .args(["--kill-after=2s", "5s"])
        .arg(&executable)
        .arg(if with_tool { "1" } else { "0" })
        .output()
        .unwrap();
    assert_eq!(native.status.code(), Some(0), "{native:?}");
    assert_eq!(
        native.stdout,
        b"wait observed last worker status exactly once\n"
    );
    assert!(native.stderr.is_empty(), "{native:?}");
    let mut backend = KvmBackend::new(256 * 1024 * 1024).unwrap();
    backend
        .install_static_elf_file_with_context(
            std::fs::File::open(&executable).unwrap(),
            &[
                executable.to_str().unwrap(),
                if with_tool { "1" } else { "0" },
            ],
            &[],
            &directory.0,
        )
        .unwrap();
    let (status, stdout, stderr) = if with_tool {
        let (log, status, stdout, stderr) =
            futures::executor::block_on(backend.run_static_elf_with_tool::<ExitTool>((), true))
                .unwrap();
        assert_eq!(
            log.0.into_inner().unwrap(),
            vec![
                (0, 3, ExitStatus::Exited(73)),
                (0, 2, ExitStatus::Exited(73)),
                (1, 2, ExitStatus::Exited(73)),
                (0, 1, ExitStatus::SUCCESS),
                (1, 1, ExitStatus::SUCCESS),
            ]
        );
        (status, stdout, stderr)
    } else {
        backend.run_static_elf_captured().unwrap()
    };
    assert_eq!(status, 0);
    assert_eq!(stdout, native.stdout);
    assert_eq!(stderr, native.stderr);
}
#[test]
fn parent_wait_observes_final_worker_status_after_direct_child_completion() {
    run_wait_status(
        "leader_exit::parent_wait_observes_final_worker_status_after_direct_child_completion",
        false,
    );
}
#[test]
fn parent_wait_observes_final_worker_status_tool() {
    run_wait_status(
        "leader_exit::parent_wait_observes_final_worker_status_tool",
        true,
    );
}

#[derive(Debug, Default)]
struct WorkerErrorControl {
    events: Mutex<Vec<(u8, i32, ExitStatus)>>,
    running: AtomicBool,
    dropped: AtomicBool,
}
static WORKER_ERROR: Mutex<Option<std::sync::Arc<WorkerErrorControl>>> = Mutex::new(None);
#[derive(Debug, Default)]
struct WorkerErrorLog(std::sync::Arc<WorkerErrorControl>);
impl Drop for WorkerErrorLog {
    fn drop(&mut self) {
        self.0.dropped.store(true, Ordering::Release);
    }
}
#[reverie::global_tool]
impl GlobalTool for WorkerErrorLog {
    type Config = u8;
    type Request = (u8, i32, ExitStatus);
    type Response = ();
    async fn init_global_state(_: &u8) -> Self {
        Self(WORKER_ERROR.lock().unwrap().as_ref().unwrap().clone())
    }
    async fn receive_rpc(&self, _: Pid, event: Self::Request) {
        match event.0 {
            2 => self.0.running.store(true, Ordering::Release),
            3 => {
                let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
                futures::future::poll_fn(|cx| {
                    assert!(
                        std::time::Instant::now() < deadline,
                        "first worker never ran"
                    );
                    if self.0.running.load(Ordering::Acquire) {
                        std::task::Poll::Ready(())
                    } else {
                        cx.waker().wake_by_ref();
                        std::task::Poll::Pending
                    }
                })
                .await;
            }
            _ => self.0.events.lock().unwrap().push(event),
        }
    }
}
#[derive(Debug, Default)]
struct WorkerErrorTool {
    mode: u8,
}
#[reverie::tool]
impl Tool for WorkerErrorTool {
    type GlobalState = WorkerErrorLog;
    type ThreadState = ();
    fn new(_: Pid, mode: &u8) -> Self {
        Self { mode: *mode }
    }
    fn subscriptions(_: &u8) -> Subscription {
        let mut result = Subscription::none();
        result.syscalls([Sysno::getpid, Sysno::getppid]);
        result
    }
    async fn handle_syscall_event<G: Guest<Self>>(
        &self,
        guest: &mut G,
        call: Syscall,
    ) -> Result<i64, reverie::Error> {
        match guest.tid().as_raw() {
            2 => guest.send_rpc((2, 2, ExitStatus::SUCCESS)).await,
            3 => {
                guest.send_rpc((3, 3, ExitStatus::SUCCESS)).await;
                assert_ne!(self.mode, 2, "controlled worker panic");
                return Err(std::io::Error::other("controlled worker execution failure").into());
            }
            _ => {}
        }
        Ok(guest.inject(call).await?)
    }
    async fn on_exit_thread<G: GlobalRPC<Self::GlobalState>>(
        &self,
        tid: Pid,
        global: &G,
        _: (),
        status: ExitStatus,
    ) -> Result<(), reverie::Error> {
        global.send_rpc((0, tid.as_raw(), status)).await;
        if self.mode == 1 && tid.as_raw() == 3 {
            return Err(std::io::Error::other("controlled failed worker hook").into());
        }
        Ok(())
    }
    async fn on_exit_process<G: GlobalRPC<Self::GlobalState>>(
        self,
        pid: Pid,
        global: &G,
        status: ExitStatus,
    ) -> Result<(), reverie::Error> {
        global.send_rpc((1, pid.as_raw(), status)).await;
        if self.mode == 1 {
            return Err(std::io::Error::other("controlled owner cleanup failure").into());
        }
        Ok(())
    }
}
fn run_worker_error(test: &str, hook_error: bool) {
    if !kvm_available(test) {
        return;
    }
    if std::env::var("REVERIE_LEADER_EXEC_CHILD").as_deref() != Ok(test) {
        let output = std::process::Command::new("timeout")
            .args(["--kill-after=2s", "30s"])
            .arg(std::env::current_exe().unwrap())
            .args(["--exact", test, "--nocapture"])
            .env("REVERIE_LEADER_EXEC_CHILD", test)
            .output()
            .unwrap();
        assert!(output.status.success(), "{test}: {output:?}");
        let stderr = String::from_utf8(output.stderr).unwrap();
        assert_eq!(
            stderr
                .matches("reverie-kvm guest thread 3 tool loop failed:")
                .count(),
            1,
            "{stderr}"
        );
        assert_eq!(
            stderr
                .matches("controlled worker execution failure")
                .count(),
            1,
            "{stderr}"
        );
        assert_eq!(
            stderr.matches("controlled failed worker hook").count(),
            usize::from(hook_error),
            "{stderr}"
        );
        assert!(
            !stderr.contains("reverie-kvm guest thread 2 tool loop failed:"),
            "{stderr}"
        );
        return;
    }
    let directory = TestDirectory::new();
    let executable = compile_c_program(
        &directory.0,
        "worker-error",
        include_str!("../fixtures/leader_pthread_exit.c"),
    );
    let mut backend = KvmBackend::new(256 * 1024 * 1024).unwrap();
    backend
        .install_static_elf_file_with_context(
            std::fs::File::open(&executable).unwrap(),
            &[executable.to_str().unwrap(), "4"],
            &[],
            &directory.0,
        )
        .unwrap();
    let control = std::sync::Arc::new(WorkerErrorControl::default());
    *WORKER_ERROR.lock().unwrap() = Some(control.clone());
    let error = futures::executor::block_on(
        backend.run_static_elf_with_tool::<WorkerErrorTool>(u8::from(hook_error), true),
    )
    .unwrap_err()
    .to_string();
    assert!(
        error.contains("controlled worker execution failure"),
        "{error}"
    );
    if hook_error {
        assert!(error.contains("controlled failed worker hook"), "{error}");
        assert!(
            error.contains("controlled owner cleanup failure"),
            "{error}"
        );
    }
    assert!(control.running.load(Ordering::Acquire));
    assert!(control.dropped.load(Ordering::Acquire));
    let events = control.events.lock().unwrap().clone();
    assert_eq!(events.len(), 4, "{events:?}");
    assert!(
        events[..2].contains(&(0, 2, ExitStatus::SUCCESS)),
        "{events:?}"
    );
    assert!(
        events[..2].contains(&(0, 3, ExitStatus::Exited(255))),
        "{events:?}"
    );
    assert_eq!(
        events[2..],
        [
            (0, 1, ExitStatus::Exited(255)),
            (1, 1, ExitStatus::Exited(255))
        ]
    );
    *WORKER_ERROR.lock().unwrap() = None;
    assert_eq!(
        std::sync::Arc::strong_count(&control),
        1,
        "no worker/global owner may survive API return"
    );
}
#[test]
fn failed_worker_cancels_live_sibling_after_leader_exit() {
    run_worker_error(
        "leader_exit::failed_worker_cancels_live_sibling_after_leader_exit",
        false,
    );
}
#[test]
fn failed_worker_preserves_execution_and_cleanup_errors() {
    run_worker_error(
        "leader_exit::failed_worker_preserves_execution_and_cleanup_errors",
        true,
    );
}

#[test]
fn panicked_worker_interrupts_natural_join_without_losing_panic() {
    const TEST: &str = "leader_exit::panicked_worker_interrupts_natural_join_without_losing_panic";
    if !kvm_available(TEST) {
        return;
    }
    if std::env::var("REVERIE_LEADER_EXEC_CHILD").as_deref() != Ok(TEST) {
        let output = std::process::Command::new("timeout")
            .args(["--kill-after=2s", "30s"])
            .arg(std::env::current_exe().unwrap())
            .args(["--exact", TEST, "--nocapture"])
            .env("REVERIE_LEADER_EXEC_CHILD", TEST)
            .output()
            .unwrap();
        assert!(output.status.success(), "{TEST}: {output:?}");
        let stderr = String::from_utf8(output.stderr).unwrap();
        assert_eq!(
            stderr.matches("controlled worker panic").count(),
            1,
            "{stderr}"
        );
        assert!(!stderr.contains("tool loop failed:"), "{stderr}");
        return;
    }
    let directory = TestDirectory::new();
    let executable = compile_c_program(
        &directory.0,
        "worker-panic",
        include_str!("../fixtures/leader_pthread_exit.c"),
    );
    let mut backend = KvmBackend::new(256 * 1024 * 1024).unwrap();
    backend
        .install_static_elf_file_with_context(
            std::fs::File::open(&executable).unwrap(),
            &[executable.to_str().unwrap(), "4"],
            &[],
            &directory.0,
        )
        .unwrap();
    let control = std::sync::Arc::new(WorkerErrorControl::default());
    *WORKER_ERROR.lock().unwrap() = Some(control.clone());
    let error =
        futures::executor::block_on(backend.run_static_elf_with_tool::<WorkerErrorTool>(2, true))
            .unwrap_err()
            .to_string();
    assert!(
        error.contains("thread 3: guest thread panicked during teardown"),
        "{error}"
    );
    assert!(control.running.load(Ordering::Acquire));
    assert!(control.dropped.load(Ordering::Acquire));
    assert_eq!(
        *control.events.lock().unwrap(),
        vec![
            (0, 2, ExitStatus::SUCCESS),
            (0, 1, ExitStatus::Exited(255)),
            (1, 1, ExitStatus::Exited(255)),
        ]
    );
    *WORKER_ERROR.lock().unwrap() = None;
    assert_eq!(std::sync::Arc::strong_count(&control), 1);
}
