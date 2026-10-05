//! Real Command lifecycle controls; no native policy receipt is inferred here.
use std::fs::File;
use std::io::Write;
use std::os::fd::AsRawFd;
use std::os::fd::FromRawFd;
use std::os::fd::OwnedFd;
use std::sync::Arc;
use std::sync::Mutex;

use reverie::GlobalRPC;
use reverie::Guest;
use reverie::InitialCommandObservation;
use reverie::syscalls::SyscallInfo;
use serde::Deserialize;
use serde::Serialize;

use super::*;

const ENTRY: u64 = 0x400078;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct State {
    stopped: u32,
    exec: u32,
    post_exec: u32,
    entry_ip: u64,
    ordinary_exec_injections: u32,
    instructions: Vec<(u16, u8, u8, u32)>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
struct Terminal {
    tid: Pid,
    state: State,
    status: ExitStatus,
}
#[derive(Debug, Default)]
struct BoundaryGlobal {
    terminals: Arc<Mutex<Vec<Terminal>>>,
    failures: Arc<Mutex<Vec<&'static str>>>,
}
#[reverie::global_tool]
impl GlobalTool for BoundaryGlobal {
    type Config = ();
    type Request = Terminal;
    type Response = ();
    async fn receive_rpc(&self, _from: Pid, terminal: Terminal) {
        self.terminals.lock().unwrap().push(terminal);
    }
    fn report_backend_failure(&self, failure: reverie::BackendFailure) {
        self.failures.lock().unwrap().push(failure.phase);
    }
}

fn instructions(filter: &reverie::process::seccomp::Filter) -> Vec<(u16, u8, u8, u32)> {
    filter
        .instructions()
        .iter()
        .map(|i| (i.code, i.jt, i.jf, i.k))
        .collect()
}

// 0: executable entry; 1/2: reject at stop/exec; 3: function guest;
// 4: initial and later successful Command execs;
// 5/6: ordinary inject at initial exec, with success/refusal at the same stop.
#[derive(Default)]
struct BoundaryTool<const MODE: u8>;
#[reverie::tool]
impl<const MODE: u8> Tool for BoundaryTool<MODE> {
    type GlobalState = BoundaryGlobal;
    type ThreadState = State;

    fn subscriptions(_: &()) -> Subscription {
        [Sysno::execve].into_iter().collect()
    }

    async fn handle_initial_stop<G: Guest<Self>>(
        &self,
        guest: &mut G,
        observation: &dyn InitialCommandObservation,
    ) -> Result<(), Error> {
        assert_ne!(MODE, 3, "function guest cannot issue Command provenance");
        assert!(guest.is_command_bootstrap());
        assert_eq!(observation.root_tid(), guest.tid());
        assert_eq!(guest.pid(), guest.tid());
        assert_eq!(observation.former_tid(), None);
        let actual = instructions(observation.seccomp_filter());
        assert_eq!(
            actual,
            instructions(&seccomp_filter(&Self::subscriptions(&()), false))
        );
        let state = guest.thread_state_mut();
        assert_eq!((state.stopped, state.exec, state.post_exec), (0, 0, 0));
        state.stopped = 1;
        state.instructions = actual;
        if MODE == 1 {
            return Err(Error::Tool(anyhow::anyhow!("initial-stop-refusal")));
        }
        Ok(())
    }

    async fn handle_initial_exec<G: Guest<Self>>(
        &self,
        guest: &mut G,
        observation: &dyn InitialCommandObservation,
    ) -> Result<(), Error> {
        assert_ne!(MODE, 3, "function guest cannot issue Command provenance");
        assert!(!guest.is_command_bootstrap());
        assert_eq!(observation.root_tid(), guest.tid());
        assert_eq!(observation.former_tid(), Some(guest.tid()));
        assert_eq!(
            instructions(observation.seccomp_filter()),
            guest.thread_state().instructions
        );
        assert_eq!(
            (
                guest.thread_state().stopped,
                guest.thread_state().exec,
                guest.thread_state().post_exec
            ),
            (1, 0, 0)
        );
        let regs = guest.regs().await;
        if MODE != 4 {
            assert_eq!(
                regs.rip, ENTRY,
                "first executable instruction already stepped"
            );
        }
        guest.thread_state_mut().entry_ip = regs.rip;
        guest.thread_state_mut().exec = 1;
        if MODE == 2 || MODE == 6 {
            return Err(Error::Tool(anyhow::anyhow!("initial-exec-refusal")));
        }
        Ok(())
    }

    async fn handle_syscall_event<G: Guest<Self>>(
        &self,
        guest: &mut G,
        syscall: reverie::syscalls::Syscall,
    ) -> Result<i64, Error> {
        if MODE == 5 || MODE == 6 {
            assert_eq!(syscall.number(), Sysno::execve);
            assert!(guest.is_command_bootstrap());
            assert_eq!(guest.thread_state().ordinary_exec_injections, 0);
            guest.thread_state_mut().ordinary_exec_injections = 1;
            let result = guest.inject(syscall).await;
            panic!("successful native exec returned to its old Tool callback: {result:?}");
        }
        guest.tail_inject(syscall).await
    }

    async fn handle_post_exec<G: Guest<Self>>(&self, guest: &mut G) -> Result<(), Errno> {
        assert_eq!(
            (guest.thread_state().stopped, guest.thread_state().exec),
            (1, 1)
        );
        guest.thread_state_mut().post_exec += 1;
        Ok(())
    }

    async fn on_exit_thread<G: GlobalRPC<BoundaryGlobal>>(
        &self,
        tid: Pid,
        global: &G,
        state: State,
        status: ExitStatus,
    ) -> Result<(), Error> {
        global.send_rpc(Terminal { tid, state, status }).await;
        Ok(())
    }
}

// A sealed static x86-64 ET_EXEC, with no interpreter. Its first instruction
// is at ENTRY and its only effect is exit(42). Holding the memfd owns the image.
fn entry_image() -> File {
    let fd = unsafe {
        libc::memfd_create(
            c"initial-command-entry".as_ptr(),
            libc::MFD_CLOEXEC | libc::MFD_ALLOW_SEALING | libc::MFD_EXEC,
        )
    };
    assert!(fd >= 0, "create executable image: {}", Errno::last());
    let mut file = unsafe { File::from_raw_fd(fd) };
    let code: &[u8] = &[0xb8, 60, 0, 0, 0, 0xbf, 42, 0, 0, 0, 0x0f, 0x05];
    let size = 120 + code.len();
    let mut elf = vec![0u8; size];
    elf[..16].copy_from_slice(&[0x7f, b'E', b'L', b'F', 2, 1, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
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
    elf[120..].copy_from_slice(code);
    file.write_all(&elf).expect("write owned ELF");
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
    file
}

async fn command_case<const MODE: u8>() {
    let image = (MODE != 4).then(entry_image);
    let mut command = if MODE == 4 {
        Command::new("/bin/sh")
    } else {
        Command::new(format!(
            "/proc/{}/fd/{}",
            std::process::id(),
            image.as_ref().unwrap().as_raw_fd()
        ))
    };
    if MODE == 4 {
        command.args(["-c", "exec /bin/true"]);
    }
    let tracer = TracerBuilder::<BoundaryTool<MODE>>::new(command)
        .spawn()
        .await
        .expect("spawn initial-boundary command");
    let pid = tracer.guest_pid();
    let raw = unsafe { libc::syscall(libc::SYS_pidfd_open, pid.as_raw(), 0) };
    assert!(raw >= 0, "retain exact command pidfd: {}", Errno::last());
    let pidfd = unsafe { OwnedFd::from_raw_fd(raw as i32) };
    let terminals = tracer.gref.terminals.clone();
    let failures = tracer.gref.failures.clone();
    let result = tokio::time::timeout(Duration::from_secs(5), tracer.wait())
        .await
        .expect("initial-boundary command hung");
    // A boundary refusal is the session's one primary backend failure.
    assert_eq!(
        *failures.lock().unwrap(),
        match MODE {
            1 => vec!["ptrace initial stop"],
            2 | 6 => vec!["ptrace initial exec"],
            _ => vec![],
        }
    );
    let expected_status = if MODE == 1 || MODE == 2 || MODE == 6 {
        let error = result.expect_err("boundary refusal resumed the command");
        assert!(
            error.to_string().contains(if MODE == 1 {
                "initial-stop-refusal"
            } else {
                "initial-exec-refusal"
            }),
            "{error}"
        );
        ExitStatus::Signaled(Signal::SIGKILL, false)
    } else {
        let (status, _) = result.expect("initial-boundary tracing failed");
        let expected = ExitStatus::Exited(if MODE == 4 { 0 } else { 42 });
        assert_eq!(status, expected);
        expected
    };
    let terminal = terminals.lock().unwrap();
    assert_eq!(terminal.len(), 1, "one actual consuming terminal callback");
    assert_eq!(terminal[0].tid, pid);
    assert_eq!(terminal[0].status, expected_status);
    let state = &terminal[0].state;
    assert_eq!(
        (state.stopped, state.exec, state.post_exec),
        match MODE {
            1 => (1, 0, 0),
            2 | 6 => (1, 1, 0),
            4 => (1, 1, 2),
            _ => (1, 1, 1),
        }
    );
    if matches!(MODE, 0 | 2 | 5 | 6) {
        assert_eq!(state.entry_ip, ENTRY);
    }
    assert_eq!(
        state.ordinary_exec_injections,
        u32::from(MODE == 5 || MODE == 6)
    );
    assert_reaped(pid, &pidfd);
}

fn assert_reaped(pid: Pid, pidfd: &OwnedFd) {
    let mut poll = libc::pollfd {
        fd: pidfd.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    assert_eq!(unsafe { libc::poll(&mut poll, 1, 0) }, 1);
    assert_ne!(poll.revents & libc::POLLIN, 0);
    assert!(!std::path::Path::new(&format!("/proc/{pid}")).exists());
    let mut status = 0;
    assert_eq!(
        unsafe { libc::waitpid(pid.as_raw(), &mut status, libc::WNOHANG) },
        -1
    );
    assert_eq!(Errno::last(), Errno::ECHILD);
}

#[tokio::test(flavor = "current_thread")]
async fn exact_filter_and_exec_stop_precede_first_instruction() {
    command_case::<0>().await;
}
#[tokio::test(flavor = "current_thread")]
async fn initial_stop_refusal_cannot_resume_launcher() {
    command_case::<1>().await;
}
#[tokio::test(flavor = "current_thread")]
async fn initial_exec_refusal_preserves_error_and_actual_terminal() {
    command_case::<2>().await;
}
#[tokio::test(flavor = "current_thread")]
async fn later_exec_does_not_reissue_initial_command_observation() {
    command_case::<4>().await;
}

#[tokio::test(flavor = "current_thread")]
async fn function_guest_does_not_issue_command_observations() {
    let tracer = spawn_fn::<BoundaryTool<3>, _>(|| {})
        .await
        .expect("spawn function");
    let pid = tracer.guest_pid();
    let raw = unsafe { libc::syscall(libc::SYS_pidfd_open, pid.as_raw(), 0) };
    assert!(raw >= 0, "retain exact function pidfd: {}", Errno::last());
    let pidfd = unsafe { OwnedFd::from_raw_fd(raw as i32) };
    let terminals = tracer.gref.terminals.clone();
    let (status, _) = tokio::time::timeout(Duration::from_secs(5), tracer.wait())
        .await
        .expect("function control hung")
        .expect("function control failed");
    assert_eq!(status, ExitStatus::Exited(0));
    let terminal = terminals.lock().unwrap();
    assert_eq!(terminal.len(), 1);
    assert_eq!(terminal[0].tid, pid);
    assert_eq!(terminal[0].status, ExitStatus::Exited(0));
    let state = &terminal[0].state;
    assert_eq!((state.stopped, state.exec, state.post_exec), (0, 0, 0));
    assert_reaped(pid, &pidfd);
}

#[tokio::test(flavor = "current_thread")]
async fn ordinary_inject_initial_exec_reaches_original_boundary() {
    command_case::<5>().await;
}
#[tokio::test(flavor = "current_thread")]
async fn ordinary_inject_initial_exec_refusal_preserves_error() {
    command_case::<6>().await;
}
