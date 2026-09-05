use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;

use reverie::Error;
use reverie::GlobalTool;
use reverie::Guest;
use reverie::Pid;
use reverie::Rdtsc;
use reverie::RdtscResult;
use reverie::Subscription;
use reverie::Tool;
use reverie::syscalls::Syscall;
use reverie::syscalls::SyscallInfo;
use reverie::syscalls::Sysno;
use reverie_preload::clock_boundary::Continuation;
use reverie_preload::trap::raw_syscall6;

static WORK: AtomicU64 = AtomicU64::new(0);
static CALLBACKS: AtomicU64 = AtomicU64::new(0);
static NOTIFICATIONS: AtomicU64 = AtomicU64::new(0);
static PID: AtomicU64 = AtomicU64::new(0);
static TID: AtomicU64 = AtomicU64::new(0);
static WINDOW_PC: AtomicU64 = AtomicU64::new(0);
static WINDOW_FD: AtomicU64 = AtomicU64::new(u64::MAX);
static WINDOW_HITS: AtomicU64 = AtomicU64::new(0);
static WINDOW_KIND: AtomicU64 = AtomicU64::new(0);
static WINDOW_GATE: AtomicU64 = AtomicU64::new(0);
static SUD: AtomicU64 = AtomicU64::new(0);
static UID: AtomicU64 = AtomicU64::new(0);
static EXPECTED_NOTIFICATION: AtomicU64 = AtomicU64::new(0);
static PENDING_UNMASK: AtomicU64 = AtomicU64::new(0);
static UNMASK_EXPECTED: AtomicU64 = AtomicU64::new(0);
static GUEST_MASK: AtomicU64 = AtomicU64::new(0);
static FORCE_INSTRUCTION: AtomicU64 = AtomicU64::new(0);
static MASK_NATIVE_SCOPE_NEGATIVE: AtomicU64 = AtomicU64::new(0);

unsafe extern "C" {
    fn clock_fixture_open_breakpoint(address: u64) -> i32;
    fn clock_fixture_owned_breakpoint(info: *const libc::siginfo_t, address: u64) -> i32;
    fn clock_fixture_notification(info: *mut libc::siginfo_t, pid: i32, uid: u32, token: u64);
    fn clock_fixture_owned_notification(
        info: *const libc::siginfo_t,
        pid: i32,
        uid: u32,
        token: u64,
    ) -> i32;
    fn reverie_liteinst_fallback_mask_restored();
    fn reverie_liteinst_instruction_scope_enter(selected: u64) -> u64;
    fn reverie_liteinst_instruction_scope_leave(token: u64);
}

#[unsafe(naked)]
unsafe extern "C" fn native_scope_enter() -> u64 {
    core::arch::naked_asm!(
        "push rbx", "push r12", "sub rsp, 24",
        "cmp qword ptr [rip + {negative}], 0", "je 2f",
        "mov qword ptr [rsp], 16", "mov eax, 14", "xor edi, edi",
        "mov rsi, rsp", "lea rdx, [rsp + 8]", "mov r10d, 8",
        "call reverie_preload_trusted_syscall_ip", "test rax, rax", "jnz 9f",
        "2:",
        "mov edi, 3", "call {enter}", "mov r12, rax",
        "cmp qword ptr [rip + {negative}], 0", "je 3f",
        "mov eax, 14", "mov edi, 2", "lea rsi, [rsp + 8]", "xor edx, edx",
        "mov r10d, 8", "call reverie_preload_trusted_syscall_ip",
        "test rax, rax", "jnz 9f", "3:",
        "xor eax, eax", "cpuid", "rdtsc", "rdtscp",
        "mov rax, r12", "add rsp, 24", "pop r12", "pop rbx", "ret",
        "9:", "mov eax, 231", "mov edi, 125",
        "call reverie_preload_trusted_syscall_ip", "ud2",
        enter = sym reverie_liteinst_instruction_scope_enter,
        negative = sym MASK_NATIVE_SCOPE_NEGATIVE,
    );
}

#[unsafe(naked)]
unsafe extern "C" fn native_scope_leave(token: u64) {
    core::arch::naked_asm!(
        "push rbx", "push r12", "sub rsp, 8", "mov r12, rdi",
        "xor eax, eax", "cpuid", "rdtsc", "rdtscp",
        "mov rdi, r12", "call {leave}",
        "mov eax, 0xdead", "mov edx, 0xbeef",
        "add rsp, 8", "pop r12", "pop rbx", "ret",
        leave = sym reverie_liteinst_instruction_scope_leave,
    );
}

static NATIVE_SCOPE: reverie_preload::clock_boundary::SignalScope =
    reverie_preload::clock_boundary::SignalScope {
        enter: native_scope_enter,
        leave: native_scope_leave,
    };

fn queue_notification(pending: bool) {
    let token = NOTIFICATIONS.load(Ordering::Relaxed) + 1;
    assert_eq!(EXPECTED_NOTIFICATION.swap(token, Ordering::Relaxed), 0);
    if pending {
        UNMASK_EXPECTED.store(1, Ordering::Relaxed);
        let mask = 1u64 << (libc::SIGUSR2 - 1);
        assert_eq!(
            unsafe {
                raw_syscall6(
                    libc::SYS_rt_sigprocmask,
                    [libc::SIG_BLOCK as u64, (&raw const mask) as u64, 0, 8, 0, 0],
                )
            },
            0
        );
    }
    let mut info: libc::siginfo_t = unsafe { core::mem::zeroed() };
    unsafe {
        clock_fixture_notification(
            &raw mut info,
            PID.load(Ordering::Relaxed) as i32,
            UID.load(Ordering::Relaxed) as u32,
            token,
        )
    };
    assert_eq!(
        unsafe {
            raw_syscall6(
                libc::SYS_rt_tgsigqueueinfo,
                [
                    PID.load(Ordering::Relaxed),
                    TID.load(Ordering::Relaxed),
                    libc::SIGUSR2 as u64,
                    (&raw const info) as u64,
                    0,
                    0,
                ],
            )
        },
        0
    );
    if pending {
        assert_eq!(NOTIFICATIONS.load(Ordering::Relaxed) + 1, token);
    }
}

fn work() {
    for iteration in 0..WORK.load(Ordering::Relaxed) {
        std::hint::black_box(iteration);
    }
}

struct Cleanup;
impl Drop for Cleanup {
    fn drop(&mut self) {
        work();
    }
}

#[derive(Default)]
pub struct ClockGlobal;

#[reverie::global_tool]
impl GlobalTool for ClockGlobal {
    type Request = u64;
    type Response = u64;
    type Config = ();

    async fn receive_rpc(&self, _from: reverie::Tid, request: u64) -> u64 {
        request + 1
    }
}

#[derive(Default)]
struct ClockTool;

async fn sample<G: Guest<ClockTool>>(guest: &mut G) -> u64 {
    let _cleanup = Cleanup;
    if PENDING_UNMASK.load(Ordering::Relaxed) != 0 {
        assert_eq!(
            NOTIFICATIONS.load(Ordering::Relaxed),
            CALLBACKS.load(Ordering::Relaxed) * 2
        );
    }
    let before = guest
        .read_clock()
        .expect("real paused hardware clock required");
    if SUD.load(Ordering::Relaxed) != 0 {
        let mut mask = 0u64;
        let query = Syscall::from_raw(
            Sysno::rt_sigprocmask,
            reverie::syscalls::SyscallArgs::new(123, 0, (&raw mut mask) as usize, 8, 0, 0),
        );
        assert_eq!(guest.inject(query).await, Ok(0));
        assert_eq!(mask, GUEST_MASK.load(Ordering::Relaxed));
    }
    assert!(guest.set_timer(reverie::TimerSchedule::Rcbs(100)).is_err());
    assert!(
        guest
            .set_timer_precise(reverie::TimerSchedule::Rcbs(100))
            .is_err()
    );
    assert!(
        guest
            .set_timer_precise(reverie::TimerSchedule::RcbsAndInstructions(0, 1))
            .is_err()
    );
    work();
    let uid = unsafe { nested_uid() };
    assert!(uid >= 0);
    assert_eq!(guest.send_rpc(before).await, before + 1);
    let notifications = NOTIFICATIONS.load(Ordering::Relaxed);
    let callbacks = CALLBACKS.load(Ordering::Relaxed);
    if SUD.load(Ordering::Relaxed) != 0 {
        queue_notification(false);
    } else {
        unsafe {
            assert_eq!(
                raw_syscall6(
                    libc::SYS_tgkill,
                    [
                        PID.load(Ordering::Relaxed),
                        TID.load(Ordering::Relaxed),
                        libc::SIGUSR2 as u64,
                        0,
                        0,
                        0
                    ]
                ),
                0
            );
        }
    }
    assert_eq!(NOTIFICATIONS.load(Ordering::Relaxed), notifications + 1);
    assert_eq!(
        CALLBACKS.load(Ordering::Relaxed),
        callbacks,
        "recursive Tool dispatch"
    );
    assert_eq!(
        guest
            .read_clock()
            .expect("real paused hardware clock required"),
        before,
        "nested Tool/RPC/notification work changes guest clock"
    );
    CALLBACKS.fetch_add(1, Ordering::Relaxed);
    if WINDOW_PC.load(Ordering::Relaxed) != 0 {
        assert_eq!(
            WINDOW_HITS.load(Ordering::Relaxed),
            1,
            "actual boundary interrupt missing"
        );
    }
    if PENDING_UNMASK.load(Ordering::Relaxed) != 0 {
        queue_notification(true);
    }
    if SUD.load(Ordering::Relaxed) != 0 {
        let stats = reverie_liteinst::syscall_mode_stats();
        assert_eq!(
            (
                stats.planning_attempts,
                stats.patch_attempts,
                stats.installed_patches,
                stats.vdso_rewrite_attempts
            ),
            (0, 0, 0, 0)
        );
        assert!(stats.deferred_sud > 0);
    }
    before
}

#[reverie::tool]
impl Tool for ClockTool {
    type GlobalState = ClockGlobal;
    type ThreadState = ();

    fn new(_pid: Pid, _config: &()) -> Self {
        work();
        Self
    }

    fn subscriptions(_config: &()) -> Subscription {
        let mut subscriptions: Subscription = [Sysno::getpid, Sysno::getuid].into_iter().collect();
        if SUD.load(Ordering::Relaxed) == 0 || FORCE_INSTRUCTION.load(Ordering::Relaxed) != 0 {
            subscriptions.rdtsc();
        }
        subscriptions
    }

    async fn handle_syscall_event<G: Guest<Self>>(
        &self,
        guest: &mut G,
        syscall: Syscall,
    ) -> Result<i64, Error> {
        if syscall.number() == Sysno::getpid {
            Ok(sample(guest).await as i64)
        } else {
            Ok(guest.inject(syscall).await?)
        }
    }

    async fn handle_rdtsc_event<G: Guest<Self>>(
        &self,
        guest: &mut G,
        _request: Rdtsc,
    ) -> Result<RdtscResult, reverie::Errno> {
        Ok(RdtscResult {
            tsc: sample(guest).await,
            aux: None,
        })
    }
}

#[unsafe(naked)]
unsafe extern "C" fn nested_uid() -> i64 {
    core::arch::naked_asm!(
        "mov eax, 102",
        ".p2align 3",
        "syscall",
        "nop",
        "nop",
        "nop",
        "nop",
        "nop",
        "nop",
        "ret"
    );
}

unsafe extern "C" fn notification_body(
    _signal: i32,
    _info: *mut libc::siginfo_t,
    _frame: *mut libc::c_void,
) -> Continuation {
    NOTIFICATIONS.fetch_add(1, Ordering::Relaxed);
    work();
    Continuation::RUNTIME
}

reverie_preload::clocked_signal!(notification, notification_body);

unsafe extern "C" fn validate_notification(
    signal: i32,
    info: *mut libc::siginfo_t,
    frame: *mut libc::c_void,
) -> bool {
    let token = EXPECTED_NOTIFICATION.load(Ordering::Relaxed);
    if signal != libc::SIGUSR2
        || token == 0
        || unsafe {
            clock_fixture_owned_notification(
                info,
                PID.load(Ordering::Relaxed) as i32,
                UID.load(Ordering::Relaxed) as u32,
                token,
            )
        } != 1
    {
        return false;
    }
    if UNMASK_EXPECTED.swap(0, Ordering::Relaxed) != 0 {
        let frame = unsafe { &*frame.cast::<libc::ucontext_t>() };
        assert_eq!(
            frame.uc_mcontext.gregs[libc::REG_RIP as usize] as u64,
            reverie_preload::trap::trusted_gate().return_ip
        );
        assert_eq!(
            unsafe { *(frame.uc_mcontext.gregs[libc::REG_RSP as usize] as *const u64) },
            reverie_liteinst_fallback_mask_restored as *const () as u64
        );
        assert_eq!(reverie_preload::user_dispatch::dispatch_mask(), None);
    }
    EXPECTED_NOTIFICATION
        .compare_exchange(token, 0, Ordering::Relaxed, Ordering::Relaxed)
        .is_ok()
}

unsafe extern "C" fn validate_window(
    signal: i32,
    info: *mut libc::siginfo_t,
    frame: *mut libc::c_void,
) -> bool {
    signal == libc::SIGTRAP
        && unsafe { clock_fixture_owned_breakpoint(info, WINDOW_PC.load(Ordering::Relaxed)) } == 1
        && unsafe {
            (*frame.cast::<libc::ucontext_t>()).uc_mcontext.gregs[libc::REG_RIP as usize] as u64
        } == WINDOW_PC.load(Ordering::Relaxed)
}

unsafe extern "C" fn window_body(
    signal: i32,
    info: *mut libc::siginfo_t,
    frame: *mut libc::c_void,
) -> Continuation {
    let target = WINDOW_PC.load(Ordering::Relaxed);
    assert_eq!(signal, libc::SIGTRAP);
    assert_eq!(unsafe { clock_fixture_owned_breakpoint(info, target) }, 1);
    let frame = unsafe { &*frame.cast::<libc::ucontext_t>() };
    assert_eq!(
        frame.uc_mcontext.gregs[libc::REG_RIP as usize] as u64,
        target
    );
    if WINDOW_KIND.load(Ordering::Relaxed) == 1 {
        assert_eq!(
            frame.uc_mcontext.gregs[libc::REG_R13 as usize],
            1,
            "stop window must interrupt a guest-owned, not already-paused activation"
        );
    }
    assert_eq!(
        unsafe {
            raw_syscall6(
                libc::SYS_ioctl,
                [WINDOW_FD.load(Ordering::Relaxed), 0x2401, 0, 0, 0, 0],
            )
        },
        0
    );
    assert_eq!(WINDOW_HITS.fetch_add(1, Ordering::Relaxed), 0);
    let callbacks = CALLBACKS.load(Ordering::Relaxed);
    work();
    assert_eq!(CALLBACKS.load(Ordering::Relaxed), callbacks);
    Continuation::RUNTIME
}

reverie_preload::clocked_signal!(window_owned, window_body);

#[unsafe(naked)]
unsafe extern "C" fn window_handler(
    signal: i32,
    info: *mut libc::siginfo_t,
    frame: *mut libc::c_void,
) {
    core::arch::naked_asm!(
        "push rdi", "push rsi", "push rdx",
        "mov eax, 16", "mov rdi, [rip + {descriptor}]", "mov esi, 0x2401", "xor edx, edx",
        "call qword ptr [rip + {gate}]",
        "lea rdx, [rip + 2f]", "lea rcx, [rip + 3f]", "test rax, rax", "cmovne rdx, rcx", "jmp rdx",
        "2:", "pop rdx", "pop rsi", "pop rdi", "jmp {owned}",
        "3:", "mov eax, 231", "mov edi, 127", "jmp qword ptr [rip + {gate}]",
        descriptor = sym WINDOW_FD, gate = sym WINDOW_GATE, owned = sym window_owned,
    );
}

#[repr(C)]
#[derive(Default)]
struct KernelAction {
    handler: u64,
    flags: u64,
    restorer: u64,
    mask: u64,
}

unsafe fn install_notification() -> bool {
    let mut existing = KernelAction::default();
    if unsafe {
        raw_syscall6(
            libc::SYS_rt_sigaction,
            [libc::SIGSYS as u64, 0, (&raw mut existing) as u64, 8, 0, 0],
        )
    } != 0
        || existing.restorer == 0
    {
        return false;
    }
    let action = KernelAction {
        handler: notification as *const () as u64,
        flags: (libc::SA_SIGINFO as u64) | 0x04000000,
        restorer: existing.restorer,
        mask: 1 << (libc::SIGUSR2 - 1),
    };
    if unsafe {
        raw_syscall6(
            libc::SYS_rt_sigaction,
            [libc::SIGUSR2 as u64, (&raw const action) as u64, 0, 8, 0, 0],
        )
    } != 0
    {
        return false;
    }
    if WINDOW_PC.load(Ordering::Relaxed) != 0 {
        let action = KernelAction {
            handler: window_handler as *const () as u64,
            flags: (libc::SA_SIGINFO as u64) | 0x04000000,
            restorer: existing.restorer,
            mask: 1 << (libc::SIGTRAP - 1),
        };
        if unsafe {
            raw_syscall6(
                libc::SYS_rt_sigaction,
                [libc::SIGTRAP as u64, (&raw const action) as u64, 0, 8, 0, 0],
            )
        } != 0
        {
            return false;
        }
    }
    true
}

unsafe extern "C" fn initialize() -> i32 {
    let Some(socket) = std::env::var_os("CLOCK_FIXTURE_SOCKET") else {
        return 0;
    };
    if std::env::var_os("CLOCK_FIXTURE_FAIL").is_some() {
        return 42;
    }
    MASK_NATIVE_SCOPE_NEGATIVE.store(
        u64::from(std::env::var_os("CLOCK_FIXTURE_MASK_NATIVE_NEGATIVE").is_some()),
        Ordering::Relaxed,
    );
    if std::env::var_os("CLOCK_FIXTURE_NATIVE_SCOPE").is_some()
        && unsafe { reverie_preload::clock_boundary::register_signal_scope(&NATIVE_SCOPE) }.is_err()
    {
        return 42;
    }
    SUD.store(
        u64::from(std::env::var_os("CLOCK_FIXTURE_SUD").is_some()),
        Ordering::Relaxed,
    );
    FORCE_INSTRUCTION.store(
        u64::from(std::env::var_os("CLOCK_FIXTURE_INSTRUCTIONS").is_some()),
        Ordering::Relaxed,
    );
    PENDING_UNMASK.store(
        u64::from(std::env::var_os("CLOCK_FIXTURE_PENDING").is_some()),
        Ordering::Relaxed,
    );
    let Some(iterations) = std::env::var("CLOCK_FIXTURE_WORK")
        .ok()
        .and_then(|value| value.parse().ok())
    else {
        return 42;
    };
    WORK.store(iterations, Ordering::Relaxed);
    if let Ok(offset) = std::env::var("CLOCK_FIXTURE_WINDOW") {
        let Ok(offset) = u64::from_str_radix(&offset, 16) else {
            return 42;
        };
        let mut info: libc::Dl_info = unsafe { std::mem::zeroed() };
        if unsafe { libc::dladdr(initialize as *const () as *const _, &mut info) } == 0 {
            return 42;
        }
        let Some(target) = (info.dli_fbase as u64).checked_add(offset) else {
            return 42;
        };
        WINDOW_PC.store(target, Ordering::Relaxed);
        WINDOW_GATE.store(
            reverie_preload::trap::trusted_gate().syscall_ip,
            Ordering::Relaxed,
        );
        WINDOW_KIND.store(
            std::env::var("CLOCK_FIXTURE_WINDOW_KIND")
                .unwrap()
                .parse()
                .unwrap(),
            Ordering::Relaxed,
        );
        let descriptor = unsafe { clock_fixture_open_breakpoint(target) };
        if descriptor < 0 {
            return 42;
        }
        WINDOW_FD.store(descriptor as u64, Ordering::Relaxed);
    }
    let _cleanup = Cleanup;
    PID.store(
        unsafe { raw_syscall6(libc::SYS_getpid, [0; 6]) } as u64,
        Ordering::Relaxed,
    );
    TID.store(
        unsafe { raw_syscall6(libc::SYS_gettid, [0; 6]) } as u64,
        Ordering::Relaxed,
    );
    UID.store(
        unsafe { raw_syscall6(libc::SYS_getuid, [0; 6]) } as u64,
        Ordering::Relaxed,
    );
    let mut guest_mask = 0u64;
    if unsafe {
        raw_syscall6(
            libc::SYS_rt_sigprocmask,
            [0, 0, (&raw mut guest_mask) as u64, 8, 0, 0],
        )
    } != 0
    {
        return 42;
    }
    GUEST_MASK.store(guest_mask, Ordering::Relaxed);
    let installed = if SUD.load(Ordering::Relaxed) != 0 {
        let mut signals = vec![reverie_preload::signal::RuntimeSignal {
            signal: libc::SIGUSR2,
            disable_descriptor: None,
            validate: validate_notification,
            body: notification_body,
        }];
        if WINDOW_PC.load(Ordering::Relaxed) != 0 {
            signals.push(reverie_preload::signal::RuntimeSignal {
                signal: libc::SIGTRAP,
                disable_descriptor: Some(WINDOW_FD.load(Ordering::Relaxed) as i32),
                validate: validate_window,
                body: window_body,
            });
        }
        if unsafe {
            reverie_preload::signal::configure_runtime_signals(Box::leak(
                signals.into_boxed_slice(),
            ))
        }
        .is_err()
        {
            return 42;
        }
        unsafe {
            reverie_liteinst::install_tool_with_mode::<ClockTool>(
                std::path::Path::new(&socket),
                reverie_liteinst::SyscallMode::UserDispatchWithoutPatching,
            )
        }
    } else {
        unsafe {
            reverie_liteinst::install_tool_quiescent::<ClockTool>(std::path::Path::new(&socket))
        }
    };
    if FORCE_INSTRUCTION.load(Ordering::Relaxed) != 0 {
        let error = installed.unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::Unsupported);
        assert!(
            error
                .to_string()
                .contains("instruction or vDSO subscriptions")
        );
        return 42;
    }
    if installed.is_err() {
        return 42;
    }
    if SUD.load(Ordering::Relaxed) == 0 && !unsafe { install_notification() } {
        return 42;
    }
    work();
    if WINDOW_PC.load(Ordering::Relaxed) != 0
        && unsafe {
            raw_syscall6(
                libc::SYS_ioctl,
                [WINDOW_FD.load(Ordering::Relaxed), 0x2400, 0, 0, 0, 0],
            )
        } != 0
    {
        return 42;
    }
    1
}

reverie_liteinst::clocked_initializer!(CLOCK_FIXTURE_INIT, initialize);
