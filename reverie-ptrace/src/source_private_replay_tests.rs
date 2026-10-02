//! Native finite-transport controls. These do not establish recorded-signal causation.
use std::os::fd::AsRawFd;
use std::os::fd::FromRawFd;
use std::os::fd::OwnedFd;
use std::time::Instant;

use reverie::PrivateInterruption;
use reverie::PrivateInterruptionAction;
use reverie::PrivateReadCompletion;
use reverie::Tid;
use serde::Deserialize;
use serde::Serialize;

use super::*;

#[repr(C)]
#[derive(Debug, Default, Clone, Eq, PartialEq)]
struct KernelInfo {
    op: u8,
    reserved: [u8; 3],
    arch: u32,
    ip: u64,
    sp: u64,
    nr_or_result: u64,
    args: [u64; 6],
    data: u32,
    padding: u32,
}
const _: () = assert!(std::mem::size_of::<KernelInfo>() == 88);

fn kernel_info(tid: Tid) -> Result<KernelInfo, Errno> {
    let mut info = KernelInfo::default();
    let count = unsafe {
        libc::ptrace(
            libc::PTRACE_GET_SYSCALL_INFO,
            tid.as_raw(),
            std::mem::size_of::<KernelInfo>() as *mut libc::c_void,
            (&mut info as *mut KernelInfo).cast::<libc::c_void>(),
        )
    };
    if count < 0 {
        return Err(Errno::last());
    }
    let minimum = match info.op {
        0 => 24,
        1 => 80,
        2 => 33,
        3 => 84,
        _ => 89,
    };
    if count < minimum || info.arch != 0xc000003e {
        return Err(Errno::EPROTO);
    }
    Ok(info)
}
fn physical_regs(tid: Tid) -> Result<libc::user_regs_struct, Errno> {
    let mut regs = std::mem::MaybeUninit::<libc::user_regs_struct>::uninit();
    if unsafe {
        libc::ptrace(
            libc::PTRACE_GETREGS,
            tid.as_raw(),
            std::ptr::null_mut::<libc::c_void>(),
            regs.as_mut_ptr().cast::<libc::c_void>(),
        )
    } != 0
    {
        return Err(Errno::last());
    }
    Ok(unsafe { regs.assume_init() })
}
#[repr(C)]
#[derive(Debug, Default, Clone)]
struct SigsysInfo {
    signo: i32,
    errno: i32,
    code: i32,
    alignment: i32,
    call_addr: u64,
    syscall: i32,
    arch: u32,
    remaining: [u64; 12],
}
const _: () = assert!(std::mem::size_of::<SigsysInfo>() == 128);

fn physical_siginfo(tid: Tid) -> Result<SigsysInfo, Errno> {
    let mut info = SigsysInfo::default();
    if unsafe {
        libc::ptrace(
            libc::PTRACE_GETSIGINFO,
            tid.as_raw(),
            std::ptr::null_mut::<libc::c_void>(),
            (&mut info as *mut SigsysInfo).cast::<libc::c_void>(),
        )
    } != 0
    {
        return Err(Errno::last());
    }
    Ok(info)
}

fn pending_siginfo(tid: Tid) -> Result<Vec<SigsysInfo>, Errno> {
    #[repr(C)]
    struct PeekArgs {
        offset: u64,
        flags: u32,
        count: i32,
    }
    let args = PeekArgs {
        offset: 0,
        flags: 0,
        count: 4,
    };
    let mut infos: [SigsysInfo; 4] = std::array::from_fn(|_| SigsysInfo::default());
    let count = unsafe {
        libc::ptrace(
            libc::PTRACE_PEEKSIGINFO,
            tid.as_raw(),
            (&args as *const PeekArgs).cast::<libc::c_void>(),
            infos.as_mut_ptr().cast::<libc::c_void>(),
        )
    };
    if count < 0 {
        return Err(Errno::last());
    }
    if count >= 4 {
        return Err(Errno::EOVERFLOW);
    } // never infer absence from a truncated queue
    Ok(infos.into_iter().take(count as usize).collect())
}

#[derive(Debug)]
struct Refusal {
    error: String,
    info: KernelInfo,
    orig: u64,
    rax: u64,
    rip: u64,
    rsp: u64,
    tool_error: bool,
    pending: Vec<SigsysInfo>,
}
#[derive(Default, Serialize, Deserialize)]
struct ReplayState {
    fd: Option<i32>,
    armed: bool,
    active: bool,
    #[serde(skip)]
    ticket: Option<PrivateInterruption>,
}
#[derive(Default)]
struct ReplayLog {
    setup: AtomicUsize,
    claims: AtomicUsize,
    tails: AtomicUsize,
    handlers: AtomicUsize,
    helper_entered: AtomicUsize,
    helper_returned: AtomicUsize,
    ordinary_denied: AtomicUsize,
    refusal: std::sync::Mutex<Option<Refusal>>,
    sigsys: std::sync::Mutex<Option<SigsysInfo>>,
    exits: std::sync::Mutex<Vec<ExitStatus>>,
}
#[reverie::global_tool]
impl GlobalTool for ReplayLog {
    type Config = u8;
    type Request = ExitStatus;
    type Response = ();
    async fn receive_rpc(&self, _: Pid, status: ExitStatus) {
        self.exits.lock().unwrap().push(status);
    }
}
#[derive(Default)]
struct ReplayTool;
#[reverie::tool]
impl Tool for ReplayTool {
    type GlobalState = ReplayLog;
    type ThreadState = ReplayState;
    fn subscriptions(_: &u8) -> Subscription {
        [Sysno::read, Sysno::write].into_iter().collect()
    }
    fn observe_injected_syscalls(_: &u8) -> bool {
        true
    }
    fn on_injected_syscall_observed(
        &self,
        _: Pid,
        log: &ReplayLog,
        state: &mut ReplayState,
        nr: Sysno,
        _: reverie::syscalls::SyscallArgs,
        event: reverie::InjectedSyscallEvent,
    ) {
        if state.active && nr == Sysno::getppid {
            match event {
                reverie::InjectedSyscallEvent::Entered => {
                    log.helper_entered.fetch_add(1, Ordering::SeqCst);
                }
                reverie::InjectedSyscallEvent::Returned(_) => {
                    log.helper_returned.fetch_add(1, Ordering::SeqCst);
                }
                _ => {}
            }
        }
    }
    async fn handle_private_interruption<G: Guest<Self>>(
        &self,
        guest: &mut G,
        event: &PrivateInterruption,
    ) -> Result<PrivateInterruptionAction, Error> {
        let state = guest.thread_state();
        if !state.active
            || state.ticket.is_some()
            || event.logical_call().0 != Sysno::read
            || event.logical_call().1.arg0 as i32 != state.fd.unwrap()
            || event.logical_call().1.arg2 != 6
            || event.helper() != reverie::syscalls::Getppid::new().into_parts()
            || event.signal() != Signal::SIGUSR1
        {
            return Err(anyhow::anyhow!("unexpected actual replay signal offer").into());
        }
        guest.claim_private_interruption(event)?;
        guest.thread_state_mut().ticket = Some(event.clone());
        guest
            .local_global_state()
            .unwrap()
            .claims
            .fetch_add(1, Ordering::SeqCst);
        Ok(PrivateInterruptionAction::FinishRead { completed: None })
    }
    async fn handle_private_read_completion<G: Guest<Self>>(
        &self,
        guest: &mut G,
        event: &PrivateReadCompletion,
    ) -> Result<(), Error> {
        if !guest
            .thread_state()
            .ticket
            .as_ref()
            .is_some_and(|old| old.same(event.interruption()))
            || event.completed().is_some()
        {
            return Err(anyhow::anyhow!("wrong replay Read completion").into());
        }
        guest.claim_private_read_completion(event)?;
        let actual = physical_regs(guest.tid())?;
        let observed = guest.regs().await;
        if actual.rax != (-i64::from(Errno::ERESTARTSYS.into_raw())) as u64
            || actual.orig_rax != Sysno::read as u64
            || (actual.rax, actual.orig_rax, actual.rip, actual.rsp)
                != (observed.rax, observed.orig_rax, observed.rip, observed.rsp)
        {
            return Err(anyhow::anyhow!("replay handback lacks actual logical registers").into());
        }
        guest.thread_state_mut().ticket = None;
        guest.thread_state_mut().active = false;
        guest
            .local_global_state()
            .unwrap()
            .tails
            .fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
    async fn handle_signal_event<G: Guest<Self>>(
        &self,
        guest: &mut G,
        signal: Signal,
    ) -> Result<Option<Signal>, Errno> {
        if signal == Signal::SIGSYS {
            let info = physical_siginfo(guest.tid())?;
            *guest.local_global_state().unwrap().sigsys.lock().unwrap() = Some(info);
            // Preserve the physical filter trap as a backend failure. Never
            // suppress it and pretend that the requested replay signal arrived.
            return Err(Errno::EPROTO);
        }
        Ok(Some(signal))
    }
    async fn handle_syscall_event<G: Guest<Self>>(
        &self,
        guest: &mut G,
        call: Syscall,
    ) -> Result<i64, Error> {
        let (nr, args) = call.into_parts();
        if nr == Sysno::write && args.arg0 == 880 {
            guest.thread_state_mut().fd = Some(args.arg1 as i32);
            guest.thread_state_mut().armed = true;
            return Ok(0);
        }
        if nr == Sysno::write && args.arg0 == 879 {
            if guest.thread_state().active
                || guest
                    .local_global_state()
                    .unwrap()
                    .tails
                    .load(Ordering::SeqCst)
                    != 1
            {
                return Err(anyhow::anyhow!("signal handler preceded logical completion").into());
            }
            guest
                .local_global_state()
                .unwrap()
                .handlers
                .fetch_add(1, Ordering::SeqCst);
            return Ok(1);
        }
        if nr != Sysno::read
            || Some(args.arg0 as i32) != guest.thread_state().fd
            || !guest.thread_state().armed
        {
            return Ok(guest.inject(call).await?);
        }
        guest.thread_state_mut().armed = false;
        let mode = *guest.config();
        if mode == 6 {
            let result = guest.inject(Getpid::new()).await;
            if result != Err(Errno::EACCES) {
                return Err(
                    anyhow::anyhow!("ordinary Tool operation bypassed its guest filter").into(),
                );
            }
            guest
                .local_global_state()
                .unwrap()
                .ordinary_denied
                .fetch_add(1, Ordering::SeqCst);
            return Ok(0);
        }
        if mode == 4 || mode == 5 {
            // The original Read is still at its authenticated SECCOMP stop.
            // Queue a real signal from this tracer process without injection.
            if unsafe {
                libc::syscall(
                    libc::SYS_tgkill,
                    guest.pid().as_raw(),
                    guest.tid().as_raw(),
                    libc::SIGUSR1,
                )
            } != 0
            {
                return Err(Errno::last().into());
            }
        } else {
            let pid = guest.inject(Getpid::new()).await?;
            if mode == 2 || mode == 3 {
                let tid = guest.inject(Gettid::new()).await?;
                guest
                    .inject(
                        Tgkill::new()
                            .with_tgid(pid as _)
                            .with_tid(tid as _)
                            .with_sig(libc::SIGUSR1),
                    )
                    .await?;
            }
            // The preceding real Getpid/Tgkill has an actual owned EXIT.
        }
        guest.thread_state_mut().active = true;
        guest
            .local_global_state()
            .unwrap()
            .setup
            .fetch_add(1, Ordering::SeqCst);
        match guest
            .await_recorded_private_interruption(
                reverie::syscalls::Getppid::new().into(),
                Signal::SIGUSR1,
            )
            .await
        {
            Ok(never) => match never {},
            Err(error) => {
                // Read the physical stop independently before propagating the
                // backend error. The oracle runs only after original cleanup.
                let info = kernel_info(guest.tid())?;
                let regs = physical_regs(guest.tid())?;
                // Nonconsuming read: a TRAP filter can queue SIGSYS before
                // the ordinary syscall EXIT, while delivery is still pending.
                let pending = pending_siginfo(guest.tid())?;
                if info.op == 0 {
                    // An unexpected signal may be refused inside the private
                    // waiter before the ordinary Tool signal callback is reached.
                    // Authenticate that same held stop, not a later callback.
                    let signal = physical_siginfo(guest.tid())?;
                    *guest.local_global_state().unwrap().sigsys.lock().unwrap() = Some(signal);
                }
                *guest.local_global_state().unwrap().refusal.lock().unwrap() = Some(Refusal {
                    error: format!("{error}"),
                    info,
                    orig: regs.orig_rax,
                    rax: regs.rax,
                    rip: regs.rip,
                    rsp: regs.rsp,
                    tool_error: matches!(&error, Error::Tool(_)),
                    pending,
                });
                Err(error)
            }
        }
    }
    async fn on_exit_thread<G: reverie::GlobalRPC<ReplayLog>>(
        &self,
        _: Tid,
        global: &G,
        _: ReplayState,
        status: ExitStatus,
    ) -> Result<(), Error> {
        global.send_rpc(status).await;
        Ok(())
    }
}

fn fixture() -> PathBuf {
    let path = PathBuf::from(crate::testing::fixture_path(
        "REVERIE_PRIVATE_REPLAY_FIXTURE",
    ));
    assert!(path.is_absolute(), "no PATH compiler or fixture lookup");
    path
}
async fn replay_case(mode: u8) {
    let mut command = Command::new(fixture());
    command.arg(mode.to_string());
    command.stdout(reverie::process::Stdio::piped());
    command.stderr(reverie::process::Stdio::piped());
    let started = Instant::now();
    let deadline = started + Duration::from_secs(5);
    let tracer = TracerBuilder::<ReplayTool>::new(command)
        .config(mode)
        .spawn()
        .await
        .unwrap();
    let root = tracer.guest_pid();
    let termination = tracer
        .termination_handle()
        .expect("ordinary original owner");
    let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, root.as_raw(), 0) };
    let open_error = (fd < 0).then(std::io::Error::last_os_error);
    let pidfd = (fd >= 0).then(|| unsafe { OwnedFd::from_raw_fd(fd as i32) });
    if let Some(error) = &open_error {
        termination.terminate(anyhow::anyhow!("replay original pidfd: {error}").into());
    }
    let mut future = Box::pin(tracer.wait_with_output_completion());
    let outcome = tokio::time::timeout(
        deadline.saturating_duration_since(Instant::now()),
        &mut future,
    )
    .await;
    let complete = match outcome {
        Ok(ToolRunOutcome::Complete(complete)) => complete,
        other => {
            termination.terminate(anyhow::anyhow!("replay original deadline/refusal").into());
            let rescue = match other {
                Err(_) => tokio::time::timeout(Duration::from_secs(2), &mut future).await,
                Ok(ToolRunOutcome::CleanupPending(pending)) => {
                    tokio::time::timeout(Duration::from_secs(2), pending.resume_cleanup()).await
                }
                Ok(ToolRunOutcome::UnsupportedBackend(tracer)) => {
                    tokio::time::timeout(
                        Duration::from_secs(2),
                        tracer.wait_with_output_completion(),
                    )
                    .await
                }
                Ok(ToolRunOutcome::Complete(_)) => unreachable!(),
            };
            if let Ok(ToolRunOutcome::CleanupPending(pending)) = rescue {
                eprintln!("replay rescue unconfirmed: {:#}", pending.quarantine());
            }
            panic!("original replay fixture did not Complete under its deadline");
        }
    };
    assert!(started.elapsed() <= Duration::from_secs(5));
    assert!(
        open_error.is_none(),
        "original pidfd acquisition: {open_error:?}"
    );
    let pidfd = pidfd.unwrap();
    let mut poll = libc::pollfd {
        fd: pidfd.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    assert_eq!(unsafe { libc::poll(&mut poll, 1, 0) }, 1);
    assert_ne!(poll.revents & libc::POLLIN, 0);
    assert_eq!(poll.revents & (libc::POLLERR | libc::POLLNVAL), 0);
    let mut status = 0;
    assert_eq!(
        unsafe { libc::waitpid(root.as_raw(), &mut status, libc::WNOHANG) },
        -1
    );
    assert_eq!(
        Errno::last(),
        Errno::ECHILD,
        "original owner must reap first"
    );
    drop(pidfd);
    eprintln!("REPLAY_TRANSPORT original_reaped=1 pidfd_closed=1 mode={mode}");
    let log = complete.global_state;
    let snapshot = log.refusal.lock().unwrap();
    let sigsys = log.sigsys.lock().unwrap();
    eprintln!("REPLAY_TRANSPORT refusal={snapshot:?} sigsys={sigsys:?}");
    assert_eq!(log.helper_entered.load(Ordering::SeqCst), 0);
    assert_eq!(log.helper_returned.load(Ordering::SeqCst), 0);
    if mode <= 1 {
        assert!(
            complete.result.is_err(),
            "no-pending refusal is a backend failure, never guest errno"
        );
        // These exact physical old-path diagnostics precede the same invariant
        // assertions, so an unrelated error cannot qualify as the old negative.
        if let Some(info) = sigsys.as_ref() {
            assert_eq!(
                (info.signo, info.code, info.syscall, info.arch),
                (libc::SIGSYS, 1, -1, 0xc000003e),
                "actual SYS_SECCOMP(-1), not another signal"
            );
        }
        if let Some(observed) = snapshot.as_ref()
            && observed.info.op == 2
        {
            let raw = if mode == 0 {
                -i64::from(Errno::EACCES.into_raw())
            } else {
                -1
            };
            assert_eq!(observed.info.nr_or_result as i64, raw);
            assert_eq!((observed.orig, observed.rax), (-1i64 as u64, raw as u64));
            if mode == 1 {
                assert_eq!(
                    observed.pending.len(),
                    1,
                    "actual queued TRAP, not a delivered signal"
                );
                let queued = &observed.pending[0];
                assert_eq!(
                    (queued.signo, queued.code, queued.syscall, queued.arch),
                    (libc::SIGSYS, 1, -1, 0xc000003e),
                    "actual pending SYS_SECCOMP(-1)"
                );
                assert_eq!(queued.errno, 0);
                assert_eq!(queued.call_addr, observed.info.ip);
            }
        }
        assert!(
            sigsys.is_none(),
            "finite transport must not run the guest's -1 filter"
        );
        let observed = snapshot
            .as_ref()
            .expect("finite refusal preserves its actual ENTRY");
        assert!(
            !observed
                .pending
                .iter()
                .any(|info| info.signo == libc::SIGSYS),
            "finite transport must not queue the guest's -1 filter SIGSYS"
        );
        assert_eq!(
            observed.info.op, 1,
            "one finite emulation ENTRY, not a filter EXIT/signal"
        );
        assert_eq!(observed.info.nr_or_result, Sysno::getppid as u64);
        assert_eq!(observed.orig, Sysno::getppid as u64);
        assert_eq!(observed.info.args, [0; 6]);
        assert_eq!(
            observed.info.ip,
            (crate::cp::PRIVATE_PAGE_OFFSET + crate::cp::SYSCALL_INSTR_SIZE) as u64
        );
        assert_eq!(
            (observed.rip, observed.rsp),
            (observed.info.ip, observed.info.sp)
        );
        assert!(observed.tool_error, "refusal cannot escape as guest Errno");
        assert!(
            observed
                .error
                .contains(&format!("recorded private interruption: {}", Errno::EAGAIN)),
            "exact backend not-ready refusal, not an arbitrary error"
        );
        assert_eq!(log.setup.load(Ordering::SeqCst), 1);
        assert_eq!(log.claims.load(Ordering::SeqCst), 0);
        assert_eq!(log.tails.load(Ordering::SeqCst), 0);
        assert_eq!(log.handlers.load(Ordering::SeqCst), 0);
    } else {
        let output = complete.result.unwrap();
        assert_eq!(output.status, ExitStatus::Exited(0));
        assert!(output.stdout.is_empty() && output.stderr.is_empty());
        assert!(snapshot.is_none() && sigsys.is_none());
        let pending = usize::from(mode != 6);
        assert_eq!(log.claims.load(Ordering::SeqCst), pending);
        assert_eq!(log.tails.load(Ordering::SeqCst), pending);
        assert_eq!(log.handlers.load(Ordering::SeqCst), pending);
        assert_eq!(
            log.ordinary_denied.load(Ordering::SeqCst),
            usize::from(mode == 6)
        );
    }
}
#[tokio::test(flavor = "current_thread")]
async fn no_pending_errno_filter_refuses_without_filter_exit() {
    replay_case(0).await;
}
#[tokio::test(flavor = "current_thread")]
async fn no_pending_trap_filter_refuses_without_sigsys() {
    replay_case(1).await;
}
#[tokio::test(flavor = "current_thread")]
async fn pending_from_exit_preserves_errno_filter() {
    replay_case(2).await;
}
#[tokio::test(flavor = "current_thread")]
async fn pending_from_exit_preserves_trap_filter() {
    replay_case(3).await;
}
#[tokio::test(flavor = "current_thread")]
async fn pending_from_seccomp_preserves_errno_filter() {
    replay_case(4).await;
}
#[tokio::test(flavor = "current_thread")]
async fn pending_from_seccomp_preserves_trap_filter() {
    replay_case(5).await;
}
#[tokio::test(flavor = "current_thread")]
async fn ordinary_tool_getpid_still_obeys_errno_filter() {
    replay_case(6).await;
}
#[tokio::test(flavor = "current_thread")]
async fn ordinary_entry_is_refused_without_resume_or_register_change() {
    use std::io::Read;
    use std::os::unix::process::ExitStatusExt;
    // One actual TRACEME child; no Runtime/SourceStop claim is constructed.
    // This same host thread owns all waitpid stops and the final terminal wait.
    let started = Instant::now();
    let deadline = started + Duration::from_secs(5);
    let mut child = std::process::Command::new(fixture())
        .arg("8")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let pid = child.id() as i32;
    let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0) };
    let pidfd = (fd >= 0).then(|| unsafe { OwnedFd::from_raw_fd(fd as i32) });
    async fn stopped(pid: i32, deadline: Instant) -> Result<i32, anyhow::Error> {
        loop {
            let mut status = 0;
            let got = unsafe { libc::waitpid(pid, &mut status, libc::WNOHANG | libc::WUNTRACED) };
            if got == pid {
                if !libc::WIFSTOPPED(status) {
                    anyhow::bail!("unexpected terminal status {status}");
                }
                return Ok(libc::WSTOPSIG(status));
            }
            if got < 0 {
                return Err(std::io::Error::last_os_error().into());
            }
            if Instant::now() >= deadline {
                anyhow::bail!("original stop deadline");
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    }
    let result: Result<(bool, bool, bool), anyhow::Error> = async {
        if pidfd.is_none() {
            anyhow::bail!("original pidfd open failed");
        }
        if stopped(pid, deadline).await? != libc::SIGSTOP {
            anyhow::bail!("wrong initial stop");
        }
        if unsafe {
            libc::ptrace(
                libc::PTRACE_SETOPTIONS,
                pid,
                std::ptr::null_mut::<libc::c_void>(),
                libc::PTRACE_O_TRACESYSGOOD as usize,
            )
        } != 0
        {
            return Err(std::io::Error::last_os_error().into());
        }
        if unsafe {
            libc::ptrace(
                libc::PTRACE_SYSCALL,
                pid,
                std::ptr::null_mut::<libc::c_void>(),
                std::ptr::null_mut::<libc::c_void>(),
            )
        } != 0
        {
            return Err(std::io::Error::last_os_error().into());
        }
        if stopped(pid, deadline).await? != (libc::SIGTRAP | 0x80) {
            anyhow::bail!("wrong ENTRY stop");
        }
        let tid = Pid::from_raw(pid);
        let info = kernel_info(tid)?;
        if info.op != 1 || info.nr_or_result != Sysno::getpid as u64 {
            anyhow::bail!("not the actual ordinary Getpid ENTRY: {info:?}");
        }
        let regs = physical_regs(tid)?;
        // The original live child, original pidfd and just-consumed ptrace stop
        // establish this raw-control premise; this is not source-authority issuance.
        let task = safeptrace::Stopped::new_unchecked(tid);
        let refused = matches!(
            task.sysemu_from_exit(),
            Err(safeptrace::Error::Errno(Errno::EPROTO))
        );
        let after = kernel_info(tid)?;
        let after_regs = physical_regs(tid)?;
        let before_words: [u64; 27] = unsafe { std::mem::transmute(regs) };
        let after_words: [u64; 27] = unsafe { std::mem::transmute(after_regs) };
        Ok((refused, info == after, before_words == after_words))
    }
    .await;
    // Cleanup runs even when the control path refuses. This child creates no
    // descendants; its streams close only after this original process exits.
    let kill = if let Some(fd) = &pidfd {
        unsafe {
            libc::syscall(
                libc::SYS_pidfd_send_signal,
                fd.as_raw_fd(),
                libc::SIGKILL,
                std::ptr::null::<libc::siginfo_t>(),
                0,
            )
        }
    } else {
        unsafe { libc::kill(pid, libc::SIGKILL) as libc::c_long }
    };
    let kill_error = (kill < 0).then(Errno::last);
    let cleanup_deadline = Instant::now() + Duration::from_secs(2);
    let terminal = loop {
        let mut status = 0;
        let got = unsafe { libc::waitpid(pid, &mut status, libc::WNOHANG | libc::WUNTRACED) };
        if got < 0 {
            break Err(std::io::Error::last_os_error());
        }
        if got == pid {
            if libc::WIFEXITED(status) || libc::WIFSIGNALED(status) {
                break Ok(std::process::ExitStatus::from_raw(status));
            }
            if !libc::WIFSTOPPED(status) {
                break Err(std::io::Error::other(
                    "unexpected nonterminal cleanup status",
                ));
            }
            // A failed guard could have resumed into another ptrace stop.
            // SIGKILL is already pending; do not mislabel that stop as reap.
            let resumed = unsafe {
                libc::ptrace(
                    libc::PTRACE_CONT,
                    pid,
                    std::ptr::null_mut::<libc::c_void>(),
                    std::ptr::null_mut::<libc::c_void>(),
                )
            };
            if resumed != 0 && Errno::last() != Errno::ESRCH {
                break Err(std::io::Error::last_os_error());
            }
        }
        if Instant::now() >= cleanup_deadline {
            break Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "original child cleanup",
            ));
        }
        tokio::time::sleep(Duration::from_millis(1)).await;
    };
    // No causal verdict can precede actual reaping and EOF.
    let terminal = terminal.expect("original native child must be reaped");
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    child
        .stdout
        .take()
        .unwrap()
        .take(4097)
        .read_to_end(&mut stdout)
        .unwrap();
    child
        .stderr
        .take()
        .unwrap()
        .take(4097)
        .read_to_end(&mut stderr)
        .unwrap();
    let mut status = 0;
    let remaining = unsafe { libc::waitpid(pid, &mut status, libc::WNOHANG) };
    let no_child = Errno::last();
    drop(pidfd);
    drop(child);
    assert_eq!(remaining, -1);
    assert_eq!(no_child, Errno::ECHILD);
    assert!(kill_error.is_none() || kill_error == Some(Errno::ESRCH));
    assert_eq!(terminal.signal(), Some(libc::SIGKILL));
    assert!(stdout.is_empty() && stderr.is_empty());
    assert!(started.elapsed() <= Duration::from_secs(5));
    eprintln!("REPLAY_TRANSPORT ordinary_ENTRY original_reaped=1 EOF=1 pidfd_closed=1");
    assert_eq!(
        result.unwrap(),
        (true, true, true),
        "ordinary ENTRY cannot switch transport or alter the physical context"
    );
}
