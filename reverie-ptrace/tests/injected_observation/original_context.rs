//! Actual ptrace stops and Linux results, without a provider or copy receipt.
//! EBADF verifies the executed dummy shape only; op13 must attest its own copy.
use std::sync::atomic::AtomicI32;
use std::sync::atomic::AtomicU64;

use reverie::syscalls::EpollCtl;
use reverie::syscalls::SyscallInfo;

use super::*;

#[derive(Clone, Copy, Default, Debug, Eq, PartialEq, Serialize, Deserialize)]
enum EntryCase {
    #[default]
    Shapes,
    Signal,
    Filter,
    WrongCall,
    WrongNumber,
    LostEntry,
    ChangedRegisters,
    DeathAtEntry,
    CancelAfterReturn,
    CancelAfterToolConsumption,
}
#[derive(Default)]
struct EntryGlobal;
#[reverie::global_tool]
impl GlobalTool for EntryGlobal {
    type Request = ();
    type Response = ();
    type Config = EntryCase;
    async fn receive_rpc(&self, _: Pid, _: ()) {}
}
#[derive(Default, Clone, Debug, Serialize, Deserialize)]
struct EntryThread {
    terminal: bool,
}
#[derive(Default)]
struct EntryObserver {
    case: EntryCase,
}
impl AsMut<EntryObserver> for EntryObserver {
    fn as_mut(&mut self) -> &mut EntryObserver {
        self
    }
}
impl AsRef<EntryThread> for EntryThread {
    fn as_ref(&self) -> &EntryThread {
        self
    }
}
impl AsMut<EntryThread> for EntryThread {
    fn as_mut(&mut self) -> &mut EntryThread {
        self
    }
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum KillBoundary {
    Entered,
    Returned,
    ToolConsumed,
}
#[derive(Default)]
struct Evidence {
    actor: Option<(i32, OwnedFd)>,
    original: Vec<SyscallArgs>,
    events: Vec<Observation>,
    terminals: Vec<(i32, ExitStatus)>,
    consumed: Vec<(i32, ExitStatus)>,
    signals: usize,
    continued: usize,
    kills: Vec<KillBoundary>,
    consumed_results: Vec<i64>,
}
static SERIAL: Mutex<()> = Mutex::new(());
static EVIDENCE: Mutex<Option<Evidence>> = Mutex::new(None);

#[reverie::tool]
impl Tool for EntryObserver {
    type GlobalState = EntryGlobal;
    type ThreadState = EntryThread;
    fn new(_: Pid, case: &EntryCase) -> Self {
        Self { case: *case }
    }
    fn subscriptions(case: &EntryCase) -> Subscription {
        let mut set = Subscription::none();
        set.syscall(if *case == EntryCase::WrongNumber {
            Sysno::getuid
        } else {
            Sysno::epoll_ctl
        });
        set
    }
    fn observe_injected_syscalls(_: &EntryCase) -> bool {
        true
    }
    fn observe_injected_syscall_preparation(_: &EntryCase) -> bool {
        true
    }
    fn on_injected_syscall_observed(
        &self,
        tid: Pid,
        _: &EntryGlobal,
        state: &mut EntryThread,
        nr: Sysno,
        args: SyscallArgs,
        event: InjectedSyscallEvent,
    ) {
        assert!(!state.terminal);
        EVIDENCE
            .lock()
            .unwrap()
            .as_mut()
            .unwrap()
            .events
            .push(Observation {
                tid: tid.as_raw(),
                nr,
                args,
                event,
            });
        let signal = match (self.case, event) {
            (EntryCase::Signal, InjectedSyscallEvent::Entered) => Some(libc::SIGUSR1),
            (EntryCase::DeathAtEntry, InjectedSyscallEvent::Entered)
            | (EntryCase::CancelAfterReturn, InjectedSyscallEvent::Returned(_)) => {
                Some(libc::SIGKILL)
            }
            _ => None,
        };
        if nr == Sysno::epoll_ctl
            && let Some(signal) = signal
        {
            if self.case == EntryCase::CancelAfterReturn {
                assert_eq!(
                    event,
                    InjectedSyscallEvent::Returned(-i64::from(libc::EBADF))
                );
            }
            assert_eq!(
                unsafe { libc::syscall(libc::SYS_tgkill, tid.as_raw(), tid.as_raw(), signal) },
                0
            );
            if signal == libc::SIGKILL {
                let boundary = match self.case {
                    EntryCase::DeathAtEntry => KillBoundary::Entered,
                    EntryCase::CancelAfterReturn => KillBoundary::Returned,
                    _ => unreachable!(),
                };
                let mut guard = EVIDENCE.lock().unwrap();
                let evidence = guard.as_mut().unwrap();
                assert!(evidence.kills.is_empty());
                evidence.kills.push(boundary);
            }
        }
    }
    async fn handle_syscall_event<G: Guest<Self>>(
        &self,
        guest: &mut G,
        syscall: Syscall,
    ) -> Result<i64, Error> {
        let (nr, args) = syscall.into_parts();
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
        let original = guest.regs().await;
        let mut requested = args;
        match self.case {
            EntryCase::WrongCall => requested.arg5 ^= 1usize << 40,
            EntryCase::LostEntry => {
                assert!(guest.inject(Getpid::new()).await.unwrap() > 0);
            }
            EntryCase::ChangedRegisters => {
                let mut changed = original;
                changed.r9 ^= 1u64 << 40;
                guest.set_regs(changed).await?;
            }
            _ => {}
        }
        let call = if nr == Sysno::epoll_ctl {
            EpollCtl::from(requested)
        } else {
            assert_eq!(self.case, EntryCase::WrongNumber);
            EpollCtl::new()
                .with_epfd(17)
                .with_op(libc::EPOLL_CTL_ADD)
                .with_fd(19)
        };
        // Exercise forwarding through IntoGuest too; no alternative executor.
        let result = Guest::<Self>::inject_epoll_ctl_copy(&mut guest.into_guest(), call).await;
        EVIDENCE.lock().unwrap().as_mut().unwrap().continued += 1;
        if self.case == EntryCase::CancelAfterReturn {
            std::future::pending::<()>().await;
            unreachable!();
        }
        let restored = guest.regs().await;
        assert_eq!(restored.orig_rax, original.orig_rax);
        assert_eq!(restored.rip, original.rip);
        assert_eq!(restored.rsp, original.rsp);
        assert_eq!(
            [
                restored.rdi,
                restored.rsi,
                restored.rdx,
                restored.r10,
                restored.r8,
                restored.r9
            ],
            [
                original.rdi,
                original.rsi,
                original.rdx,
                original.r10,
                original.r8,
                original.r9
            ]
        );
        assert_eq!(restored.rcx, original.rcx);
        assert_eq!(restored.r11, original.r11);
        assert!(matches!(result, Err(Error::Errno(_))));
        if self.case == EntryCase::CancelAfterToolConsumption {
            assert!(matches!(&result, Err(Error::Errno(error)) if *error == reverie::Errno::EBADF));
            let mut expected = original;
            expected.rax = (-i64::from(libc::EBADF)) as u64;
            assert_eq!(register_image(&restored), register_image(&expected));
            {
                let mut guard = EVIDENCE.lock().unwrap();
                let evidence = guard.as_mut().unwrap();
                assert_eq!(evidence.continued, 1);
                assert!(evidence.kills.is_empty());
                assert_eq!(
                    evidence.events.last().unwrap().event,
                    InjectedSyscallEvent::Returned(-i64::from(libc::EBADF))
                );
                evidence.consumed_results.push(-i64::from(libc::EBADF));
            }
            // This kill is causally after the typed API returned and every
            // original register was checked. It cannot cancel consumption.
            let tid = guest.tid().as_raw();
            assert_eq!(
                unsafe { libc::syscall(libc::SYS_tgkill, tid, tid, libc::SIGKILL) },
                0
            );
            EVIDENCE
                .lock()
                .unwrap()
                .as_mut()
                .unwrap()
                .kills
                .push(KillBoundary::ToolConsumed);
            std::future::pending::<()>().await;
            unreachable!();
        }
        result
    }
    async fn handle_signal_event<G: Guest<Self>>(
        &self,
        guest: &mut G,
        signal: Signal,
    ) -> Result<Option<Signal>, reverie::Errno> {
        if signal == Signal::SIGUSR1 {
            let regs = guest.regs().await;
            let mut guard = EVIDENCE.lock().unwrap();
            let evidence = guard.as_mut().unwrap();
            evidence.signals += 1;
            assert_eq!(evidence.signals, 1);
            assert_eq!(
                evidence.events.last().unwrap().event,
                InjectedSyscallEvent::Returned(-(libc::EBADF as i64))
            );
            let original = evidence.original.last().unwrap();
            assert_eq!(
                [regs.rdi, regs.rsi, regs.rdx, regs.r10, regs.r8, regs.r9],
                [
                    original.arg0 as u64,
                    original.arg1 as u64,
                    original.arg2 as u64,
                    original.arg3 as u64,
                    original.arg4 as u64,
                    original.arg5 as u64
                ]
            );
            assert_eq!(regs.orig_rax, Sysno::epoll_ctl as u64);
        }
        Ok(Some(signal))
    }
    fn on_backend_thread_terminal(
        &self,
        tid: Pid,
        _: &EntryGlobal,
        state: &mut EntryThread,
        status: ExitStatus,
    ) {
        assert!(!state.terminal);
        state.terminal = true;
        EVIDENCE
            .lock()
            .unwrap()
            .as_mut()
            .unwrap()
            .terminals
            .push((tid.as_raw(), status));
    }
    async fn on_exit_thread<G: GlobalRPC<EntryGlobal>>(
        &self,
        tid: Pid,
        _: &G,
        state: EntryThread,
        status: ExitStatus,
    ) -> Result<(), Error> {
        assert!(state.terminal);
        let mut guard = EVIDENCE.lock().unwrap();
        let evidence = guard.as_mut().unwrap();
        assert_eq!(evidence.terminals, vec![(tid.as_raw(), status)]);
        evidence.consumed.push((tid.as_raw(), status));
        Ok(())
    }
}

fn register_image(regs: &libc::user_regs_struct) -> [u64; 27] {
    [
        regs.r15,
        regs.r14,
        regs.r13,
        regs.r12,
        regs.rbp,
        regs.rbx,
        regs.r11,
        regs.r10,
        regs.r9,
        regs.r8,
        regs.rax,
        regs.rcx,
        regs.rdx,
        regs.rsi,
        regs.rdi,
        regs.orig_rax,
        regs.rip,
        regs.cs,
        regs.eflags,
        regs.rsp,
        regs.ss,
        regs.fs_base,
        regs.gs_base,
        regs.ds,
        regs.es,
        regs.fs,
        regs.gs,
    ]
}

fn finish_evidence(status: ExitStatus) -> Evidence {
    let evidence = EVIDENCE.lock().unwrap().take().unwrap();
    let (tid, fd) = evidence.actor.as_ref().unwrap();
    assert_eq!(evidence.terminals, vec![(*tid, status)]);
    assert_eq!(evidence.consumed, evidence.terminals);
    let mut poll = libc::pollfd {
        fd: fd.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    assert_eq!(unsafe { libc::poll(&mut poll, 1, 0) }, 1);
    assert_eq!(poll.revents & libc::POLLHUP, libc::POLLHUP);
    assert_eq!(unsafe { libc::kill(*tid, 0) }, -1);
    assert_eq!(
        std::io::Error::last_os_error().raw_os_error(),
        Some(libc::ESRCH)
    );
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
fn start() {
    assert!(
        EVIDENCE
            .lock()
            .unwrap()
            .replace(Evidence::default())
            .is_none()
    );
}
const TAIL4: usize = 0x9abc_def0_1234_5678;
const TAIL5: usize = 0x8765_4321_fedc_ba98;
fn ctl(epfd: i32, op: i32, fd: i32, event: *mut libc::epoll_event) -> i32 {
    unsafe {
        libc::syscall(
            libc::SYS_epoll_ctl,
            epfd as i64,
            op as i64,
            fd as i64,
            event,
            TAIL4,
            TAIL5,
        ) as i32
    }
}
fn descriptors() -> (OwnedFd, OwnedFd) {
    unsafe {
        libc::alarm(5);
    }
    let ep = unsafe { libc::epoll_create1(libc::EPOLL_CLOEXEC) };
    assert!(ep >= 0);
    let fd = unsafe { libc::eventfd(1, libc::EFD_CLOEXEC | libc::EFD_NONBLOCK) };
    assert!(fd >= 0);
    unsafe { (OwnedFd::from_raw_fd(ep), OwnedFd::from_raw_fd(fd)) }
}
fn require_results(evidence: &Evidence, results: &[i64]) {
    let rows: Vec<_> = evidence
        .events
        .iter()
        .filter(|event| event.nr == Sysno::epoll_ctl)
        .collect();
    assert_eq!(rows.len(), results.len() * 3);
    assert_eq!(evidence.continued, results.len());
    let (groups, remainder) = rows.as_chunks::<3>();
    assert!(remainder.is_empty());
    for (index, (&raw, rows)) in results.iter().zip(groups).enumerate() {
        assert_eq!(
            rows.iter().map(|row| row.event).collect::<Vec<_>>(),
            vec![
                InjectedSyscallEvent::Prepared,
                InjectedSyscallEvent::Entered,
                InjectedSyscallEvent::Returned(raw),
            ]
        );
        let mut expected = evidence.original[index];
        expected.arg0 = usize::MAX;
        expected.arg2 = usize::MAX;
        assert!(rows.iter().all(|row| row.args == expected));
        assert_eq!(expected.arg4, TAIL4);
        assert_eq!(expected.arg5, TAIL5);
    }
}
#[test]
fn original_epoll_copy_preserves_tuple_and_linux_twelve_byte_fault_boundaries() {
    let _serial = SERIAL.lock().unwrap();
    start();
    let (output, _) = test_fn_with_config::<EntryObserver, _>(
        || {
            let (ep, fd) = descriptors();
            let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) } as usize;
            let mapping = unsafe {
                libc::mmap(
                    std::ptr::null_mut(),
                    page * 2,
                    libc::PROT_READ | libc::PROT_WRITE,
                    libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
                    -1,
                    0,
                )
            };
            assert_ne!(mapping, libc::MAP_FAILED);
            let event = unsafe {
                (mapping as *mut u8)
                    .add(page - 12)
                    .cast::<libc::epoll_event>()
            };
            let expected = libc::epoll_event {
                events: libc::EPOLLIN as u32,
                u64: 0xfedc_ba98_7654_3210,
            };
            unsafe {
                event.write_unaligned(expected);
            }
            assert_eq!(
                unsafe {
                    libc::mprotect((mapping as *mut u8).add(page).cast(), page, libc::PROT_NONE)
                },
                0
            );
            for op in [libc::EPOLL_CTL_ADD, libc::EPOLL_CTL_MOD] {
                assert_eq!(ctl(ep.as_raw_fd(), op, fd.as_raw_fd(), event), -1);
                assert_eq!(
                    std::io::Error::last_os_error().raw_os_error(),
                    Some(libc::EBADF)
                );
                let actual = unsafe { event.read_unaligned() };
                let (bits, data) = (actual.events, actual.u64);
                assert_eq!(bits, libc::EPOLLIN as u32);
                assert_eq!(data, 0xfedc_ba98_7654_3210);
            }
            assert_eq!(
                ctl(
                    ep.as_raw_fd(),
                    libc::EPOLL_CTL_DEL,
                    fd.as_raw_fd(),
                    std::ptr::null_mut()
                ),
                -1
            );
            assert_eq!(
                std::io::Error::last_os_error().raw_os_error(),
                Some(libc::EBADF)
            );
            for pointer in [
                std::ptr::null_mut(),
                unsafe { (mapping as *mut u8).add(page - 4).cast() },
                unsafe { (mapping as *mut u8).add(page - 11).cast() },
            ] {
                assert_eq!(
                    ctl(ep.as_raw_fd(), libc::EPOLL_CTL_ADD, fd.as_raw_fd(), pointer),
                    -1
                );
                assert_eq!(
                    std::io::Error::last_os_error().raw_os_error(),
                    Some(libc::EFAULT)
                );
            }
            let mut ready = expected;
            assert_eq!(
                unsafe { libc::epoll_wait(ep.as_raw_fd(), &mut ready, 1, 0) },
                0,
                "copy-only execution installed an epoll registration"
            );
            assert_eq!(unsafe { libc::munmap(mapping, page * 2) }, 0);
        },
        EntryCase::Shapes,
        true,
    )
    .expect("actual original epoll copies");
    assert_eq!(output.status, ExitStatus::Exited(0));
    let evidence = finish_evidence(output.status);
    require_results(
        &evidence,
        &[
            -i64::from(libc::EBADF),
            -i64::from(libc::EBADF),
            -i64::from(libc::EBADF),
            -i64::from(libc::EFAULT),
            -i64::from(libc::EFAULT),
            -i64::from(libc::EFAULT),
        ],
    );
}

static SIGNAL_COUNT: AtomicI32 = AtomicI32::new(0);
static SIGNAL_FD: AtomicI32 = AtomicI32::new(-1);
static SIGNAL_EPFD: AtomicI32 = AtomicI32::new(-1);
static SIGNAL_POINTER: AtomicU64 = AtomicU64::new(0);
static SIGNAL_SENDER: AtomicI32 = AtomicI32::new(-1);
extern "C" fn signal_handler(signo: i32, info: *mut libc::siginfo_t, context: *mut libc::c_void) {
    unsafe {
        let regs = &(*(context as *const libc::ucontext_t)).uc_mcontext.gregs;
        let valid = signo == libc::SIGUSR1
            && (*info).si_code == libc::SI_TKILL
            && (*info).si_pid() == SIGNAL_SENDER.load(Ordering::SeqCst)
            && regs[libc::REG_RDI as usize] == i64::from(SIGNAL_EPFD.load(Ordering::SeqCst))
            && regs[libc::REG_RDX as usize] == i64::from(SIGNAL_FD.load(Ordering::SeqCst))
            && regs[libc::REG_RSI as usize] == i64::from(libc::EPOLL_CTL_ADD)
            && regs[libc::REG_R10 as usize] as u64 == SIGNAL_POINTER.load(Ordering::SeqCst)
            && regs[libc::REG_R8 as usize] as usize == TAIL4
            && regs[libc::REG_R9 as usize] as usize == TAIL5
            && regs[libc::REG_RAX as usize] == -i64::from(libc::EBADF);
        if !valid {
            libc::_exit(93);
        }
        SIGNAL_COUNT.fetch_add(1, Ordering::SeqCst);
    }
}
#[test]
fn original_epoll_copy_restores_logical_signal_context_and_siginfo() {
    let _serial = SERIAL.lock().unwrap();
    start();
    let tracer_pid = unsafe { libc::getpid() };
    let (output, _) = test_fn_with_config::<EntryObserver, _>(
        move || {
            let (ep, fd) = descriptors();
            let mut event = libc::epoll_event {
                events: libc::EPOLLIN as u32,
                u64: u64::MAX - 5,
            };
            SIGNAL_EPFD.store(ep.as_raw_fd(), Ordering::SeqCst);
            SIGNAL_FD.store(fd.as_raw_fd(), Ordering::SeqCst);
            SIGNAL_POINTER.store(
                (&mut event as *mut libc::epoll_event) as u64,
                Ordering::SeqCst,
            );
            SIGNAL_SENDER.store(tracer_pid, Ordering::SeqCst);
            SIGNAL_COUNT.store(0, Ordering::SeqCst);
            let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
            action.sa_sigaction = signal_handler as *const () as libc::sighandler_t;
            action.sa_flags = libc::SA_SIGINFO | libc::SA_RESTART;
            assert_eq!(unsafe { libc::sigemptyset(&mut action.sa_mask) }, 0);
            assert_eq!(
                unsafe { libc::sigaction(libc::SIGUSR1, &action, std::ptr::null_mut()) },
                0
            );
            assert_eq!(
                ctl(
                    ep.as_raw_fd(),
                    libc::EPOLL_CTL_ADD,
                    fd.as_raw_fd(),
                    &mut event
                ),
                -1
            );
            assert_eq!(
                std::io::Error::last_os_error().raw_os_error(),
                Some(libc::EBADF)
            );
            assert_eq!(SIGNAL_COUNT.load(Ordering::SeqCst), 1);
        },
        EntryCase::Signal,
        true,
    )
    .expect("actual signal after copy result");
    assert_eq!(output.status, ExitStatus::Exited(0));
    let evidence = finish_evidence(output.status);
    assert_eq!(evidence.signals, 1);
    require_results(&evidence, &[-i64::from(libc::EBADF)]);
}

#[test]
fn original_epoll_copy_obeys_filter_recheck_of_changed_operands() {
    let _serial = SERIAL.lock().unwrap();
    start();
    let (output, _) = test_fn_with_config::<EntryObserver, _>(
        || {
            let (ep, fd) = descriptors();
            // Allow the original positive epfd; reject only the backend's changed
            // -1 operand. Existing Tool TRACE remains active on the original tuple.
            let mut filter = [
                libc::sock_filter {
                    code: 0x20,
                    jt: 0,
                    jf: 0,
                    k: 0,
                },
                libc::sock_filter {
                    code: 0x15,
                    jt: 0,
                    jf: 3,
                    k: libc::SYS_epoll_ctl as u32,
                },
                libc::sock_filter {
                    code: 0x20,
                    jt: 0,
                    jf: 0,
                    k: 16,
                },
                libc::sock_filter {
                    code: 0x15,
                    jt: 0,
                    jf: 1,
                    k: u32::MAX,
                },
                libc::sock_filter {
                    code: 0x06,
                    jt: 0,
                    jf: 0,
                    k: 0x0005_0000 | libc::EACCES as u32,
                },
                libc::sock_filter {
                    code: 0x06,
                    jt: 0,
                    jf: 0,
                    k: 0x7fff_0000,
                },
            ];
            let program = libc::sock_fprog {
                len: filter.len() as u16,
                filter: filter.as_mut_ptr(),
            };
            assert_eq!(
                unsafe { libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) },
                0
            );
            assert_eq!(
                unsafe { libc::syscall(libc::SYS_seccomp, 1, 0, &program) },
                0
            );
            // NULL would be EFAULT if do_epoll_ctl's copy ran. Real EACCES must
            // come from the recheck; the backend never interprets it as a copy.
            assert_eq!(
                ctl(
                    ep.as_raw_fd(),
                    libc::EPOLL_CTL_ADD,
                    fd.as_raw_fd(),
                    std::ptr::null_mut()
                ),
                -1
            );
            assert_eq!(
                std::io::Error::last_os_error().raw_os_error(),
                Some(libc::EACCES)
            );
        },
        EntryCase::Filter,
        true,
    )
    .expect("actual seccomp recheck");
    assert_eq!(output.status, ExitStatus::Exited(0));
    require_results(&finish_evidence(output.status), &[-i64::from(libc::EACCES)]);
}

#[test]
fn original_epoll_copy_refuses_wrong_lost_and_changed_entry_without_helper() {
    let _serial = SERIAL.lock().unwrap();
    for case in [
        EntryCase::WrongCall,
        EntryCase::WrongNumber,
        EntryCase::LostEntry,
        EntryCase::ChangedRegisters,
    ] {
        start();
        let result = test_fn_with_config::<EntryObserver, _>(
            move || {
                let (ep, fd) = descriptors();
                if case == EntryCase::WrongNumber {
                    unsafe {
                        libc::syscall(libc::SYS_getuid);
                    }
                } else {
                    ctl(
                        ep.as_raw_fd(),
                        libc::EPOLL_CTL_ADD,
                        fd.as_raw_fd(),
                        std::ptr::null_mut(),
                    );
                }
                unsafe {
                    libc::_exit(99);
                }
            },
            case,
            true,
        );
        let error = result
            .map(|_| ())
            .expect_err("missing authenticated entry cannot be a guest errno");
        // run_to_terminal retains the original backend EPROTO as the outer
        // run error; the Tool never returned it to the guest as syscall errno.
        assert!(
            matches!(error, Error::Errno(reverie::Errno::EPROTO)),
            "backend error kind: {error:?}"
        );
        let evidence = finish_evidence(ExitStatus::Signaled(Signal::SIGKILL, false));
        assert_eq!(evidence.continued, 0);
        assert!(
            evidence
                .events
                .iter()
                .all(|event| event.nr != Sysno::epoll_ctl)
        );
        assert_eq!(evidence.original.len(), 1);
        if case == EntryCase::LostEntry {
            assert!(evidence.events.iter().any(|event| event.nr == Sysno::getpid
                && matches!(event.event, InjectedSyscallEvent::Returned(raw) if raw > 0)));
        } else {
            assert!(evidence.events.is_empty());
        }
    }
}

#[test]
fn original_epoll_copy_death_has_no_return_and_cancellation_keeps_actual_return() {
    let _serial = SERIAL.lock().unwrap();
    for case in [EntryCase::DeathAtEntry, EntryCase::CancelAfterReturn] {
        start();
        let (output, _) = test_fn_with_config::<EntryObserver, _>(
            || {
                let (ep, fd) = descriptors();
                let mut event = libc::epoll_event {
                    events: libc::EPOLLIN as u32,
                    u64: 0x1234_5678_9abc_def0,
                };
                ctl(
                    ep.as_raw_fd(),
                    libc::EPOLL_CTL_ADD,
                    fd.as_raw_fd(),
                    &mut event,
                );
                unsafe {
                    libc::_exit(99);
                }
            },
            case,
            true,
        )
        .expect("actual native death drains owned tracee");
        assert_eq!(output.status, ExitStatus::Signaled(Signal::SIGKILL, false));
        let evidence = finish_evidence(output.status);
        let events: Vec<_> = evidence.events.iter().map(|event| event.event).collect();
        let mut expected = vec![
            InjectedSyscallEvent::Prepared,
            InjectedSyscallEvent::Entered,
        ];
        if case == EntryCase::CancelAfterReturn {
            expected.push(InjectedSyscallEvent::Returned(-i64::from(libc::EBADF)));
            // Returned precedes cancellable register restoration and API
            // continuation. Its exact receipt must survive this earlier kill;
            // the separate ToolConsumed control requires continued == 1.
            assert_eq!(evidence.kills, vec![KillBoundary::Returned]);
        } else {
            assert_eq!(evidence.continued, 0);
            assert_eq!(evidence.kills, vec![KillBoundary::Entered]);
        }
        assert_eq!(events, expected);
        assert!(evidence.consumed_results.is_empty());
        assert_eq!(evidence.original.len(), 1);
        let mut expected_args = evidence.original[0];
        expected_args.arg0 = usize::MAX;
        expected_args.arg2 = usize::MAX;
        assert!(
            evidence
                .events
                .iter()
                .all(|row| row.tid == evidence.actor.as_ref().unwrap().0
                    && row.nr == Sysno::epoll_ctl
                    && row.args == expected_args)
        );
    }
}

#[test]
fn original_epoll_copy_cancellation_after_tool_consumption_keeps_result_and_restored_registers() {
    let _serial = SERIAL.lock().unwrap();
    start();
    let (output, _) = test_fn_with_config::<EntryObserver, _>(
        || {
            let (ep, fd) = descriptors();
            let mut event = libc::epoll_event {
                events: libc::EPOLLIN as u32,
                u64: 0x1234_5678_9abc_def0,
            };
            ctl(
                ep.as_raw_fd(),
                libc::EPOLL_CTL_ADD,
                fd.as_raw_fd(),
                &mut event,
            );
            unsafe {
                libc::_exit(99);
            }
        },
        EntryCase::CancelAfterToolConsumption,
        true,
    )
    .expect("actual death after Tool result consumption");
    assert_eq!(output.status, ExitStatus::Signaled(Signal::SIGKILL, false));
    let evidence = finish_evidence(output.status);
    assert_eq!(evidence.continued, 1);
    assert_eq!(evidence.consumed_results, vec![-i64::from(libc::EBADF)]);
    assert_eq!(evidence.kills, vec![KillBoundary::ToolConsumed]);
    require_results(&evidence, &[-i64::from(libc::EBADF)]);
}
