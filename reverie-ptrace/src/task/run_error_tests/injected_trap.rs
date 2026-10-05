//! Actual marker/RIP/frame traps exercise the same dispatch as an injected
//! syscall. This does not qualify rewritten signal frames or Read restart.
use std::fs::File;
use std::io::Write;
use std::os::fd::FromRawFd;

use reverie::process::Command;
use reverie::process::Stdio;

use super::*;
use crate::TracerBuilder;

const ENTRY: u64 = 0x400078;
const MARKER: u64 = 0x5245564552524f52;

#[derive(Clone, Copy, Debug, Default, serde::Serialize, serde::Deserialize)]
enum Case {
    #[default]
    Errno,
    Tool,
    Io,
}
impl Case {
    fn fatal(self) -> bool {
        !matches!(self, Self::Errno)
    }
    fn count(self) -> usize {
        if self.fatal() { 2 } else { 1 }
    }
    fn status(self) -> ExitStatus {
        if self.fatal() {
            ExitStatus::Signaled(Signal::SIGKILL, false)
        } else {
            ExitStatus::Exited(17)
        }
    }
}
#[derive(Debug)]
struct TrapFailure;
impl std::fmt::Display for TrapFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("typed injected-trap Tool refusal")
    }
}
impl std::error::Error for TrapFailure {}

struct TrapLog {
    case: Case,
    owners: BTreeMap<Pid, TerminalCleanup>,
    counts: Option<(Arc<AtomicUsize>, Arc<AtomicUsize>)>,
    calls: BTreeMap<Pid, usize>,
    terminal: BTreeMap<Pid, ExitStatus>,
    consumed: BTreeSet<Pid>,
    processes: BTreeSet<Pid>,
    retired: BTreeSet<Pid>,
    ready_tx: Option<oneshot::Sender<()>>,
    ready_rx: Option<oneshot::Receiver<()>>,
}
thread_local! {
    static TRAP: RefCell<Option<Arc<StdMutex<TrapLog>>>> = const { RefCell::new(None) };
}
fn log() -> Arc<StdMutex<TrapLog>> {
    TRAP.with(|s| s.borrow().clone().unwrap())
}
struct ActiveTrap;
impl Drop for ActiveTrap {
    fn drop(&mut self) {
        TRAP.with(|s| assert!(s.borrow_mut().take().is_some()));
    }
}
pub(super) fn retained(task: &Stopped, tasks: &Arc<AtomicUsize>, daemons: &Arc<AtomicUsize>) {
    let Some(log) = TRAP.with(|s| s.borrow().clone()) else {
        return;
    };
    let mut log = log.lock().unwrap();
    assert!(
        log.owners
            .insert(task.pid(), task.terminal_cleanup())
            .is_none()
    );
    if let Some((old_tasks, old_daemons)) = &log.counts {
        assert!(Arc::ptr_eq(old_tasks, tasks));
        assert!(Arc::ptr_eq(old_daemons, daemons));
    } else {
        log.counts = Some((Arc::clone(tasks), Arc::clone(daemons)));
    }
}
pub(super) fn retired(tid: Pid, tasks: &Arc<AtomicUsize>, daemons: &Arc<AtomicUsize>) {
    let Some(log) = TRAP.with(|s| s.borrow().clone()) else {
        return;
    };
    let mut log = log.lock().unwrap();
    assert!(log.terminal.contains_key(&tid));
    assert!(log.consumed.contains(&tid));
    assert!(log.processes.contains(&tid));
    assert!(log.retired.insert(tid));
    assert_eq!(
        tasks.load(Ordering::SeqCst),
        log.case.count() - log.retired.len()
    );
    assert_eq!(daemons.load(Ordering::SeqCst), 0);
}
#[derive(Default)]
struct TrapTool;
#[reverie::tool]
impl Tool for TrapTool {
    type GlobalState = Global;
    type ThreadState = State;
    fn subscriptions(_: &()) -> Subscription {
        [Sysno::getuid].into_iter().collect()
    }
    async fn handle_syscall_event<G: Guest<Self>>(
        &self,
        guest: &mut G,
        call: Syscall,
    ) -> Result<i64, reverie::Error> {
        assert!(matches!(call, Syscall::Getuid(_)));
        let (_, args) = call.into_parts();
        let shared = log();
        let case = {
            let mut log = shared.lock().unwrap();
            assert!(log.calls.insert(guest.tid(), args.arg0).is_none());
            log.case
        };
        if !case.fatal() {
            assert_eq!(args.arg0, 1);
            return Err(Errno::EBADF.into());
        }
        match args.arg0 {
            1 => {
                let ready = shared.lock().unwrap().ready_rx.take().unwrap();
                ready
                    .await
                    .expect("actual child reached injected pending callback");
                {
                    let log = shared.lock().unwrap();
                    assert_eq!(log.owners.len(), 2);
                    assert_eq!(log.calls.len(), 2);
                    assert!(log.terminal.is_empty());
                    assert!(log.owners.values().all(|o| o.observed_terminal().is_none()));
                }
                match case {
                    Case::Tool => Err(reverie::Error::Tool(anyhow::Error::new(TrapFailure))),
                    Case::Io => Err(reverie::Error::Io(std::io::Error::new(
                        std::io::ErrorKind::PermissionDenied,
                        TrapFailure,
                    ))),
                    Case::Errno => unreachable!(),
                }
            }
            2 => {
                shared
                    .lock()
                    .unwrap()
                    .ready_tx
                    .take()
                    .unwrap()
                    .send(())
                    .unwrap();
                future::pending().await
            }
            other => panic!("unexpected injected operand {other}"),
        }
    }
    fn on_backend_thread_terminal(
        &self,
        tid: Pid,
        _: &Global,
        state: &mut State,
        status: ExitStatus,
    ) {
        assert!(!state.terminal);
        state.terminal = true;
        let shared = log();
        let mut log = shared.lock().unwrap();
        assert_eq!(status, log.case.status());
        assert!(
            matches!(log.owners[&tid].observed_terminal(), Some(Ok(actual)) if actual == status)
        );
        assert!(log.terminal.insert(tid, status).is_none());
    }
    async fn on_exit_thread<G: GlobalRPC<Global>>(
        &self,
        tid: Pid,
        _: &G,
        state: State,
        status: ExitStatus,
    ) -> Result<(), reverie::Error> {
        assert!(state.terminal);
        let shared = log();
        let mut log = shared.lock().unwrap();
        assert_eq!(log.terminal.get(&tid), Some(&status));
        assert!(log.consumed.insert(tid));
        Ok(())
    }
    async fn on_exit_process<G: GlobalRPC<Global>>(
        self,
        pid: Pid,
        _: &G,
        status: ExitStatus,
    ) -> Result<(), reverie::Error> {
        let shared = log();
        let mut log = shared.lock().unwrap();
        assert_eq!(log.terminal.get(&pid), Some(&status));
        assert!(log.consumed.contains(&pid));
        assert!(log.processes.insert(pid));
        Ok(())
    }
}

// Same small static-ELF convention as the initial Command controls. The only
// getuid event is the actual int3: there is no native getuid syscall to confuse
// with the injected path. The guest owns and initializes its writable frame.
fn image(case: Case) -> (File, u64) {
    let mut code = vec![0x41, 0xb8, 1, 0, 0, 0]; // mov r8d,1 (root role)
    if case.fatal() {
        code.extend_from_slice(&[
            0xb8, 57, 0, 0, 0, 0x0f, 0x05, // fork
            0x48, 0x85, 0xc0, 0x75, 0x03, // test rax,rax; jnz root
            0x41, 0xff, 0xc0, // inc r8d (child role)
        ]);
    }
    code.extend_from_slice(&[
        0x48, 0x81, 0xec, 144, 0, 0, 0, // sub rsp,144
        0x48, 0x89, 0xe7, 0xb9, 18, 0, 0, 0, 0x31, 0xc0, 0xfc, 0xf3, 0x48, 0xab,
        // mov rdi,rsp; mov ecx,18; xor eax,eax; cld; rep stosq
        0x4c, 0x89, 0x44, 0x24, 72, // frame.rdi = role
        0x48, 0xc7, 0x44, 0x24, 120, 102, 0, 0, 0, // frame.rax = getuid
        0x48, 0x8d, 0x84, 0x24, 144, 0, 0, 0, // lea rax,[rsp+144]
        0x48, 0x89, 0x84, 0x24, 128, 0, 0, 0, // frame.rsp = initial rsp
        0x48, 0xc7, 0x84, 0x24, 136, 0, 0, 0,
    ]);
    code.extend_from_slice(&(ENTRY as u32).to_le_bytes()); // logical frame.rip
    code.extend_from_slice(&[0x48, 0x89, 0xe7, 0x48, 0xb8]); // rdi=frame; rax=marker
    code.extend_from_slice(&MARKER.to_le_bytes());
    code.push(0xcc);
    let trap_rip = ENTRY + code.len() as u64;
    code.extend_from_slice(&[
        0xb8, 1, 0, 0, 0, 0xbf, 1, 0, 0, 0, // write(stdout, frame.rax, 8)
        0x48, 0x8d, 0x74, 0x24, 120, 0xba, 8, 0, 0, 0, 0x0f, 0x05, 0xb8, 60, 0, 0, 0, 0xbf, 17, 0,
        0, 0, 0x0f, 0x05, // exit(17)
    ]);
    let size = 120 + code.len();
    let mut elf = vec![0u8; size];
    elf[..7].copy_from_slice(b"\x7fELF\x02\x01\x01");
    elf[16..18].copy_from_slice(&2u16.to_le_bytes());
    elf[18..20].copy_from_slice(&62u16.to_le_bytes());
    elf[20..24].copy_from_slice(&1u32.to_le_bytes());
    elf[24..32].copy_from_slice(&ENTRY.to_le_bytes());
    elf[32..40].copy_from_slice(&64u64.to_le_bytes());
    elf[52..54].copy_from_slice(&64u16.to_le_bytes());
    elf[54..56].copy_from_slice(&56u16.to_le_bytes());
    elf[56..58].copy_from_slice(&1u16.to_le_bytes());
    elf[64..68].copy_from_slice(&1u32.to_le_bytes());
    elf[68..72].copy_from_slice(&5u32.to_le_bytes());
    elf[80..88].copy_from_slice(&0x400000u64.to_le_bytes());
    elf[88..96].copy_from_slice(&0x400000u64.to_le_bytes());
    elf[96..104].copy_from_slice(&(size as u64).to_le_bytes());
    elf[104..112].copy_from_slice(&(size as u64).to_le_bytes());
    elf[112..120].copy_from_slice(&4096u64.to_le_bytes());
    elf[120..].copy_from_slice(&code);
    let fd = unsafe {
        libc::memfd_create(
            c"injected-trap-error".as_ptr(),
            libc::MFD_CLOEXEC | libc::MFD_ALLOW_SEALING | libc::MFD_EXEC,
        )
    };
    assert!(fd >= 0);
    let mut file = unsafe { File::from_raw_fd(fd) };
    file.write_all(&elf).unwrap();
    assert_eq!(unsafe { libc::fchmod(fd, 0o500) }, 0);
    assert_eq!(
        unsafe {
            libc::fcntl(
                fd,
                libc::F_ADD_SEALS,
                libc::F_SEAL_WRITE | libc::F_SEAL_GROW | libc::F_SEAL_SHRINK | libc::F_SEAL_SEAL,
            )
        },
        0
    );
    (file, trap_rip)
}

fn run(case: Case) {
    let (ready_tx, ready_rx) = oneshot::channel();
    let shared = Arc::new(StdMutex::new(TrapLog {
        case,
        owners: BTreeMap::new(),
        counts: None,
        calls: BTreeMap::new(),
        terminal: BTreeMap::new(),
        consumed: BTreeSet::new(),
        processes: BTreeSet::new(),
        retired: BTreeSet::new(),
        ready_tx: Some(ready_tx),
        ready_rx: Some(ready_rx),
    }));
    TRAP.with(|s| assert!(s.borrow_mut().replace(Arc::clone(&shared)).is_none()));
    let _active = ActiveTrap;
    let (image, trap_rip) = image(case);
    let result = crate::testing::run_tokio_test(async {
        let mut command = Command::new(format!(
            "/proc/{}/fd/{}",
            std::process::id(),
            image.as_raw_fd()
        ));
        command.stdout(Stdio::piped()).stderr(Stdio::piped());
        let tracer = TracerBuilder::<TrapTool>::new(command)
            .injected_syscall_trap(MARKER, trap_rip)
            .spawn()
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(5), tracer.wait_with_output())
            .await
            .expect("injected-trap error cleanup hung")
    });
    match case {
        Case::Errno => {
            let (output, _) = result.expect("guest errno became a fatal error");
            assert_eq!(output.status, ExitStatus::Exited(17));
            assert_eq!(output.stdout, (-i64::from(libc::EBADF)).to_le_bytes());
            assert!(output.stderr.is_empty());
        }
        Case::Tool => match result {
            Err(reverie::Error::Tool(error)) => {
                assert!(error.is::<TrapFailure>(), "typed cause lost: {error}")
            }
            _ => panic!("injected Tool refusal was converted to guest result"),
        },
        Case::Io => match result {
            Err(reverie::Error::Io(error)) => {
                assert_eq!(error.kind(), std::io::ErrorKind::PermissionDenied);
                assert!(error.get_ref().unwrap().is::<TrapFailure>());
            }
            _ => panic!("injected I/O refusal was converted to guest result"),
        },
    }
    let log = shared.lock().unwrap();
    let count = case.count();
    assert_eq!(log.owners.len(), count);
    assert_eq!(log.calls.len(), count);
    assert_eq!(
        log.calls.values().copied().collect::<BTreeSet<_>>(),
        if case.fatal() {
            BTreeSet::from([1, 2])
        } else {
            BTreeSet::from([1])
        }
    );
    assert_eq!(log.terminal.len(), count);
    assert_eq!(log.consumed.len(), count);
    assert_eq!(log.processes.len(), count);
    assert_eq!(log.retired.len(), count);
    let (tasks, daemons) = log.counts.as_ref().unwrap();
    assert_eq!(tasks.load(Ordering::SeqCst), 0);
    assert_eq!(daemons.load(Ordering::SeqCst), 0);
    for (tid, owner) in &log.owners {
        assert!(
            owner.wait(Duration::from_secs(1)),
            "original notifier did not drain {tid}"
        );
        assert!(matches!(owner.observed_terminal(), Some(Ok(actual)) if actual == case.status()));
        assert_eq!(unsafe { libc::kill(tid.as_raw(), 0) }, -1);
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::ESRCH)
        );
        assert_eq!(
            unsafe { libc::waitpid(tid.as_raw(), std::ptr::null_mut(), libc::WNOHANG) },
            -1
        );
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::ECHILD)
        );
    }
}

#[test]
fn injected_trap_errno_remains_exact_guest_errno() {
    run(Case::Errno);
}
#[test]
fn injected_trap_tool_error_preserves_cause_and_drains_actual_root_and_child() {
    let Some(receipt) = isolated_real_parent(
        "task::run_error_tests::injected_trap::injected_trap_tool_error_preserves_cause_and_drains_actual_root_and_child",
    ) else {
        return;
    };
    run(Case::Tool);
    assert_eq!(receipt.send(&[1]).unwrap(), 1);
}
#[test]
fn injected_trap_io_error_preserves_cause_and_drains_actual_root_and_child() {
    let Some(receipt) = isolated_real_parent(
        "task::run_error_tests::injected_trap::injected_trap_io_error_preserves_cause_and_drains_actual_root_and_child",
    ) else {
        return;
    };
    run(Case::Io);
    assert_eq!(receipt.send(&[1]).unwrap(), 1);
}
