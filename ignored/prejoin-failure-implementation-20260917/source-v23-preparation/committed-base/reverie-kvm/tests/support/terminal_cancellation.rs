use std::collections::BTreeMap;

use super::*;

static TERMINAL_MEMORY: Mutex<Option<reverie_kvm::GuestMemory>> = Mutex::new(None);
static TERMINAL_EVENTS: Mutex<Vec<(u8, i32, u64)>> = Mutex::new(Vec::new());

fn event(kind: u8, tid: i32, value: u64) {
    TERMINAL_EVENTS.lock().unwrap().push((kind, tid, value));
}

#[derive(Default)]
struct TerminalLog;
#[reverie::global_tool]
impl GlobalTool for TerminalLog {
    type Request = ();
    type Response = ();
    type Config = u8;
    async fn receive_rpc(&self, _: Pid, _: ()) {}
}

#[derive(Default)]
struct TerminalTool {
    pid: i32,
    mode: u8,
    clone_count: AtomicU64,
    target: AtomicU64,
    completed_tids: Mutex<std::collections::BTreeSet<i32>>,
    clone_tids: Mutex<BTreeMap<u64, i32>>,
    peer: AtomicU64,
    child_tid: Mutex<BTreeMap<i32, u64>>,
    starts: AtomicU64,
    exits: AtomicU64,
    posts: AtomicU64,
}
#[reverie::tool]
impl Tool for TerminalTool {
    type GlobalState = TerminalLog;
    type ThreadState = (i32, i32, bool, u64);

    fn new(pid: Pid, mode: &u8) -> Self {
        Self {
            pid: pid.as_raw(),
            mode: *mode,
            ..Self::default()
        }
    }
    fn init_thread_state(
        &self,
        tid: Pid,
        _: Option<(Pid, &Self::ThreadState)>,
    ) -> Self::ThreadState {
        (self.pid, tid.as_raw(), false, 0)
    }
    async fn handle_thread_start<G: Guest<Self>>(
        &self,
        guest: &mut G,
    ) -> Result<(), reverie::Error> {
        assert_eq!(
            *guest.thread_state(),
            (self.pid, guest.tid().as_raw(), false, 0)
        );
        guest.thread_state_mut().2 = true;
        self.starts.fetch_add(1, Ordering::SeqCst);
        event(0, guest.tid().as_raw(), 0);
        if self.mode == 0 || (self.mode == 3 && self.is_target(guest.tid())) {
            event(3, guest.tid().as_raw(), 0);
            guest.cancel_current_thread().await;
        }
        Ok(())
    }
    async fn handle_post_exec<G: Guest<Self>>(&self, guest: &mut G) -> Result<(), Errno> {
        let count = self.posts.fetch_add(1, Ordering::SeqCst) + 1;
        event(4, guest.tid().as_raw(), count);
        if self.mode == 2 || (self.mode == 12 && count == 2) {
            event(3, guest.tid().as_raw(), 2);
            guest.cancel_current_thread().await;
        }
        Ok(())
    }
    async fn handle_syscall_event<G: Guest<Self>>(
        &self,
        guest: &mut G,
        syscall: Syscall,
    ) -> Result<i64, reverie::Error> {
        assert!(guest.thread_state().2);
        if guest.tid().as_raw() == self.pid
            && syscall.number() == Sysno::getpid
            && syscall.into_parts().1.arg0 == 0x7465726d
        {
            let ordinal = syscall.into_parts().1.arg1 as u64;
            let child = *self
                .clone_tids
                .lock()
                .unwrap()
                .get(&ordinal)
                .expect("created child ordinal");
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            futures::future::poll_fn(|cx| {
                assert!(std::time::Instant::now() < deadline,
                    "consuming on_exit_thread for guest thread {child} did not complete before pthread_join");
                if self.completed_tids.lock().unwrap().contains(&child) {
                    std::task::Poll::Ready(())
                } else {
                    cx.waker().wake_by_ref();
                    std::task::Poll::Pending
                }
            })
            .await;
            return Ok(i64::from(self.pid));
        }
        if self.mode == 1 && matches!(syscall, Syscall::Execve(_)) {
            event(3, guest.tid().as_raw(), 1);
            guest.cancel_current_thread().await;
        }
        if matches!(syscall.number(), Sysno::clone | Sysno::clone3) {
            let request = reverie_kvm::SyscallRequest::from_syscall(syscall);
            let (flags, child_tid_address) = if syscall.number() == Sysno::clone {
                (request.args()[0], request.args()[3])
            } else {
                let memory = guest.memory();
                let flags = memory.read_value(
                    reverie::syscalls::Addr::<u64>::from_raw(request.args()[0] as usize).unwrap(),
                )?;
                let child_tid = memory.read_value(
                    reverie::syscalls::Addr::<u64>::from_raw(request.args()[0] as usize + 16)
                        .unwrap(),
                )?;
                (flags, child_tid)
            };
            let child = guest.inject(syscall).await?;
            assert!(child > 0);
            assert_ne!(flags & libc::CLONE_CHILD_CLEARTID as u64, 0);
            assert_ne!(child_tid_address, 0);
            self.child_tid
                .lock()
                .unwrap()
                .insert(child as i32, child_tid_address);
            let ordinal = self.clone_count.fetch_add(1, Ordering::SeqCst) + 1;
            self.clone_tids
                .lock()
                .unwrap()
                .insert(ordinal, child as i32);
            if ordinal == 1 {
                self.peer.store(child as u64, Ordering::SeqCst);
            }
            if ordinal == 2 {
                self.target.store(child as u64, Ordering::SeqCst);
                if self.mode == 7 {
                    assert_eq!(
                        guest
                            .inject(
                                reverie::syscalls::Tgkill::new()
                                    .with_tgid(guest.pid().as_raw())
                                    .with_tid(child as i32)
                                    .with_sig(libc::SIGUSR1)
                            )
                            .await?,
                        0
                    );
                }
            }
            if self.mode == 9 && self.is_target(guest.tid()) {
                event(3, guest.tid().as_raw(), 9);
                guest.cancel_current_thread().await;
            }
            return Ok(child);
        }
        if self.is_target(guest.tid()) && matches!(syscall, Syscall::Getpid(_)) {
            if self.mode == 4 {
                event(3, guest.tid().as_raw(), 4);
                guest.cancel_current_thread().await;
            }
            if matches!(self.mode, 5 | 6 | 10 | 13) {
                for sig in if self.mode == 6 {
                    &[libc::SIGUSR1, libc::SIGUSR2][..]
                } else {
                    &[libc::SIGUSR1][..]
                } {
                    let mut info = [0; reverie::SIGNAL_INFO_SIZE];
                    info[0..4].copy_from_slice(&sig.to_ne_bytes());
                    info[8..12].copy_from_slice(&libc::SI_TKILL.to_ne_bytes());
                    guest
                        .defer_signal_delivery(SignalEvent::new(
                            *sig,
                            info,
                            SignalTarget::Thread {
                                pid: guest.pid(),
                                tid: guest.tid(),
                            },
                        )?)
                        .await?;
                }
                return Ok(guest.pid().as_raw() as i64);
            }
        }
        guest.tail_inject(syscall).await
    }
    async fn handle_structured_signal_event<G: Guest<Self>>(
        &self,
        guest: &mut G,
        signal: SignalEvent,
    ) -> Result<Option<SignalEvent>, Errno> {
        assert!(self.is_target(guest.tid()));
        assert!(guest.thread_state().2);
        assert_eq!(
            signal.target(),
            SignalTarget::Thread {
                pid: guest.pid(),
                tid: guest.tid()
            }
        );
        let ordinal = guest.thread_state().3 + 1;
        guest.thread_state_mut().3 = ordinal;
        let registers = guest.regs().await;
        event(5, guest.tid().as_raw(), signal.signal() as u64);
        if self.mode == 13 || (self.mode == 6 && ordinal == 1) {
            assert_eq!(signal.signal(), libc::SIGUSR1);
            return Ok(Some(signal));
        }
        if self.mode == 6 {
            assert_eq!(ordinal, 2);
            assert_eq!(signal.signal(), libc::SIGUSR2);
            assert_eq!(registers.orig_rax, libc::SYS_rt_sigreturn as u64);
        } else if self.mode == 8 {
            assert_eq!(signal.signal(), libc::SIGSEGV);
            assert_eq!(registers.orig_rax, u64::MAX);
            assert_eq!(&signal.siginfo()[16..24], &[0; 8]);
        } else if self.mode == 7 {
            assert_eq!(signal.signal(), libc::SIGUSR1);
            assert_eq!(registers.orig_rax, u64::MAX);
        } else {
            assert_eq!(signal.signal(), libc::SIGUSR1);
            assert_eq!(registers.orig_rax, libc::SYS_getpid as u64);
        }
        event(3, guest.tid().as_raw(), self.mode as u64);
        guest.cancel_current_thread().await
    }
    async fn on_exit_thread<G: GlobalRPC<Self::GlobalState>>(
        &self,
        tid: Pid,
        _: &G,
        state: Self::ThreadState,
        status: ExitStatus,
    ) -> Result<(), reverie::Error> {
        assert_eq!((state.0, state.1, state.2), (self.pid, tid.as_raw(), true));
        assert_eq!(status, ExitStatus::SUCCESS);
        if tid.as_raw() != self.pid {
            let address = *self
                .child_tid
                .lock()
                .unwrap()
                .get(&tid.as_raw())
                .expect("registered clear-child-tid");
            let mut bytes = [0; 4];
            TERMINAL_MEMORY
                .lock()
                .unwrap()
                .as_ref()
                .unwrap()
                .read(address, &mut bytes)
                .unwrap();
            assert_eq!(
                bytes, [0; 4],
                "child TID must be cleared before consuming Tool exit"
            );
            event(6, tid.as_raw(), address);
        }
        self.exits.fetch_add(1, Ordering::SeqCst);
        event(1, tid.as_raw(), state.3);
        self.completed_tids.lock().unwrap().insert(tid.as_raw());
        if self.mode == 10
            && (self.is_target(tid) || self.peer.load(Ordering::SeqCst) == tid.as_raw() as u64)
        {
            return Err(Errno::EIO.into());
        }
        Ok(())
    }
    async fn on_exit_process<G: GlobalRPC<Self::GlobalState>>(
        self,
        pid: Pid,
        _: &G,
        status: ExitStatus,
    ) -> Result<(), reverie::Error> {
        assert_eq!(pid.as_raw(), self.pid);
        assert_eq!(status, ExitStatus::SUCCESS);
        let count = self.starts.load(Ordering::SeqCst);
        assert_eq!(self.exits.load(Ordering::SeqCst), count);
        event(2, pid.as_raw(), count);
        Ok(())
    }
}
impl TerminalTool {
    fn is_target(&self, tid: Pid) -> bool {
        self.target.load(Ordering::SeqCst) == tid.as_raw() as u64
    }
}

#[test]
fn terminal_cancellation_consumes_all_callback_contexts_and_preserves_live_peers() {
    const TEST: &str = "terminal_cancellation::terminal_cancellation_consumes_all_callback_contexts_and_preserves_live_peers";
    if !leader_self_exec_bounded(TEST) {
        return;
    }
    let directory = TestDirectory::new();
    let executable = compile_c_program(
        &directory.0,
        "terminal-cancellation",
        include_str!("../fixtures/terminal_cancellation.c"),
    );
    if let Some(dest) = std::env::var_os("REVERIE_TERMINAL_ARTIFACTS") {
        std::fs::create_dir_all(&dest).unwrap();
        std::fs::copy(&executable, PathBuf::from(&dest).join("guest")).unwrap();
        std::fs::copy(
            executable.with_extension("c"),
            PathBuf::from(&dest).join("guest.c"),
        )
        .unwrap();
    }
    for mode in [0u8, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 12, 13] {
        eprintln!("terminal callback mode={mode} starting");
        TERMINAL_EVENTS.lock().unwrap().clear();
        let argument = mode.to_string();
        let mut backend = KvmBackend::new(256 * 1024 * 1024).unwrap();
        backend
            .install_static_elf_file_with_context(
                std::fs::File::open(&executable).unwrap(),
                &[executable.to_str().unwrap(), &argument],
                &[],
                &directory.0,
            )
            .unwrap();
        *TERMINAL_MEMORY.lock().unwrap() = Some(backend.memory().clone());
        let result = futures::executor::block_on(
            backend.run_static_elf_with_tool::<TerminalTool>(mode, true),
        );
        *TERMINAL_MEMORY.lock().unwrap() = None;
        let events = TERMINAL_EVENTS.lock().unwrap().clone();
        eprintln!("terminal callback mode={mode} events={events:?}");
        if mode == 10 {
            let error = result
                .err()
                .expect("both forced worker exit-hook errors must remain failures")
                .to_string();
            let mut worker_tids = events
                .iter()
                .filter(|e| e.0 == 0)
                .map(|e| e.1)
                .skip(1)
                .take(2)
                .collect::<Vec<_>>();
            worker_tids.sort();
            let positions = worker_tids
                .iter()
                .map(|tid| {
                    error
                        .find(&format!("thread {tid}:"))
                        .expect("each failed worker must be reported")
                })
                .collect::<Vec<_>>();
            assert!(
                positions[0] < positions[1],
                "stable guest-TID order: {error}"
            );
            assert_eq!(error.matches("EIO").count(), 2, "{error}");
            eprintln!("terminal forced hook error: {error}");
        } else {
            let (_, status, stdout, stderr) = result.unwrap();
            assert_eq!(
                status, 0,
                "mode={mode}, stdout={stdout:?}, stderr={stderr:?}"
            );
            let expected: &[u8] = if mode <= 2 {
                b""
            } else if mode == 12 {
                b"terminal-before-exec\n"
            } else {
                b"terminal-lifecycle-checked\n"
            };
            assert_eq!(stdout, expected, "mode={mode}");
            assert!(stderr.is_empty(), "mode={mode} stderr={stderr:?}");
        }
        let expected_threads = if mode <= 2 || mode == 12 {
            1
        } else if mode == 9 {
            5
        } else {
            4
        };
        let mut starts = events
            .iter()
            .filter(|e| e.0 == 0)
            .map(|e| e.1)
            .collect::<Vec<_>>();
        let mut exits = events
            .iter()
            .filter(|e| e.0 == 1)
            .map(|e| e.1)
            .collect::<Vec<_>>();
        assert_eq!(starts.len(), expected_threads, "mode={mode}");
        assert_eq!(exits.len(), expected_threads, "mode={mode}");
        starts.sort();
        exits.sort();
        assert_eq!(starts, exits);
        starts.dedup();
        assert_eq!(
            starts.len(),
            expected_threads,
            "no duplicated consuming hooks"
        );
        assert_eq!(
            events.iter().filter(|e| e.0 == 6).count(),
            expected_threads - 1
        );
        assert_eq!(
            events.iter().filter(|e| e.0 == 3).count(),
            usize::from(mode != 13)
        );
        let processes = events.iter().filter(|e| e.0 == 2).collect::<Vec<_>>();
        assert_eq!(processes.len(), 1);
        assert_eq!(processes[0].2 as usize, expected_threads);
        assert_eq!(
            events.last().unwrap().0,
            2,
            "process hook follows every consuming thread hook"
        );
        eprintln!(
            "terminal callback mode={mode} checked {expected_threads} thread hooks and one process hook"
        );
    }
}

#[derive(Default)]
struct DirectTerminalTool;
#[reverie::tool]
impl Tool for DirectTerminalTool {
    type GlobalState = TerminalLog;
    type ThreadState = (bool, bool);
    async fn handle_thread_start<G: Guest<Self>>(
        &self,
        guest: &mut G,
    ) -> Result<(), reverie::Error> {
        guest.thread_state_mut().0 = true;
        event(0, guest.tid().as_raw(), 0);
        if *guest.config() == 0 {
            guest.cancel_current_thread().await;
        }
        Ok(())
    }
    async fn handle_syscall_event<G: Guest<Self>>(
        &self,
        guest: &mut G,
        syscall: Syscall,
    ) -> Result<i64, reverie::Error> {
        assert_eq!(syscall.number(), Sysno::getpid);
        guest.thread_state_mut().1 = true;
        event(3, guest.tid().as_raw(), 0);
        guest.cancel_current_thread().await
    }
    async fn on_exit_thread<G: GlobalRPC<Self::GlobalState>>(
        &self,
        tid: Pid,
        global: &G,
        state: Self::ThreadState,
        status: ExitStatus,
    ) -> Result<(), reverie::Error> {
        assert_eq!(state, (true, *global.config() == 1));
        assert_eq!(status, ExitStatus::SUCCESS);
        event(1, tid.as_raw(), 0);
        Ok(())
    }
    async fn on_exit_process<G: GlobalRPC<Self::GlobalState>>(
        self,
        pid: Pid,
        _: &G,
        status: ExitStatus,
    ) -> Result<(), reverie::Error> {
        assert_eq!(status, ExitStatus::SUCCESS);
        event(2, pid.as_raw(), 0);
        Ok(())
    }
}
struct NoDirectInjection;
impl reverie_kvm::SyscallExecutor for NoDirectInjection {
    fn execute(&mut self, _: &reverie_kvm::SyscallRequest, _: &reverie_kvm::GuestMemory) -> i64 {
        panic!("terminal operation must not execute a syscall")
    }
}
#[test]
fn terminal_cancellation_cleans_direct_hypercall_start_and_syscall_callbacks() {
    const TEST: &str = "terminal_cancellation::terminal_cancellation_cleans_direct_hypercall_start_and_syscall_callbacks";
    if !leader_self_exec_bounded(TEST) {
        return;
    }
    for mode in [0, 1] {
        TERMINAL_EVENTS.lock().unwrap().clear();
        let mut backend = KvmBackend::new(MEMORY_SIZE).unwrap();
        backend
            .install_syscall(
                0x1000,
                0x2000,
                reverie_kvm::SyscallRequest::new(libc::SYS_getpid as u64, [0; 6]),
            )
            .unwrap();
        futures::executor::block_on(
            backend.run_with_tool::<DirectTerminalTool, _>(mode, NoDirectInjection),
        )
        .unwrap();
        let events = TERMINAL_EVENTS.lock().unwrap().clone();
        let kinds = events.iter().map(|event| event.0).collect::<Vec<_>>();
        assert_eq!(
            kinds,
            if mode == 0 {
                vec![0, 1, 2]
            } else {
                vec![0, 3, 1, 2]
            }
        );
        eprintln!("terminal direct mode={mode} events={events:?}");
    }
}
