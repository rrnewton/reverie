//! Real backend boundary controls. This Tool has no provider or Detcore Call;
//! its handback explicitly exercises only the backend half of the contract.
use std::sync::atomic::AtomicI32;

use reverie::InjectedReadResult;
use reverie::syscalls::AddrMut;
use reverie::syscalls::MemoryAccess;
use reverie::syscalls::Read;

use super::*;

#[derive(Clone, Copy, Default, Debug, Eq, PartialEq, Serialize, Deserialize)]
enum ReadCase {
    #[default]
    ReadyAtOriginalEntry,
    PriorProgress,
    Partial,
    Restart,
    AdjustedReady,
    AdjustedPartial,
    AdjustedRestart,
    AdjustedRestartable,
    PrivateRetryInterrupted,
    PrivateRetryRestartable,
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
enum ReadRequest {
    BeforeHandback,
    SignalHook,
    Consumed,
}
#[derive(Default)]
struct ReadLog {
    inner: Log,
    handback: AtomicBool,
    signal_hooks: AtomicI32,
    pidfd: Mutex<Option<OwnedFd>>,
}
#[reverie::global_tool]
impl GlobalTool for ReadLog {
    type Request = ReadRequest;
    type Response = bool;
    type Config = ReadCase;
    async fn receive_rpc(&self, from: Pid, request: ReadRequest) -> bool {
        match request {
            ReadRequest::BeforeHandback => {
                let observations = self.inner.observations.lock().unwrap();
                let last = observations
                    .iter()
                    .rev()
                    .find(|row| row.tid == from.as_raw() && row.nr == Sysno::read)
                    .unwrap();
                assert_eq!(last.event, InjectedSyscallEvent::InterruptedBeforeEntry);
                assert_eq!(self.signal_hooks.load(Ordering::SeqCst), 0);
                assert!(!self.handback.swap(true, Ordering::SeqCst));
            }
            ReadRequest::SignalHook => {
                assert_eq!(self.signal_hooks.fetch_add(1, Ordering::SeqCst), 0);
            }
            ReadRequest::Consumed => {
                assert!(
                    self.inner
                        .terminal
                        .lock()
                        .unwrap()
                        .iter()
                        .any(|(tid, _)| *tid == from.as_raw())
                );
                self.inner.tool_exit.lock().unwrap().push(from.as_raw());
            }
        }
        true
    }
}
#[derive(Default, Clone, Debug, Serialize, Deserialize)]
struct ReadThread {
    attempts: usize,
    terminal: bool,
}
#[derive(Default)]
struct ReadObserver {
    case: ReadCase,
}
#[reverie::tool]
impl Tool for ReadObserver {
    type GlobalState = ReadLog;
    type ThreadState = ReadThread;
    fn new(_: Pid, case: &ReadCase) -> Self {
        Self { case: *case }
    }
    fn subscriptions(_: &ReadCase) -> Subscription {
        let mut subscriptions = Subscription::none();
        subscriptions.syscall(Sysno::read);
        subscriptions
    }
    fn observe_injected_syscalls(_: &ReadCase) -> bool {
        true
    }
    fn observe_injected_syscall_preparation(_: &ReadCase) -> bool {
        true
    }
    fn on_injected_syscall_observed(
        &self,
        tid: Pid,
        global: &ReadLog,
        state: &mut ReadThread,
        nr: Sysno,
        args: SyscallArgs,
        event: InjectedSyscallEvent,
    ) {
        assert!(!state.terminal);
        global.inner.observations.lock().unwrap().push(Observation {
            tid: tid.as_raw(),
            nr,
            args,
            event,
        });
        if nr != Sysno::read {
            return;
        }
        if global.pidfd.lock().unwrap().is_none() {
            let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, tid.as_raw(), 0) } as i32;
            assert!(
                fd >= 0,
                "retain exact read actor: {}",
                std::io::Error::last_os_error()
            );
            *global.pidfd.lock().unwrap() = Some(unsafe { OwnedFd::from_raw_fd(fd) });
        }
        if event == InjectedSyscallEvent::Prepared {
            state.attempts += 1;
        }
        let before = event == InjectedSyscallEvent::Prepared
            && match self.case {
                ReadCase::ReadyAtOriginalEntry | ReadCase::AdjustedReady => state.attempts == 1,
                ReadCase::PriorProgress
                | ReadCase::PrivateRetryInterrupted
                | ReadCase::PrivateRetryRestartable => state.attempts == 2,
                _ => false,
            };
        let entered = event == InjectedSyscallEvent::Entered
            && (matches!(self.case, ReadCase::Partial | ReadCase::Restart)
                || (state.attempts == 1
                    && matches!(
                        self.case,
                        ReadCase::AdjustedPartial
                            | ReadCase::AdjustedRestart
                            | ReadCase::AdjustedRestartable
                    )));
        if before || entered {
            assert!(!global.inner.signal_sent.swap(true, Ordering::SeqCst));
            // Real targeted signal while the backend still owns Prepared or
            // the authenticated ENTRY stop, before it resumes that exact task.
            assert_eq!(
                unsafe {
                    libc::syscall(libc::SYS_tgkill, tid.as_raw(), tid.as_raw(), libc::SIGUSR1)
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
        let Syscall::Read(mut call) = syscall else {
            panic!("unexpected subscription")
        };
        // The first production Read retains its exact native seccomp entry.
        // The partial-progress component below deliberately asks for a shorter
        // first attempt; it separately exercises private follow-up injection.
        let mut total = 0;
        if guest.thread_state().attempts == 0
            && matches!(
                self.case,
                ReadCase::PrivateRetryInterrupted | ReadCase::PrivateRetryRestartable
            )
        {
            // A real completed no-byte attempt consumes the original entry.
            // The next attempt is private; its Prepared hook queues a real
            // signal before that second kernel Read can enter.
            assert!(matches!(
                guest.inject_original_read(call.with_len(0)).await,
                InjectedReadResult::Complete(Ok(0))
            ));
        }
        if guest.thread_state().attempts == 0
            && matches!(
                self.case,
                ReadCase::AdjustedReady
                    | ReadCase::AdjustedPartial
                    | ReadCase::AdjustedRestart
                    | ReadCase::AdjustedRestartable
            )
        {
            // The backend must execute this adjusted attempt at the held
            // original entry and restore the logical operands for any restart.
            call = call
                .with_len(3)
                .with_buf(AddrMut::from_raw(call.buf().unwrap().as_raw() + 1));
        }
        if self.case == ReadCase::PriorProgress {
            match guest.inject_original_read(call.with_len(1)).await {
                InjectedReadResult::Complete(Ok(1)) => total = 1,
                other => panic!("first actual byte: {other:?}"),
            }
            call = call
                .with_len(1)
                .with_buf(AddrMut::from_raw(call.buf().unwrap().as_raw() + 1));
        }
        match guest.inject_original_read(call).await {
            InjectedReadResult::RecordedInterruption(_) => {
                panic!("native backend issued recorded control")
            }
            InjectedReadResult::Complete(result) => Ok(total + result?),
            InjectedReadResult::Interrupted(ticket) => {
                let mut untouched = vec![0; call.len()];
                guest
                    .memory()
                    .read_exact(call.buf().unwrap(), &mut untouched)?;
                assert_eq!(untouched, vec![0x5a; call.len()]);
                assert!(guest.send_rpc(ReadRequest::BeforeHandback).await);
                guest
                    .finish_interrupted_syscall(ticket.clone(), (total != 0).then_some(total))
                    .await?;
                if total != 0 {
                    Ok(total)
                } else {
                    Err(Error::Tool(anyhow::Error::new(ticket)))
                }
            }
        }
    }
    async fn handle_signal_event<G: Guest<Self>>(
        &self,
        guest: &mut G,
        signal: Signal,
    ) -> Result<Option<Signal>, reverie::Errno> {
        if signal == Signal::SIGUSR1 {
            assert!(guest.send_rpc(ReadRequest::SignalHook).await);
        }
        Ok(Some(signal))
    }
    fn on_backend_thread_terminal(
        &self,
        tid: Pid,
        global: &ReadLog,
        state: &mut ReadThread,
        status: ExitStatus,
    ) {
        assert!(!state.terminal);
        state.terminal = true;
        global
            .inner
            .terminal
            .lock()
            .unwrap()
            .push((tid.as_raw(), status));
    }
    async fn on_exit_thread<G: GlobalRPC<ReadLog>>(
        &self,
        _: Pid,
        global: &G,
        state: ReadThread,
        _: ExitStatus,
    ) -> Result<(), Error> {
        assert!(state.terminal);
        assert!(global.send_rpc(ReadRequest::Consumed).await);
        Ok(())
    }
}
static SIGNALS: AtomicI32 = AtomicI32::new(0);
static READ_SLOT: AtomicI32 = AtomicI32::new(-1);
static REPLACEMENT: AtomicI32 = AtomicI32::new(-1);
extern "C" fn read_signal(_: libc::c_int) {
    let slot = READ_SLOT.load(Ordering::SeqCst);
    if slot >= 0 && unsafe { libc::dup3(REPLACEMENT.load(Ordering::SeqCst), slot, 0) } != slot {
        unsafe { libc::_exit(91) };
    }
    SIGNALS.fetch_add(1, Ordering::SeqCst);
}
fn install_handler() {
    SIGNALS.store(0, Ordering::SeqCst);
    READ_SLOT.store(-1, Ordering::SeqCst);
    REPLACEMENT.store(-1, Ordering::SeqCst);
    let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
    action.sa_sigaction = read_signal as libc::sighandler_t;
    action.sa_flags = 0; // Restart test requires Linux's real EINTR conversion.
    assert_eq!(unsafe { libc::sigemptyset(&mut action.sa_mask) }, 0);
    assert_eq!(
        unsafe { libc::sigaction(libc::SIGUSR1, &action, std::ptr::null_mut()) },
        0
    );
    unsafe {
        libc::alarm(5);
    }
}
fn pipe() -> (OwnedFd, OwnedFd) {
    let mut fds = [-1; 2];
    assert_eq!(unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) }, 0);
    unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) }
}
fn require_actor(log: &ReadLog) {
    assert_normal_terminal(&log.inner, 1);
    assert_eq!(log.signal_hooks.load(Ordering::SeqCst), 1);
    let held = log.pidfd.lock().unwrap();
    let pidfd = held.as_ref().unwrap();
    let mut poll = libc::pollfd {
        fd: pidfd.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    assert_eq!(unsafe { libc::poll(&mut poll, 1, 0) }, 1);
    assert_ne!(poll.revents & libc::POLLIN, 0);
}
fn slot_replacement_case(case: ReadCase) {
    let (output, log) = test_fn_with_config::<ReadObserver, _>(
        move || {
            install_handler();
            let (original, producer) = pipe();
            let (replacement, replacement_producer) = pipe();
            let alias = unsafe { libc::fcntl(original.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 0) };
            assert!(alias >= 0);
            let alias = unsafe { OwnedFd::from_raw_fd(alias) };
            assert_eq!(
                unsafe { libc::write(producer.as_raw_fd(), b"AB".as_ptr().cast(), 2) },
                2
            );
            assert_eq!(
                unsafe { libc::write(replacement_producer.as_raw_fd(), b"N".as_ptr().cast(), 1) },
                1
            );
            READ_SLOT.store(original.as_raw_fd(), Ordering::SeqCst);
            REPLACEMENT.store(replacement.as_raw_fd(), Ordering::SeqCst);
            let mut buffer = [0x5a; 2];
            let count = if case == ReadCase::PriorProgress {
                2
            } else {
                1
            };
            assert_eq!(
                unsafe {
                    libc::syscall(
                        libc::SYS_read,
                        original.as_raw_fd(),
                        buffer.as_mut_ptr(),
                        count,
                    )
                },
                1
            );
            assert_eq!(SIGNALS.load(Ordering::SeqCst), 1);
            // Ready data returns before the handler replaces the FD. The
            // prior-progress case likewise preserves its already returned A.
            assert_eq!(buffer, [b'A', 0x5a]);
            let mut old = [0; 2];
            let expected: &[u8] = b"B";
            let iov = libc::iovec {
                iov_base: old.as_mut_ptr().cast(),
                iov_len: expected.len(),
            };
            assert_eq!(
                unsafe { libc::readv(alias.as_raw_fd(), &iov, 1) },
                expected.len() as isize
            );
            assert_eq!(&old[..expected.len()], expected);
            let iov = libc::iovec {
                iov_base: old.as_mut_ptr().cast(),
                iov_len: 1,
            };
            assert_eq!(unsafe { libc::readv(replacement.as_raw_fd(), &iov, 1) }, 1);
            assert_eq!(old[0], b'N', "Read consumed the handler's replacement");
        },
        case,
        true,
    )
    .expect("actual Read and signal slot replacement");
    assert_eq!(output.status, ExitStatus::Exited(0));
    require_actor(&log);
    assert_eq!(
        log.handback.load(Ordering::SeqCst),
        case == ReadCase::PriorProgress
    );
    let observations = log.inner.observations.lock().unwrap();
    let reads: Vec<_> = observations
        .iter()
        .filter(|row| row.nr == Sysno::read)
        .collect();
    let events: Vec<_> = reads.iter().map(|row| row.event).collect();
    let expected = if case == ReadCase::PriorProgress {
        vec![
            InjectedSyscallEvent::Prepared,
            InjectedSyscallEvent::Entered,
            InjectedSyscallEvent::Returned(1),
            InjectedSyscallEvent::Prepared,
            InjectedSyscallEvent::InterruptedBeforeEntry,
        ]
    } else {
        vec![
            InjectedSyscallEvent::Prepared,
            InjectedSyscallEvent::Entered,
            InjectedSyscallEvent::Returned(1),
        ]
    };
    assert_eq!(events, expected);
    assert!(
        reads
            .iter()
            .all(|row| row.tid == reads[0].tid && row.args.arg0 == reads[0].args.arg0)
    );
    if case == ReadCase::ReadyAtOriginalEntry {
        assert!(reads.iter().all(|row| row.args == reads[0].args));
    } else {
        assert_eq!(reads[3].args.arg1, reads[0].args.arg1 + 1);
    }
}
#[test]
fn read_signal_at_original_entry_returns_ready_bytes_before_handler_slot_replacement() {
    slot_replacement_case(ReadCase::ReadyAtOriginalEntry);
}
#[test]
fn read_signal_before_second_entry_preserves_prior_partial_count_without_retry() {
    slot_replacement_case(ReadCase::PriorProgress);
}
fn exit_case(case: ReadCase) {
    let (output, log) = test_fn_with_config::<ReadObserver, _>(
        move || {
            use std::io::Write;
            install_handler();
            let listener = std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
            let mut peer = std::net::TcpStream::connect(listener.local_addr().unwrap()).unwrap();
            let (stream, _) = listener.accept().unwrap();
            let lowat = 3i32;
            assert_eq!(
                unsafe {
                    libc::setsockopt(
                        stream.as_raw_fd(),
                        libc::SOL_SOCKET,
                        libc::SO_RCVLOWAT,
                        (&lowat as *const i32).cast(),
                        std::mem::size_of_val(&lowat) as libc::socklen_t,
                    )
                },
                0
            );
            if case == ReadCase::Partial {
                peer.write_all(b"P").unwrap();
                let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
                loop {
                    let mut queued = 0i32;
                    assert_eq!(
                        unsafe { libc::ioctl(stream.as_raw_fd(), libc::FIONREAD, &mut queued) },
                        0
                    );
                    if queued == 1 {
                        break;
                    }
                    assert_eq!(queued, 0);
                    assert!(std::time::Instant::now() < deadline);
                    std::thread::yield_now();
                }
            }
            let mut buffer = [0x5a; 8];
            let result = unsafe {
                libc::syscall(
                    libc::SYS_read,
                    stream.as_raw_fd(),
                    buffer.as_mut_ptr(),
                    buffer.len(),
                )
            };
            assert_eq!(SIGNALS.load(Ordering::SeqCst), 1);
            if case == ReadCase::Partial {
                assert_eq!(result, 1);
                assert_eq!(buffer, [b'P', 0x5a, 0x5a, 0x5a, 0x5a, 0x5a, 0x5a, 0x5a]);
            } else {
                assert_eq!(result, -1);
                assert_eq!(unsafe { *libc::__errno_location() }, libc::EINTR);
                assert_eq!(buffer, [0x5a; 8]);
            }
        },
        case,
        true,
    )
    .expect("actual signal-interrupted syscall exit");
    assert_eq!(output.status, ExitStatus::Exited(0));
    require_actor(&log);
    assert!(!log.handback.load(Ordering::SeqCst));
    let observations = log.inner.observations.lock().unwrap();
    let reads: Vec<_> = observations
        .iter()
        .filter(|row| row.nr == Sysno::read)
        .collect();
    assert_eq!(reads.len(), 3);
    assert_eq!(reads[0].event, InjectedSyscallEvent::Prepared);
    assert_eq!(reads[1].event, InjectedSyscallEvent::Entered);
    let raw = if case == ReadCase::Partial {
        1
    } else {
        -i64::from(reverie::Errno::ERESTARTSYS.into_raw())
    };
    assert_eq!(reads[2].event, InjectedSyscallEvent::Returned(raw));
    assert!(
        reads
            .iter()
            .all(|row| row.args == reads[0].args && row.tid == reads[0].tid)
    );
}
#[test]
fn read_signal_after_entry_preserves_actual_partial_exit_before_handler() {
    exit_case(ReadCase::Partial);
}
#[test]
fn read_signal_after_entry_preserves_raw_restart_and_linux_handler_result() {
    exit_case(ReadCase::Restart);
}

fn adjusted_original_entry_case(case: ReadCase) {
    let (output, log) = test_fn_with_config::<ReadObserver, _>(
        move || {
            use std::io::Write;
            install_handler();
            if case == ReadCase::AdjustedRestartable {
                let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
                assert_eq!(
                    unsafe { libc::sigaction(libc::SIGUSR1, std::ptr::null(), &mut action) },
                    0
                );
                action.sa_flags |= libc::SA_RESTART;
                assert_eq!(
                    unsafe { libc::sigaction(libc::SIGUSR1, &action, std::ptr::null_mut()) },
                    0
                );
            }
            let listener = std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
            let mut peer = std::net::TcpStream::connect(listener.local_addr().unwrap()).unwrap();
            let (stream, _) = listener.accept().unwrap();
            let lowat = 3i32;
            assert_eq!(
                unsafe {
                    libc::setsockopt(
                        stream.as_raw_fd(),
                        libc::SOL_SOCKET,
                        libc::SO_RCVLOWAT,
                        (&lowat as *const i32).cast(),
                        std::mem::size_of_val(&lowat) as libc::socklen_t,
                    )
                },
                0
            );
            let bytes: &[u8] = match case {
                ReadCase::AdjustedReady => b"ABC",
                ReadCase::AdjustedPartial => b"P",
                ReadCase::AdjustedRestart | ReadCase::AdjustedRestartable => b"",
                _ => panic!("wrong adjusted-entry case"),
            };
            peer.write_all(bytes).unwrap();
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
            loop {
                let mut queued = 0i32;
                assert_eq!(
                    unsafe { libc::ioctl(stream.as_raw_fd(), libc::FIONREAD, &mut queued) },
                    0
                );
                if queued == bytes.len() as i32 {
                    break;
                }
                assert!(queued >= 0 && queued < bytes.len() as i32);
                assert!(std::time::Instant::now() < deadline);
                std::thread::yield_now();
            }
            let (replacement, replacement_producer) = pipe();
            assert_eq!(
                unsafe {
                    libc::write(
                        replacement_producer.as_raw_fd(),
                        b"RSTUVWXY".as_ptr().cast(),
                        8,
                    )
                },
                8
            );
            READ_SLOT.store(stream.as_raw_fd(), Ordering::SeqCst);
            REPLACEMENT.store(replacement.as_raw_fd(), Ordering::SeqCst);
            let mut buffer = [0x5a; 10];
            let result = unsafe {
                libc::syscall(
                    libc::SYS_read,
                    stream.as_raw_fd(),
                    buffer.as_mut_ptr().add(1),
                    8,
                )
            };
            assert_eq!(SIGNALS.load(Ordering::SeqCst), 1);
            let mut expected = [0x5a; 10];
            match case {
                ReadCase::AdjustedReady => {
                    assert_eq!(result, 3);
                    expected[2..5].copy_from_slice(b"ABC");
                }
                ReadCase::AdjustedPartial => {
                    assert_eq!(result, 1);
                    expected[2] = b'P';
                }
                ReadCase::AdjustedRestart => {
                    assert_eq!(result, -1);
                    assert_eq!(unsafe { *libc::__errno_location() }, libc::EINTR);
                }
                ReadCase::AdjustedRestartable => {
                    // Linux restarts the original guest operands, not the
                    // shortened/shifted physical attempt from the Tool.
                    assert_eq!(result, 8);
                    expected[1..9].copy_from_slice(b"RSTUVWXY");
                }
                _ => unreachable!(),
            }
            assert_eq!(buffer, expected);
            if case != ReadCase::AdjustedRestartable {
                let mut remaining = [0; 8];
                let iov = libc::iovec {
                    iov_base: remaining.as_mut_ptr().cast(),
                    iov_len: 8,
                };
                assert_eq!(unsafe { libc::readv(replacement.as_raw_fd(), &iov, 1) }, 8);
                assert_eq!(
                    &remaining, b"RSTUVWXY",
                    "the canceled/restarted path consumed replacement bytes"
                );
            }
        },
        case,
        true,
    )
    .expect("adjusted native Read entry preserves Linux completion and restart");
    assert_eq!(output.status, ExitStatus::Exited(0));
    require_actor(&log);
    assert!(
        !log.handback.load(Ordering::SeqCst),
        "the first Read entry became a private cancellation"
    );
    let observations = log.inner.observations.lock().unwrap();
    let reads: Vec<_> = observations
        .iter()
        .filter(|row| row.nr == Sysno::read)
        .collect();
    let raw = match case {
        ReadCase::AdjustedReady => 3,
        ReadCase::AdjustedPartial => 1,
        ReadCase::AdjustedRestart | ReadCase::AdjustedRestartable => {
            -i64::from(reverie::Errno::ERESTARTSYS.into_raw())
        }
        _ => unreachable!(),
    };
    let mut expected = vec![
        InjectedSyscallEvent::Prepared,
        InjectedSyscallEvent::Entered,
        InjectedSyscallEvent::Returned(raw),
    ];
    if case == ReadCase::AdjustedRestartable {
        expected.extend([
            InjectedSyscallEvent::Prepared,
            InjectedSyscallEvent::Entered,
            InjectedSyscallEvent::Returned(8),
        ]);
    }
    assert_eq!(
        reads.iter().map(|row| row.event).collect::<Vec<_>>(),
        expected
    );
    assert_eq!(reads[0].args.arg2, 3);
    assert!(
        reads[..3]
            .iter()
            .all(|row| row.args == reads[0].args && row.tid == reads[0].tid)
    );
    if case == ReadCase::AdjustedRestartable {
        assert_eq!(reads[3].args.arg0, reads[0].args.arg0);
        assert_eq!(reads[3].args.arg1 + 1, reads[0].args.arg1);
        assert_eq!(reads[3].args.arg2, 8);
        assert!(
            reads[3..]
                .iter()
                .all(|row| row.args == reads[3].args && row.tid == reads[0].tid)
        );
    }
}

#[test]
fn adjusted_read_at_original_entry_preserves_ready_bytes_and_buffer_operand() {
    adjusted_original_entry_case(ReadCase::AdjustedReady);
}
#[test]
fn adjusted_read_at_original_entry_preserves_positive_partial_before_signal() {
    adjusted_original_entry_case(ReadCase::AdjustedPartial);
}
#[test]
fn adjusted_read_at_original_entry_preserves_linux_eintr_without_sa_restart() {
    adjusted_original_entry_case(ReadCase::AdjustedRestart);
}
#[test]
fn adjusted_read_at_original_entry_restarts_original_operands_with_sa_restart() {
    adjusted_original_entry_case(ReadCase::AdjustedRestartable);
}

fn private_retry_logical_restart_case(restart: bool) {
    let case = if restart {
        ReadCase::PrivateRetryRestartable
    } else {
        ReadCase::PrivateRetryInterrupted
    };
    let (output, log) = test_fn_with_config::<ReadObserver, _>(
        move || {
            install_handler();
            if restart {
                let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
                action.sa_sigaction = read_signal as libc::sighandler_t;
                action.sa_flags = libc::SA_RESTART;
                assert_eq!(unsafe { libc::sigemptyset(&mut action.sa_mask) }, 0);
                assert_eq!(
                    unsafe { libc::sigaction(libc::SIGUSR1, &action, std::ptr::null_mut()) },
                    0
                );
            }
            let (original, _producer) = pipe();
            let (replacement, replacement_producer) = pipe();
            assert_eq!(
                unsafe { libc::write(replacement_producer.as_raw_fd(), b"XY".as_ptr().cast(), 2) },
                2
            );
            READ_SLOT.store(original.as_raw_fd(), Ordering::SeqCst);
            REPLACEMENT.store(replacement.as_raw_fd(), Ordering::SeqCst);
            let mut buffer = [0x5a; 4];
            let returned = unsafe {
                libc::syscall(
                    libc::SYS_read,
                    original.as_raw_fd(),
                    buffer.as_mut_ptr().add(1),
                    2usize,
                )
            };
            if restart {
                assert_eq!(returned, 2);
                assert_eq!(buffer, [0x5a, b'X', b'Y', 0x5a]);
            } else {
                assert_eq!(returned, -1);
                assert_eq!(
                    std::io::Error::last_os_error().raw_os_error(),
                    Some(libc::EINTR)
                );
                assert_eq!(buffer, [0x5a; 4]);
                let mut replacement_bytes = [0u8; 2];
                let iov = libc::iovec {
                    iov_base: replacement_bytes.as_mut_ptr().cast(),
                    iov_len: 2,
                };
                assert_eq!(unsafe { libc::readv(original.as_raw_fd(), &iov, 1) }, 2);
                assert_eq!(&replacement_bytes, b"XY");
            }
            assert_eq!(SIGNALS.load(Ordering::SeqCst), 1);
            unsafe {
                libc::alarm(0);
            }
        },
        case,
        true,
    )
    .expect("private retry logical restart backend control");
    assert_eq!(output.status, ExitStatus::Exited(0));
    require_actor(&log);
    assert!(log.handback.load(Ordering::SeqCst));
    let observations = log.inner.observations.lock().unwrap();
    let rows: Vec<_> = observations
        .iter()
        .filter(|row| row.nr == Sysno::read)
        .collect();
    let mut expected = vec![
        InjectedSyscallEvent::Prepared,
        InjectedSyscallEvent::Entered,
        InjectedSyscallEvent::Returned(0),
        InjectedSyscallEvent::Prepared,
        InjectedSyscallEvent::InterruptedBeforeEntry,
    ];
    if restart {
        expected.extend([
            InjectedSyscallEvent::Prepared,
            InjectedSyscallEvent::Entered,
            InjectedSyscallEvent::Returned(2),
        ]);
    }
    assert_eq!(
        rows.iter().map(|row| row.event).collect::<Vec<_>>(),
        expected
    );
    assert_eq!(rows[0].args.arg2, 0);
    assert_eq!(rows[3].args.arg2, 2);
    assert_eq!(rows[0].args.arg0, rows[3].args.arg0);
    assert_eq!(rows[0].args.arg1, rows[3].args.arg1);
    if restart {
        assert_eq!(rows[5].args, rows[3].args);
    }
}

#[test]
fn private_read_before_entry_uses_linux_eintr_after_completed_zero_attempt() {
    private_retry_logical_restart_case(false);
}

#[test]
fn private_read_before_entry_uses_linux_restart_after_completed_zero_attempt() {
    private_retry_logical_restart_case(true);
}
