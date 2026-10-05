//! Native bodies: compile/list only in packet 804. Each needs separate admission.
use std::os::fd::AsRawFd;
use std::os::fd::FromRawFd;
use std::os::fd::OwnedFd;

use reverie::Stack;
use serde::Deserialize;
use serde::Serialize;

use super::*;

#[derive(Default)]
struct Log {
    stages: StdMutex<Vec<InjectedSyscallEvent>>,
    original: StdMutex<Option<libc::user_regs_struct>>,
    completed: AtomicUsize,
    signals: AtomicUsize,
}
#[reverie::global_tool]
impl GlobalTool for Log {
    type Config = ();
    type Request = ();
    type Response = ();
    async fn receive_rpc(&self, _: Pid, _: ()) {}
}
#[derive(Default, Serialize, Deserialize)]
struct State;
impl AsRef<State> for State {
    fn as_ref(&self) -> &Self {
        self
    }
}
impl AsMut<State> for State {
    fn as_mut(&mut self) -> &mut Self {
        self
    }
}
#[derive(Default)]
struct OptionTool;
impl AsMut<OptionTool> for OptionTool {
    fn as_mut(&mut self) -> &mut Self {
        self
    }
}

fn context(regs: libc::user_regs_struct) -> [u64; 11] {
    [
        regs.rip,
        regs.rsp,
        regs.rcx,
        regs.r11,
        regs.eflags,
        regs.rdi,
        regs.rsi,
        regs.rdx,
        regs.r10,
        regs.r8,
        regs.r9,
    ]
}

pub(super) const FRAME_INDICES: [usize; 11] = [
    libc::REG_RIP as usize,
    libc::REG_RSP as usize,
    libc::REG_RCX as usize,
    libc::REG_R11 as usize,
    libc::REG_EFL as usize,
    libc::REG_RDI as usize,
    libc::REG_RSI as usize,
    libc::REG_RDX as usize,
    libc::REG_R10 as usize,
    libc::REG_R8 as usize,
    libc::REG_R9 as usize,
];

pub(super) fn frame_matches(frame: &[libc::greg_t; 23], expected: [u64; 11]) -> bool {
    FRAME_INDICES
        .iter()
        .zip(expected)
        .all(|(index, value)| frame[*index] as u64 == value)
}

#[reverie::tool]
impl Tool for OptionTool {
    type GlobalState = Log;
    type ThreadState = State;
    fn subscriptions(_: &()) -> Subscription {
        let mut value = Subscription::none();
        value.syscalls([Sysno::setsockopt]);
        value
    }
    fn observe_injected_syscalls(_: &()) -> bool {
        true
    }
    fn observe_injected_syscall_preparation(_: &()) -> bool {
        true
    }
    fn on_injected_syscall_observed(
        &self,
        _: Tid,
        global: &Log,
        _: &mut State,
        nr: Sysno,
        _: SyscallArgs,
        event: InjectedSyscallEvent,
    ) {
        assert_eq!(nr, Sysno::setsockopt);
        global.stages.lock().unwrap().push(event);
    }
    async fn handle_syscall_event<G: Guest<Self>>(
        &self,
        guest: &mut G,
        call: Syscall,
    ) -> Result<i64, reverie::Error> {
        let Syscall::Setsockopt(call) = call else {
            panic!("unexpected subscription")
        };
        let before = guest.regs().await;
        assert_eq!(
            before.r9, 0xfeed,
            "unused raw argument must survive the typed call"
        );
        assert!(
            guest
                .local_global_state()
                .unwrap()
                .original
                .lock()
                .unwrap()
                .replace(before)
                .is_none()
        );
        // test_fn forks this same image without exec or a PID/user namespace.
        // Publish expected ORIGINAL entry registers into the child's own
        // fixture record before the signal is queued. This memory write is not
        // another injected syscall and never changes the actual stop registers.
        let mut expected = [0u64; 14];
        expected[..11].copy_from_slice(&context(before));
        expected[11] = unsafe { libc::getpid() } as u64;
        expected[12] = unsafe { libc::getuid() } as u64;
        expected[13] = FRAME_READY;
        let address = AddrMut::from_raw(std::ptr::addr_of_mut!(EXPECTED_FRAME) as usize)
            .expect("fixture expectation has a non-null address");
        guest.memory().write_value(address, &expected)?;
        // Queue a real occurrence at the same original SECCOMP boundary in
        // baseline, mutant and candidate. This is not a private Tgkill helper.
        assert_eq!(
            unsafe {
                libc::syscall(
                    libc::SYS_tgkill,
                    guest.pid().as_raw(),
                    guest.tid().as_raw(),
                    libc::SIGUSR1,
                )
            },
            0
        );
        let result = {
            // Same Guest::inject/stack lifetime as Detcore's option snapshot.
            let mut stack = guest.stack().await;
            let snapshot = stack.push(2i32);
            let _guard = stack.commit()?;
            guest.inject(call.with_optval(Some(snapshot.cast()))).await
        };
        let raw = match result {
            Ok(value) => value,
            Err(errno) => -(errno.into_raw() as i64),
        };
        let after = guest.regs().await;
        assert_eq!(
            context(after),
            context(before),
            "original context restored before Tool continuation"
        );
        assert_eq!(after.rax as i64, raw, "actual kernel result retained");
        let global = guest.local_global_state().unwrap();
        assert_eq!(global.completed.fetch_add(1, Ordering::SeqCst), 0);
        assert_eq!(
            *global.stages.lock().unwrap(),
            vec![
                InjectedSyscallEvent::Prepared,
                InjectedSyscallEvent::Entered,
                InjectedSyscallEvent::Returned(raw)
            ]
        );
        Ok(result?)
    }
    async fn handle_signal_event<G: Guest<Self>>(
        &self,
        guest: &mut G,
        signal: Signal,
    ) -> Result<Option<Signal>, Errno> {
        if signal == Signal::SIGUSR1 {
            let regs = guest.regs().await;
            let global = guest.local_global_state().unwrap();
            assert_eq!(
                global.completed.load(Ordering::SeqCst),
                1,
                "signal requires the real original result, not a private pre-ENTRY register"
            );
            assert_eq!(
                context(regs),
                context(global.original.lock().unwrap().unwrap())
            );
            assert_eq!(global.signals.fetch_add(1, Ordering::SeqCst), 0);
        }
        Ok(Some(signal))
    }
}

static FD: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(-1);
static EXPECTED: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(0);
static HANDLERS: AtomicUsize = AtomicUsize::new(0);
const FRAME_READY: u64 = 0x8125_1a6f_4a6d_e001;
static mut EXPECTED_FRAME: [u64; 14] = [0; 14];
static mut EXPECTED_MASK: libc::sigset_t = unsafe { std::mem::zeroed() };
extern "C" fn handler(signal: i32, info: *mut libc::siginfo_t, context: *mut libc::c_void) {
    unsafe {
        let frame = &*(context as *mut libc::ucontext_t);
        let regs = &frame.uc_mcontext.gregs;
        let expected_frame = std::ptr::read_volatile(std::ptr::addr_of!(EXPECTED_FRAME));
        let expected_mask = std::ptr::read_volatile(std::ptr::addr_of!(EXPECTED_MASK));
        let mut expected_regs = [0; 11];
        expected_regs.copy_from_slice(&expected_frame[..11]);
        let mut value = 0i32;
        let mut len = std::mem::size_of_val(&value) as libc::socklen_t;
        let expected = EXPECTED.load(Ordering::SeqCst);
        let ok = signal == libc::SIGUSR1
            && (*info).si_code == libc::SI_TKILL
            && (*info).si_errno == 0
            && (*info).si_pid() as u64 == expected_frame[11]
            && (*info).si_uid() as u64 == expected_frame[12]
            && expected_frame[13] == FRAME_READY
            && frame_matches(regs, expected_regs)
            && (1..=64).all(|number| {
                libc::sigismember(&frame.uc_sigmask, number)
                    == libc::sigismember(&expected_mask, number)
            })
            && regs[libc::REG_RAX as usize] == expected
            && libc::getsockopt(
                FD.load(Ordering::SeqCst),
                libc::SOL_SOCKET,
                libc::SO_RCVLOWAT,
                (&mut value as *mut i32).cast(),
                &mut len,
            ) == 0
            && value == if expected == 0 { 2 } else { 1 }
            && HANDLERS.fetch_add(1, Ordering::SeqCst) == 0;
        if !ok {
            libc::_exit(81);
        }
    }
}

// A guest-owned filter must see the rewritten operand on Linux's post-TRACE
// recheck. Only the original pointer is allowed; this is never loaded by pure
// tests or test listing.
unsafe fn install_pointer_filter(original: *const i32) {
    let insn = |code, jt, jf, k| libc::sock_filter { code, jt, jf, k };
    let mut filter = [
        insn(0x20, 0, 0, 0), // seccomp_data.nr
        insn(0x15, 0, 5, libc::SYS_setsockopt as u32),
        insn(0x20, 0, 0, 40), // args[3], low word
        insn(0x15, 0, 2, original as usize as u32),
        insn(0x20, 0, 0, 44), // args[3], high word
        insn(0x15, 1, 0, ((original as usize) >> 32) as u32),
        insn(0x06, 0, 0, libc::SECCOMP_RET_ERRNO | libc::EACCES as u32),
        insn(0x06, 0, 0, libc::SECCOMP_RET_ALLOW),
    ];
    let program = libc::sock_fprog {
        len: filter.len() as u16,
        filter: filter.as_mut_ptr(),
    };
    unsafe {
        assert_eq!(libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0), 0);
        assert_eq!(
            libc::prctl(libc::PR_SET_SECCOMP, libc::SECCOMP_MODE_FILTER, &program),
            0
        );
    }
}

fn run_option(mode: u8) {
    let (output, global) = crate::testing::test_fn::<OptionTool, _>(move || unsafe {
        HANDLERS.store(0, Ordering::SeqCst);
        std::ptr::write_volatile(std::ptr::addr_of_mut!(EXPECTED_FRAME), [0; 14]);
        let errno_expected = match mode {
            0 => 0,
            1 => libc::ENOPROTOOPT,
            2 => libc::EACCES,
            _ => panic!("unknown fixture mode"),
        };
        let expected = -(errno_expected as i64);
        EXPECTED.store(expected, Ordering::SeqCst);
        let fd = libc::socket(libc::AF_UNIX, libc::SOCK_STREAM | libc::SOCK_CLOEXEC, 0);
        assert!(fd >= 0);
        FD.store(fd, Ordering::SeqCst);
        let mut initial = 0i32;
        let mut initial_len = std::mem::size_of_val(&initial) as libc::socklen_t;
        assert_eq!(
            libc::getsockopt(
                fd,
                libc::SOL_SOCKET,
                libc::SO_RCVLOWAT,
                (&mut initial as *mut i32).cast(),
                &mut initial_len
            ),
            0
        );
        assert_eq!(initial, 1, "success must change the actual kernel option");
        let mut action: libc::sigaction = std::mem::zeroed();
        action.sa_flags = libc::SA_SIGINFO;
        action.sa_sigaction = handler as *const () as usize;
        assert_eq!(libc::sigemptyset(&mut action.sa_mask), 0);
        assert_eq!(
            libc::sigaction(libc::SIGUSR1, &action, std::ptr::null_mut()),
            0
        );
        let mut installed: libc::sigaction = std::mem::zeroed();
        assert_eq!(
            libc::sigaction(libc::SIGUSR1, std::ptr::null(), &mut installed),
            0
        );
        let mut before: libc::sigset_t = std::mem::zeroed();
        assert_eq!(
            libc::sigprocmask(libc::SIG_SETMASK, std::ptr::null(), &mut before),
            0
        );
        assert_eq!(libc::sigismember(&before, libc::SIGUSR1), 0);
        std::ptr::write_volatile(std::ptr::addr_of_mut!(EXPECTED_MASK), before);
        let value = 2i32;
        if mode == 2 {
            install_pointer_filter(&value);
        }
        let name = if mode == 1 {
            0x7fffffff
        } else {
            libc::SO_RCVLOWAT
        };
        *libc::__errno_location() = 0;
        let result = libc::syscall(
            libc::SYS_setsockopt,
            fd,
            libc::SOL_SOCKET,
            name,
            &value as *const i32,
            std::mem::size_of_val(&value),
            0xfeedusize,
        );
        let errno = *libc::__errno_location();
        assert_eq!(result, if mode != 0 { -1 } else { 0 });
        assert_eq!(errno, errno_expected);
        assert_eq!(
            HANDLERS.load(Ordering::SeqCst),
            1,
            "one genuine handler before logical return"
        );
        let mut after: libc::sigset_t = std::mem::zeroed();
        assert_eq!(
            libc::sigprocmask(libc::SIG_SETMASK, std::ptr::null(), &mut after),
            0
        );
        for signal in 1..=64 {
            assert_eq!(
                libc::sigismember(&before, signal),
                libc::sigismember(&after, signal)
            );
        }
        let mut retained: libc::sigaction = std::mem::zeroed();
        assert_eq!(
            libc::sigaction(libc::SIGUSR1, std::ptr::null(), &mut retained),
            0
        );
        assert_eq!(retained.sa_sigaction, action.sa_sigaction);
        assert_eq!(retained.sa_flags, installed.sa_flags);
        assert_eq!(
            retained.sa_restorer.map(|f| f as usize),
            installed.sa_restorer.map(|f| f as usize)
        );
        for signal in 1..=64 {
            assert_eq!(
                libc::sigismember(&retained.sa_mask, signal),
                libc::sigismember(&installed.sa_mask, signal)
            );
        }
        assert_eq!(libc::close(fd), 0);
    })
    .expect("native option fixture must finish through ordinary backend custody");
    assert_eq!(output.status, reverie::ExitStatus::Exited(0));
    assert!(output.stdout.is_empty());
    assert!(output.stderr.is_empty());
    assert_eq!(global.completed.load(Ordering::SeqCst), 1);
    assert_eq!(global.signals.load(Ordering::SeqCst), 1);
}

#[test]
fn native_original_setsockopt_signal_success() {
    run_option(0);
}
#[test]
fn native_original_setsockopt_signal_errno() {
    run_option(1);
}

#[test]
fn native_original_setsockopt_rechecks_rewritten_pointer() {
    run_option(2);
}

#[derive(Default)]
struct CaptureLog {
    stages: StdMutex<Vec<InjectedSyscallEvent>>,
    captured: AtomicUsize,
    callbacks: AtomicUsize,
    continued: AtomicUsize,
    owner: StdMutex<Option<TerminalCleanup>>,
}

struct CaptureHook {
    fd: usize,
    refuse: bool,
    log: Arc<CaptureLog>,
}

std::thread_local! {
    static CAPTURE_HOOK: std::cell::RefCell<Option<CaptureHook>> = const { std::cell::RefCell::new(None) };
}

struct CaptureHookGuard;
impl Drop for CaptureHookGuard {
    fn drop(&mut self) {
        CAPTURE_HOOK.with(|slot| slot.borrow_mut().take());
    }
}

// Called only AFTER production capture has authenticated the real native
// entry, full tuple, CS and direction. The retained owner is observational;
// this seam never resumes, fabricates a stop, or replaces cleanup ownership.
pub(super) fn observe_capture_for_test(
    task: &Stopped,
    args: SyscallArgs,
) -> Result<(), TraceError> {
    let hook = CAPTURE_HOOK.with(|slot| {
        let mut slot = slot.borrow_mut();
        if slot.as_ref().is_some_and(|hook| hook.fd == args.arg0) {
            slot.take()
        } else {
            None
        }
    });
    if let Some(hook) = hook {
        assert_eq!(hook.log.captured.fetch_add(1, Ordering::SeqCst), 0);
        assert!(
            hook.log
                .owner
                .lock()
                .unwrap()
                .replace(task.terminal_cleanup())
                .is_none()
        );
        if hook.refuse {
            return Err(Errno::EIO.into());
        }
    }
    Ok(())
}

#[derive(Clone, Default, Serialize, Deserialize)]
struct CaptureConfig {
    // Ptrace's in-process global initializer receives this original Arc. The
    // guest does not need or deserialize the parent observation log.
    #[serde(skip)]
    log: Arc<CaptureLog>,
}

#[derive(Default)]
struct CaptureGlobal {
    log: Arc<CaptureLog>,
}

#[reverie::global_tool]
impl GlobalTool for CaptureGlobal {
    type Config = CaptureConfig;
    type Request = ();
    type Response = ();
    async fn init_global_state(config: &CaptureConfig) -> Self {
        Self {
            log: Arc::clone(&config.log),
        }
    }
    async fn receive_rpc(&self, _: Pid, _: ()) {}
}

#[derive(Default)]
struct CaptureTool;
impl AsMut<CaptureTool> for CaptureTool {
    fn as_mut(&mut self) -> &mut Self {
        self
    }
}

#[reverie::tool]
impl Tool for CaptureTool {
    type GlobalState = CaptureGlobal;
    type ThreadState = State;
    fn subscriptions(_: &CaptureConfig) -> Subscription {
        let mut subscriptions = Subscription::none();
        subscriptions.syscalls([Sysno::setsockopt]);
        subscriptions
    }
    fn observe_injected_syscalls(_: &CaptureConfig) -> bool {
        true
    }
    fn observe_injected_syscall_preparation(_: &CaptureConfig) -> bool {
        true
    }
    fn on_injected_syscall_observed(
        &self,
        _: Tid,
        global: &CaptureGlobal,
        _: &mut State,
        nr: Sysno,
        _: SyscallArgs,
        event: InjectedSyscallEvent,
    ) {
        assert_eq!(nr, Sysno::setsockopt);
        global.log.stages.lock().unwrap().push(event);
    }
    async fn handle_syscall_event<G: Guest<Self>>(
        &self,
        guest: &mut G,
        call: Syscall,
    ) -> Result<i64, reverie::Error> {
        let Syscall::Setsockopt(call) = call else {
            panic!("unexpected subscription")
        };
        let log = Arc::clone(&guest.local_global_state().unwrap().log);
        assert_eq!(log.callbacks.fetch_add(1, Ordering::SeqCst), 0);
        assert_eq!(
            log.captured.load(Ordering::SeqCst),
            1,
            "real native capture seam must precede Tool dispatch"
        );
        assert_eq!(guest.regs().await.r9, 0xfeed);
        let mut stack = guest.stack().await;
        let snapshot = stack.push(2i32);
        let _guard = stack.commit()?;
        let result = guest.inject(call.with_optval(Some(snapshot.cast()))).await;
        log.continued.fetch_add(1, Ordering::SeqCst);
        Ok(result?)
    }
}

fn low_water(fd: i32) -> i32 {
    let mut value = 0i32;
    let mut len = std::mem::size_of_val(&value) as libc::socklen_t;
    assert_eq!(
        unsafe {
            libc::getsockopt(
                fd,
                libc::SOL_SOCKET,
                libc::SO_RCVLOWAT,
                (&mut value as *mut i32).cast(),
                &mut len,
            )
        },
        0
    );
    assert_eq!(len as usize, std::mem::size_of_val(&value));
    value
}

fn run_capture_option(refuse: bool) {
    let original =
        unsafe { libc::socket(libc::AF_UNIX, libc::SOCK_STREAM | libc::SOCK_CLOEXEC, 0) };
    assert!(original >= 0);
    let original = unsafe { OwnedFd::from_raw_fd(original) };
    // init_tracee closes3..255 before the closure. Duplicate BEFORE fork to a
    // kernel-selected free descriptor, never overwrite an assumed fixed FD.
    let inherited = unsafe { libc::fcntl(original.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 256) };
    assert!(inherited >= 256);
    let socket = unsafe { OwnedFd::from_raw_fd(inherited) };
    drop(original);
    assert_eq!(low_water(socket.as_raw_fd()), 1);
    let log = Arc::new(CaptureLog::default());
    CAPTURE_HOOK.with(|slot| {
        assert!(
            slot.borrow_mut()
                .replace(CaptureHook {
                    fd: inherited as usize,
                    refuse,
                    log: Arc::clone(&log)
                })
                .is_none()
        );
    });
    let _hook = CaptureHookGuard;
    let result = crate::testing::test_fn_with_config::<CaptureTool, _>(
        move || unsafe {
            let value = 2i32;
            let result = libc::syscall(
                libc::SYS_setsockopt,
                inherited,
                libc::SOL_SOCKET,
                libc::SO_RCVLOWAT,
                &value as *const i32,
                std::mem::size_of_val(&value),
                0xfeedusize,
            );
            assert_eq!(result, 0);
            assert_eq!(libc::close(inherited), 0);
        },
        CaptureConfig {
            log: Arc::clone(&log),
        },
        true,
    );
    // Require the real reached phase and retained cleanup, not a universal
    // startup failure or a late numeric lookup, before interpreting the result.
    assert_eq!(log.captured.load(Ordering::SeqCst), 1);
    assert_eq!(log.callbacks.load(Ordering::SeqCst), 1);
    let owner = log
        .owner
        .lock()
        .unwrap()
        .take()
        .expect("captured original task owner");
    assert!(
        owner.wait(std::time::Duration::from_secs(1)),
        "original notifier owner must retire"
    );
    assert!(
        owner
            .observed_exit_status()
            .expect("original final wait")
            .is_some()
    );
    // Physical state on the SAME parent-held OFD is the first effect oracle.
    // The old-fallback mutant must reach this counterexample before any error-
    // shape assertion: its private syscall changes1->2 despite capture refusal.
    assert_eq!(
        low_water(socket.as_raw_fd()),
        if refuse { 1 } else { 2 },
        "capture refusal must leave the actual socket option unchanged"
    );
    if refuse {
        assert_eq!(
            *log.stages.lock().unwrap(),
            vec![InjectedSyscallEvent::Prepared]
        );
        assert_eq!(log.continued.load(Ordering::SeqCst), 0);
        match result {
            Err(reverie::Error::Errno(Errno::EIO)) => (),
            Err(other) => panic!("original capture EIO must be retained: {other:?}"),
            Ok((output, _)) => panic!("capture refusal unexpectedly completed: {output:?}"),
        }
    } else {
        let (output, _) = result.expect("same inherited OFD neighbor must complete");
        assert_eq!(output.status, reverie::ExitStatus::Exited(0));
        assert!(output.stdout.is_empty() && output.stderr.is_empty());
        assert_eq!(log.continued.load(Ordering::SeqCst), 1);
        assert_eq!(
            *log.stages.lock().unwrap(),
            vec![
                InjectedSyscallEvent::Prepared,
                InjectedSyscallEvent::Entered,
                InjectedSyscallEvent::Returned(0)
            ]
        );
    }
    drop(socket);
}

#[test]
fn native_original_setsockopt_capture_failure_has_zero_effect() {
    run_capture_option(true);
}

#[test]
fn native_original_setsockopt_capture_valid_same_ofd_neighbor() {
    run_capture_option(false);
}
