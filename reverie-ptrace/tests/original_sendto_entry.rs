//! Native coverage for the exact-pending Tool injection used by Record Sendto.
//! https://github.com/rrnewton/reverie/issues/899
//! This supplies backend ENTRY/result evidence, not a provider TX receipt.
#![cfg(target_arch = "x86_64")]

use std::io::Read;
use std::net::TcpListener;
use std::net::TcpStream;
use std::os::fd::AsRawFd;
use std::os::fd::FromRawFd;
use std::os::fd::OwnedFd;
use std::sync::Mutex;

use reverie::Error;
use reverie::ExitStatus;
use reverie::GlobalRPC;
use reverie::GlobalTool;
use reverie::Guest;
use reverie::InjectedSyscallEvent;
use reverie::Pid;
use reverie::Signal;
use reverie::Subscription;
use reverie::Tool;
use reverie::syscalls::Syscall;
use reverie::syscalls::SyscallArgs;
use reverie::syscalls::SyscallInfo;
use reverie::syscalls::Sysno;
use reverie_ptrace::testing::test_fn_with_config;
use serde::Deserialize;
use serde::Serialize;

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize)]
enum Case {
    #[default]
    Results,
    ChangeRegister(usize),
    UnobservedChangedLength,
    CancelBefore,
    DieAtEntry,
    CancelAfterReturn,
}
#[derive(Default)]
struct Global;
#[reverie::global_tool]
impl GlobalTool for Global {
    type Request = ();
    type Response = ();
    type Config = Case;
    async fn receive_rpc(&self, _: Pid, _: ()) {}
}
#[derive(Default, Clone, Debug, Serialize, Deserialize)]
struct Thread {
    terminal: bool,
}
#[derive(Default)]
struct Observer {
    case: Case,
}
#[derive(Clone, Copy, Debug)]
struct Observation {
    tid: i32,
    args: SyscallArgs,
    event: InjectedSyscallEvent,
}
#[derive(Default)]
struct Evidence {
    actor: Option<(i32, OwnedFd)>,
    original: Vec<SyscallArgs>,
    events: Vec<Observation>,
    terminal: Vec<(i32, ExitStatus)>,
    consumed: Vec<(i32, ExitStatus)>,
    continuations: usize,
}
static SERIAL: Mutex<()> = Mutex::new(());
static EVIDENCE: Mutex<Option<Evidence>> = Mutex::new(None);

#[reverie::tool]
impl Tool for Observer {
    type GlobalState = Global;
    type ThreadState = Thread;
    fn new(_: Pid, case: &Case) -> Self {
        Self { case: *case }
    }
    fn subscriptions(_: &Case) -> Subscription {
        let mut set = Subscription::none();
        set.syscall(Sysno::sendto);
        set
    }
    fn observe_injected_syscalls(case: &Case) -> bool {
        !matches!(case, Case::UnobservedChangedLength)
    }
    fn observe_injected_syscall_preparation(_: &Case) -> bool {
        true
    }
    fn on_injected_syscall_observed(
        &self,
        tid: Pid,
        _: &Global,
        state: &mut Thread,
        nr: Sysno,
        args: SyscallArgs,
        event: InjectedSyscallEvent,
    ) {
        assert!(!state.terminal);
        assert_eq!(nr, Sysno::sendto);
        EVIDENCE
            .lock()
            .unwrap()
            .as_mut()
            .unwrap()
            .events
            .push(Observation {
                tid: tid.as_raw(),
                args,
                event,
            });
        if matches!(
            (self.case, event),
            (Case::DieAtEntry, InjectedSyscallEvent::Entered)
                | (Case::CancelAfterReturn, InjectedSyscallEvent::Returned(_))
        ) {
            assert_eq!(
                unsafe {
                    libc::syscall(libc::SYS_tgkill, tid.as_raw(), tid.as_raw(), libc::SIGKILL)
                },
                0
            );
        }
    }
    async fn handle_syscall_event<G: Guest<Self>>(
        &self,
        guest: &mut G,
        syscall: Syscall,
    ) -> Result<i64, Error> {
        let (nr, args) = syscall.into_parts();
        assert_eq!(nr, Sysno::sendto);
        {
            let mut guard = EVIDENCE.lock().unwrap();
            let evidence = guard.as_mut().unwrap();
            if evidence.actor.is_none() {
                let fd =
                    unsafe { libc::syscall(libc::SYS_pidfd_open, guest.tid().as_raw(), 0) } as i32;
                assert!(fd >= 0);
                evidence.actor = Some((guest.tid().as_raw(), unsafe { OwnedFd::from_raw_fd(fd) }));
            }
            evidence.original.push(args);
        }
        if let Case::ChangeRegister(index) = self.case {
            let mut regs = guest.regs().await;
            let register = match index {
                0 => &mut regs.rdi,
                1 => &mut regs.rsi,
                2 => &mut regs.rdx,
                3 => &mut regs.r10,
                4 => &mut regs.r8,
                5 => &mut regs.r9,
                6 => &mut regs.orig_rax,
                _ => unreachable!(),
            };
            *register ^= 1;
            guest.set_regs(regs).await?;
        }
        if matches!(self.case, Case::UnobservedChangedLength) {
            let mut regs = guest.regs().await;
            // Linux ignores addrlen with a NULL destination. Without observer
            // opt-in, preserve the existing same-call execution behavior.
            regs.r9 = 1;
            guest.set_regs(regs).await?;
        }
        if matches!(self.case, Case::CancelBefore) {
            let tid = guest.tid().as_raw();
            assert_eq!(
                unsafe { libc::syscall(libc::SYS_tgkill, tid, tid, libc::SIGKILL) },
                0
            );
            std::future::pending::<()>().await;
        }
        // Keep the original number and all six arguments. This must use the
        // exact-pending fast path, not an administrative private injection.
        let result = guest.inject(syscall).await;
        EVIDENCE.lock().unwrap().as_mut().unwrap().continuations += 1;
        if matches!(self.case, Case::CancelAfterReturn) {
            std::future::pending::<()>().await;
        }
        result.map_err(Into::into)
    }
    fn on_backend_thread_terminal(
        &self,
        tid: Pid,
        _: &Global,
        state: &mut Thread,
        status: ExitStatus,
    ) {
        assert!(!state.terminal);
        state.terminal = true;
        EVIDENCE
            .lock()
            .unwrap()
            .as_mut()
            .unwrap()
            .terminal
            .push((tid.as_raw(), status));
    }
    async fn on_exit_thread<G: GlobalRPC<Global>>(
        &self,
        tid: Pid,
        _: &G,
        state: Thread,
        status: ExitStatus,
    ) -> Result<(), Error> {
        assert!(state.terminal);
        let mut guard = EVIDENCE.lock().unwrap();
        let evidence = guard.as_mut().unwrap();
        assert_eq!(evidence.terminal, vec![(tid.as_raw(), status)]);
        evidence.consumed.push((tid.as_raw(), status));
        Ok(())
    }
}
fn start() {
    assert!(
        EVIDENCE
            .lock()
            .unwrap()
            .replace(Evidence::default())
            .is_none()
    );
}
fn finish(status: ExitStatus) -> Evidence {
    let evidence = EVIDENCE.lock().unwrap().take().unwrap();
    let (tid, fd) = evidence.actor.as_ref().unwrap();
    assert_eq!(evidence.terminal, vec![(*tid, status)]);
    assert_eq!(evidence.consumed, evidence.terminal);
    let mut poll = libc::pollfd {
        fd: fd.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    assert_eq!(unsafe { libc::poll(&mut poll, 1, 0) }, 1);
    assert_ne!(poll.revents & libc::POLLIN, 0);
    let mut native_status = 0;
    assert_eq!(
        unsafe { libc::waitpid(*tid, &mut native_status, libc::WNOHANG) },
        -1
    );
    assert_eq!(
        std::io::Error::last_os_error().raw_os_error(),
        Some(libc::ECHILD)
    );
    evidence
}
fn sockets() -> (TcpStream, TcpStream) {
    unsafe {
        libc::alarm(5);
    }
    let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
    let client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
    let (server, _) = listener.accept().unwrap();
    (client, server)
}
fn send(fd: i32, address: *const u8, count: usize) -> i64 {
    unsafe {
        libc::syscall(
            libc::SYS_sendto,
            fd,
            address,
            count,
            libc::MSG_NOSIGNAL,
            0usize,
            0usize,
        )
    }
}
fn assert_events(evidence: &Evidence, expected: &[InjectedSyscallEvent]) {
    assert_eq!(
        evidence
            .events
            .iter()
            .map(|row| row.event)
            .collect::<Vec<_>>(),
        expected
    );
    assert!(
        evidence
            .events
            .iter()
            .all(|row| row.tid == evidence.actor.as_ref().unwrap().0)
    );
}
#[test]
fn original_sendto_authenticates_entry_before_success_zero_and_fault_results() {
    let _serial = SERIAL.lock().unwrap();
    start();
    let (output, _) = test_fn_with_config::<Observer, _>(
        || {
            let (client, mut server) = sockets();
            let payload = [0x5bu8; 79];
            assert_eq!(
                send(client.as_raw_fd(), payload.as_ptr(), payload.len()),
                79
            );
            let mut actual = [0u8; 79];
            server.read_exact(&mut actual).unwrap();
            assert_eq!(actual, payload);
            assert_eq!(send(client.as_raw_fd(), payload.as_ptr(), 0), 0);
            assert_eq!(send(client.as_raw_fd(), std::ptr::null(), 79), -1);
            assert_eq!(
                std::io::Error::last_os_error().raw_os_error(),
                Some(libc::EFAULT)
            );
            drop(client);
            let mut extra = [0u8; 1];
            assert_eq!(
                server.read(&mut extra).unwrap(),
                0,
                "no duplicate native Sendto"
            );
        },
        Case::Results,
        true,
    )
    .expect("actual original TCP Sendto");
    assert_eq!(
        output.status,
        ExitStatus::Exited(0),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let evidence = finish(output.status);
    assert_eq!(evidence.continuations, 3);
    let expected: Vec<_> = [79, 0, -i64::from(libc::EFAULT)]
        .into_iter()
        .flat_map(|raw| {
            [
                InjectedSyscallEvent::Prepared,
                InjectedSyscallEvent::Entered,
                InjectedSyscallEvent::Returned(raw),
            ]
        })
        .collect();
    assert_events(&evidence, &expected);
    for (index, rows) in evidence.events.chunks_exact(3).enumerate() {
        assert!(rows.iter().all(|row| row.args == evidence.original[index]));
        assert_eq!(rows[0].args.arg3, libc::MSG_NOSIGNAL as usize);
        assert_eq!((rows[0].args.arg4, rows[0].args.arg5), (0, 0));
    }
}
#[test]
fn original_sendto_rejects_changed_number_or_any_argument_before_entry() {
    let _serial = SERIAL.lock().unwrap();
    for index in 0..7 {
        start();
        let result = test_fn_with_config::<Observer, _>(
            || {
                let (client, _) = sockets();
                send(client.as_raw_fd(), [3u8; 79].as_ptr(), 79);
                unsafe {
                    libc::_exit(99);
                }
            },
            Case::ChangeRegister(index),
            true,
        );
        let error = result
            .map(|_| ())
            .expect_err("changed original entry must fail the backend");
        assert!(
            matches!(error, Error::Errno(reverie::Errno::EPROTO)),
            "{error:?}"
        );
        let evidence = finish(ExitStatus::Signaled(Signal::SIGKILL, false));
        assert_eq!(evidence.continuations, 0);
        assert_events(&evidence, &[InjectedSyscallEvent::Prepared]);
    }
}
#[test]
fn original_sendto_cancellation_keeps_entry_distinct_from_result() {
    let _serial = SERIAL.lock().unwrap();
    for case in [
        Case::CancelBefore,
        Case::DieAtEntry,
        Case::CancelAfterReturn,
    ] {
        start();
        let (output, _) = test_fn_with_config::<Observer, _>(
            || {
                let (client, _server) = sockets();
                send(client.as_raw_fd(), [7u8; 79].as_ptr(), 79);
                unsafe {
                    libc::_exit(99);
                }
            },
            case,
            true,
        )
        .expect("actual cancellation and final wait");
        assert_eq!(output.status, ExitStatus::Signaled(Signal::SIGKILL, false));
        let evidence = finish(output.status);
        let expected = match case {
            Case::CancelBefore => vec![],
            Case::DieAtEntry => vec![
                InjectedSyscallEvent::Prepared,
                InjectedSyscallEvent::Entered,
            ],
            Case::CancelAfterReturn => vec![
                InjectedSyscallEvent::Prepared,
                InjectedSyscallEvent::Entered,
                InjectedSyscallEvent::Returned(79),
            ],
            _ => unreachable!(),
        };
        assert_events(&evidence, &expected);
        assert_eq!(
            evidence.continuations,
            usize::from(matches!(case, Case::CancelAfterReturn))
        );
    }
}

#[test]
fn original_sendto_without_observation_preserves_existing_register_behavior() {
    let _serial = SERIAL.lock().unwrap();
    start();
    let (output, _) = test_fn_with_config::<Observer, _>(
        || {
            let (client, mut server) = sockets();
            let payload = [0x63u8; 79];
            assert_eq!(
                send(client.as_raw_fd(), payload.as_ptr(), payload.len()),
                79
            );
            let mut actual = [0u8; 79];
            server.read_exact(&mut actual).unwrap();
            assert_eq!(actual, payload);
            drop(client);
            assert_eq!(server.read(&mut [0u8; 1]).unwrap(), 0);
        },
        Case::UnobservedChangedLength,
        true,
    )
    .expect("unobserved original Sendto");
    assert_eq!(output.status, ExitStatus::Exited(0));
    let evidence = finish(output.status);
    assert_eq!(evidence.continuations, 1);
    assert_events(&evidence, &[]);
}
