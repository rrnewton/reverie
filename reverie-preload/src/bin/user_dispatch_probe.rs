use std::sync::atomic::AtomicBool;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;

use reverie_preload::dispatch::PassthroughDispatcher;
use reverie_preload::dispatch::SyscallDispatcher;
use reverie_preload::dispatch::SyscallEvent;
use reverie_preload::lifecycle::InProcessSeccomp;
use reverie_preload::lifecycle::InProcessUserDispatch;
use reverie_preload::lifecycle::LifecycleController;
use reverie_preload::lifecycle::RuntimeConfig;
use reverie_preload::trap::RuntimeEntryHooks;
use reverie_preload::trap::raw_syscall6;

const PR_SET_SYSCALL_USER_DISPATCH: u64 = 59;
const PR_SYS_DISPATCH_ON: u64 = 1;

static CALLS: AtomicUsize = AtomicUsize::new(0);
static ENTERS: AtomicUsize = AtomicUsize::new(0);
static LEAVES: AtomicUsize = AtomicUsize::new(0);
static START_THREAD: AtomicBool = AtomicBool::new(false);
static SIGNALS: AtomicUsize = AtomicUsize::new(0);
static NESTED_SYSCALLS: AtomicBool = AtomicBool::new(false);
static NESTED_RESULTS: AtomicUsize = AtomicUsize::new(0);
static SIGNAL_DIAGNOSTICS: AtomicBool = AtomicBool::new(false);
static MEDIATED_DELIVERIES: AtomicUsize = AtomicUsize::new(0);

extern "C" fn ordinary_handler(_: i32) {
    diagnostic(b"signal-handler-enter\n");
    diagnostic_stack();
    let mut current_mask = 0u64;
    raw(
        libc::SYS_rt_sigprocmask,
        [0, 0, (&raw mut current_mask) as u64, 8, 0, 0],
    );
    diagnostic(if current_mask & (1u64 << (libc::SIGSYS - 1)) == 0 {
        b"handler-sigsys-open\n"
    } else {
        b"handler-sigsys-blocked\n"
    });
    SIGNALS.fetch_add(1, Ordering::Relaxed);
    if NESTED_SYSCALLS.load(Ordering::Relaxed) {
        let mut mask = 0u64;
        let expected_mask = (1u64 << (libc::SIGUSR1 - 1))
            | (1u64 << (libc::SIGUSR2 - 1))
            | (1u64 << (libc::SIGTERM - 1));
        if application_syscall(libc::SYS_getuid, [0; 6]) == 7654321
            && application_syscall(
                libc::SYS_rt_sigprocmask,
                [0, 0, (&raw mut mask) as u64, 8, 0, 0],
            ) == 0
            && mask == expected_mask
        {
            NESTED_RESULTS.fetch_add(1, Ordering::Relaxed);
        }
    }
    diagnostic(b"signal-handler-leave\n");
}

fn diagnostic(message: &[u8]) {
    raw(
        libc::SYS_write,
        [2, message.as_ptr() as u64, message.len() as u64, 0, 0, 0],
    );
}

fn diagnostic_hex(value: usize) {
    let mut message = *b"0x0000000000000000\n";
    for digit in 0..16 {
        message[17 - digit] = b"0123456789abcdef"[(value >> (digit * 4)) & 15];
    }
    diagnostic(&message);
}

fn diagnostic_stack() {
    let pointer: usize;
    unsafe { core::arch::asm!("mov {}, rsp", out(reg) pointer, options(nomem, nostack)) };
    let mut stack: libc::stack_t = unsafe { std::mem::zeroed() };
    raw(
        libc::SYS_sigaltstack,
        [0, (&raw mut stack) as u64, 0, 0, 0, 0],
    );
    diagnostic_hex(pointer);
    diagnostic_hex(stack.ss_sp as usize);
    diagnostic_hex(stack.ss_size);
    diagnostic_hex(stack.ss_flags as usize);
}

struct SignalDispatcher;

impl SyscallDispatcher for SignalDispatcher {
    fn dispatch(&self, event: &mut SyscallEvent) {
        if event.number() == libc::SYS_tgkill {
            MEDIATED_DELIVERIES.fetch_add(1, Ordering::Relaxed);
        }
        if event.number() == libc::SYS_getuid {
            event.set_result(7654321);
        } else {
            PassthroughDispatcher::new().dispatch(event);
        }
    }
}

fn ordinary_signal(use_sud: bool, intercepted: bool, nested: bool) -> bool {
    SIGNAL_DIAGNOSTICS.store(true, Ordering::Relaxed);
    NESTED_SYSCALLS.store(nested, Ordering::Relaxed);
    unsafe {
        let mut action: libc::sigaction = std::mem::zeroed();
        action.sa_sigaction = ordinary_handler as *const () as usize;
        if libc::sigemptyset(&mut action.sa_mask) != 0 {
            return false;
        }
        if nested {
            let mask = 1u64 << (libc::SIGUSR2 - 1);
            if libc::sigaddset(&mut action.sa_mask, libc::SIGTERM) != 0
                || raw(
                    libc::SYS_rt_sigprocmask,
                    [libc::SIG_BLOCK as u64, (&raw const mask) as u64, 0, 8, 0, 0],
                ) != 0
            {
                return false;
            }
        }
        if libc::sigaction(libc::SIGUSR1, &action, std::ptr::null_mut()) != 0 {
            return false;
        }
        let controller: &dyn LifecycleController = if use_sud {
            &InProcessUserDispatch
        } else {
            &InProcessSeccomp
        };
        if reverie_preload::trap::register_runtime_entry_hooks(&HOOKS).is_err()
            || reverie_preload::install(
                Box::new(SignalDispatcher),
                controller,
                &RuntimeConfig::default(),
            )
            .is_err()
        {
            return false;
        }
        let process = raw(libc::SYS_getpid, [0; 6]);
        let thread = raw(libc::SYS_gettid, [0; 6]);
        for expected in 1..=4 {
            if expected == 3 {
                action.sa_flags = libc::SA_ONSTACK;
                if libc::sigaction(libc::SIGUSR1, &action, std::ptr::null_mut()) != 0 {
                    return false;
                }
            }
            *libc::__errno_location() = libc::EDOM;
            diagnostic(b"signal-delivery-enter\n");
            let result = if intercepted {
                libc::syscall(libc::SYS_tgkill, process, thread, libc::SIGUSR1)
            } else {
                raw(
                    libc::SYS_tgkill,
                    [process as u64, thread as u64, libc::SIGUSR1 as u64, 0, 0, 0],
                )
            };
            diagnostic(b"signal-delivery-returned\n");
            let mut mask = 0u64;
            if result != 0
                || SIGNALS.load(Ordering::Relaxed) != expected
                || MEDIATED_DELIVERIES.load(Ordering::Relaxed)
                    != if intercepted { expected } else { 0 }
                || *libc::__errno_location() != libc::EDOM
                || raw(
                    libc::SYS_rt_sigprocmask,
                    [0, 0, (&raw mut mask) as u64, 8, 0, 0],
                ) != 0
                || mask & ((1u64 << (libc::SIGUSR1 - 1)) | (1u64 << (libc::SIGSYS - 1))) != 0
                || (nested && mask != 1u64 << (libc::SIGUSR2 - 1))
                || (nested && NESTED_RESULTS.load(Ordering::Relaxed) != expected)
                || application_syscall(libc::SYS_getpid, [0; 6]) != process
                || ENTERS.load(Ordering::Relaxed) != LEAVES.load(Ordering::Relaxed)
            {
                return false;
            }
        }
        true
    }
}

fn interrupted_read() -> bool {
    unsafe {
        let mut pipe = [0i32; 2];
        let mut action: libc::sigaction = std::mem::zeroed();
        action.sa_sigaction = ordinary_handler as *const () as usize;
        let mut notification: libc::sigevent = std::mem::zeroed();
        notification.sigev_notify = libc::SIGEV_SIGNAL;
        notification.sigev_signo = libc::SIGUSR1;
        let mut timer = 0i32;
        if libc::pipe(pipe.as_mut_ptr()) != 0
            || libc::sigaction(libc::SIGUSR1, &action, std::ptr::null_mut()) != 0
            || raw(
                libc::SYS_timer_create,
                [
                    libc::CLOCK_MONOTONIC as u64,
                    (&raw mut notification) as u64,
                    (&raw mut timer) as u64,
                    0,
                    0,
                    0,
                ],
            ) != 0
            || reverie_preload::install(
                Box::new(SignalDispatcher),
                &InProcessUserDispatch,
                &RuntimeConfig::default(),
            )
            .is_err()
        {
            return false;
        }
        let interval = libc::timespec {
            tv_sec: 0,
            tv_nsec: 50_000_000,
        };
        let timer_spec = libc::itimerspec {
            it_interval: interval,
            it_value: interval,
        };
        if application_syscall(
            libc::SYS_timer_settime,
            [timer as u64, 0, (&raw const timer_spec) as u64, 0, 0, 0],
        ) != 0
        {
            return false;
        }
        let mut byte = 0u8;
        let result = application_syscall(
            libc::SYS_read,
            [pipe[0] as u64, (&raw mut byte) as u64, 1, 0, 0, 0],
        );
        raw(libc::SYS_timer_delete, [timer as u64, 0, 0, 0, 0, 0]);
        result == -i64::from(libc::EINTR) && SIGNALS.load(Ordering::Relaxed) > 0
    }
}

unsafe extern "C" fn enter() {
    ENTERS.fetch_add(1, Ordering::Relaxed);
    if SIGNAL_DIAGNOSTICS.load(Ordering::Relaxed) {
        diagnostic(b"runtime-enter\n");
        diagnostic_stack();
    }
}

unsafe extern "C" fn leave() {
    LEAVES.fetch_add(1, Ordering::Relaxed);
    if SIGNAL_DIAGNOSTICS.load(Ordering::Relaxed) {
        diagnostic(b"runtime-leave\n");
    }
}

static HOOKS: RuntimeEntryHooks = RuntimeEntryHooks { enter, leave };

fn raw(number: i64, args: [u64; 6]) -> i64 {
    unsafe { raw_syscall6(number, args) }
}

fn application_syscall(number: i64, args: [u64; 6]) -> i64 {
    let mut result = number;
    unsafe {
        core::arch::asm!(
            "syscall",
            inlateout("rax") result,
            in("rdi") args[0], in("rsi") args[1], in("rdx") args[2],
            in("r10") args[3], in("r8") args[4], in("r9") args[5],
            lateout("rcx") _, lateout("r11") _,
            options(nostack),
        );
    }
    result
}

fn finish(success: bool) -> ! {
    if success {
        let text = b"sud-probe-ok\n";
        if raw(
            libc::SYS_write,
            [1, text.as_ptr() as u64, text.len() as u64, 0, 0, 0],
        ) != text.len() as i64
        {
            raw(libc::SYS_exit_group, [2, 0, 0, 0, 0, 0]);
        }
    }
    raw(libc::SYS_exit_group, [u64::from(!success), 0, 0, 0, 0, 0]);
    unreachable!()
}

struct ProbeDispatcher;

impl SyscallDispatcher for ProbeDispatcher {
    fn dispatch(&self, event: &mut SyscallEvent) {
        CALLS.fetch_add(1, Ordering::Relaxed);
        match event.number() {
            libc::SYS_getpid => {
                let native = raw(libc::SYS_getpid, [0; 6]);
                event.set_result(if native > 0 && native != 424242 {
                    424242
                } else {
                    -1
                });
            }
            libc::SYS_getuid => {
                event.set_result(if event.args() == [11, 22, 33, 44, 55, 66] {
                    7654321
                } else {
                    -1
                });
            }
            libc::SYS_getgid => event.fail(libc::EACCES),
            _ => PassthroughDispatcher::new().dispatch(event),
        }
    }
}

fn filter(instructions: &mut [libc::sock_filter]) {
    let mut program = libc::sock_fprog {
        len: instructions.len() as u16,
        filter: instructions.as_mut_ptr(),
    };
    if raw(
        libc::SYS_prctl,
        [libc::PR_SET_NO_NEW_PRIVS as u64, 1, 0, 0, 0, 0],
    ) != 0
        || raw(
            libc::SYS_prctl,
            [
                libc::PR_SET_SECCOMP as u64,
                2,
                (&raw mut program) as u64,
                0,
                0,
                0,
            ],
        ) != 0
    {
        finish(false);
    }
}

fn instruction(code: u16, jump_true: u8, jump_false: u8, value: u32) -> libc::sock_filter {
    libc::sock_filter {
        code,
        jt: jump_true,
        jf: jump_false,
        k: value,
    }
}

fn deny_ptrace() {
    filter(&mut [
        instruction(0x20, 0, 0, 0),
        instruction(0x15, 0, 1, libc::SYS_ptrace as u32),
        instruction(0x06, 0, 0, libc::SECCOMP_RET_KILL_PROCESS),
        instruction(0x06, 0, 0, libc::SECCOMP_RET_ALLOW),
    ]);
}

fn deny_sud_as_unavailable() {
    filter(&mut [
        instruction(0x20, 0, 0, 0),
        instruction(0x15, 0, 3, libc::SYS_prctl as u32),
        instruction(0x20, 0, 0, 16),
        instruction(0x15, 0, 1, PR_SET_SYSCALL_USER_DISPATCH as u32),
        instruction(0x06, 0, 0, libc::SECCOMP_RET_ERRNO | libc::ENOSYS as u32),
        instruction(0x06, 0, 0, libc::SECCOMP_RET_ALLOW),
    ]);
}

fn install() -> std::io::Result<()> {
    unsafe {
        reverie_preload::trap::register_runtime_entry_hooks(&HOOKS)?;
        reverie_preload::install(
            Box::new(ProbeDispatcher),
            &InProcessUserDispatch,
            &RuntimeConfig::default(),
        )
    }
}

fn fork_rearm() -> bool {
    let child = raw(libc::SYS_fork, [0; 6]);
    if child < 0 {
        return false;
    }
    if child == 0 {
        let native = application_syscall(libc::SYS_getpid, [0; 6]);
        let before = CALLS.load(Ordering::Relaxed);
        let enabled = unsafe { InProcessUserDispatch::enable_current_thread() }.is_ok();
        let mediated = application_syscall(libc::SYS_getpid, [0; 6]);
        raw(
            libc::SYS_exit_group,
            [
                u64::from(
                    !(enabled
                        && native != 424242
                        && mediated == 424242
                        && CALLS.load(Ordering::Relaxed) == before + 1),
                ),
                0,
                0,
                0,
                0,
                0,
            ],
        );
        unreachable!();
    }
    let mut status = -1;
    let waited = raw(
        libc::SYS_wait4,
        [child as u64, (&raw mut status) as u64, 0, 0, 0, 0],
    );
    child > 0
        && waited == child
        && status == 0
        && application_syscall(libc::SYS_getpid, [0; 6]) == 424242
}

fn thread_rearm() -> bool {
    if unsafe { InProcessUserDispatch::disable_current_thread() }.is_err() {
        return false;
    }
    let thread = std::thread::spawn(|| {
        while !START_THREAD.load(Ordering::Acquire) {
            std::hint::spin_loop();
        }
        let native = application_syscall(libc::SYS_getpid, [0; 6]);
        let enabled = unsafe { InProcessUserDispatch::enable_current_thread() }.is_ok();
        let mediated = application_syscall(libc::SYS_getpid, [0; 6]);
        let disabled = unsafe { InProcessUserDispatch::disable_current_thread() }.is_ok();
        native != 424242 && enabled && mediated == 424242 && disabled
    });
    let enabled = unsafe { InProcessUserDispatch::enable_current_thread() }.is_ok();
    START_THREAD.store(true, Ordering::Release);
    let mediated = application_syscall(libc::SYS_getpid, [0; 6]);
    let disabled = unsafe { InProcessUserDispatch::disable_current_thread() }.is_ok();
    enabled && mediated == 424242 && disabled && thread.join().unwrap_or(false)
}

fn main() {
    let case = std::env::args().nth(1).expect("one probe case required");
    let limits = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    unsafe {
        libc::setrlimit(libc::RLIMIT_CORE, &limits);
        libc::alarm(8);
    }
    deny_ptrace();
    if case == "ordinary-signal-seccomp" || case == "ordinary-signal-sud" {
        finish(ordinary_signal(case == "ordinary-signal-sud", false, false));
    }
    if case == "mediated-signal-seccomp" || case == "mediated-signal-sud" {
        finish(ordinary_signal(case == "mediated-signal-sud", true, false));
    }
    if case == "nested-signal-sud" {
        finish(ordinary_signal(true, true, true));
    }
    if case == "interrupted-read" {
        finish(interrupted_read());
    }
    if case == "rearm-before-install" {
        finish(
            unsafe { InProcessUserDispatch::enable_current_thread() }
                .is_err_and(|error| error.kind() == std::io::ErrorKind::InvalidInput),
        );
    }
    if case == "blocked-mask" {
        let mask = 1u64 << (libc::SIGSYS - 1);
        if raw(
            libc::SYS_rt_sigprocmask,
            [libc::SIG_BLOCK as u64, (&raw const mask) as u64, 0, 8, 0, 0],
        ) != 0
        {
            finish(false);
        }
        finish(install().is_err_and(|error| error.kind() == std::io::ErrorKind::InvalidInput));
    }
    if case == "unavailable" {
        deny_sud_as_unavailable();
        finish(install().is_err_and(|error| error.raw_os_error() == Some(libc::ENOSYS)));
    }
    if let Err(error) = install() {
        eprintln!("SUD installation failed (not skipped): {error}");
        finish(false);
    }
    let success = match case.as_str() {
        "traps" => {
            let before = CALLS.load(Ordering::Relaxed);
            let first = application_syscall(libc::SYS_getpid, [0; 6]);
            let native = raw(libc::SYS_getpid, [0; 6]);
            let second = application_syscall(libc::SYS_getpid, [0; 6]);
            first == 424242
                && second == 424242
                && native != 424242
                && CALLS.load(Ordering::Relaxed) == before + 2
                && ENTERS.load(Ordering::Relaxed) == LEAVES.load(Ordering::Relaxed)
                && ENTERS.load(Ordering::Relaxed) >= 2
        }
        "six-args" => {
            application_syscall(libc::SYS_getuid, [11, 22, 33, 44, 55, 66]) == 7654321
                && application_syscall(libc::SYS_getgid, [0; 6]) == -i64::from(libc::EACCES)
        }
        "failed-exec" => {
            let before = CALLS.load(Ordering::Relaxed);
            let path = c"/definitely-absent-reverie-sud-probe";
            let args = [path.as_ptr(), std::ptr::null()];
            let environment: [*const libc::c_char; 1] = [std::ptr::null()];
            let result = raw(
                libc::SYS_execve,
                [
                    path.as_ptr() as u64,
                    args.as_ptr() as u64,
                    environment.as_ptr() as u64,
                    0,
                    0,
                    0,
                ],
            );
            result == -i64::from(libc::ENOENT)
                && CALLS.load(Ordering::Relaxed) == before
                && application_syscall(libc::SYS_getpid, [0; 6]) == 424242
                && CALLS.load(Ordering::Relaxed) == before + 1
        }
        "exec-refused" => {
            application_syscall(libc::SYS_execve, [0; 6]) == -i64::from(libc::ENOTSUP)
                && application_syscall(libc::SYS_execveat, [0; 6]) == -i64::from(libc::ENOTSUP)
        }
        "fork-rearm" => fork_rearm(),
        "thread-rearm" => thread_rearm(),
        "disable-rearm" => {
            let disabled = unsafe { InProcessUserDispatch::disable_current_thread() }.is_ok();
            let native = application_syscall(libc::SYS_getpid, [0; 6]);
            let enabled = unsafe { InProcessUserDispatch::enable_current_thread() }.is_ok();
            disabled
                && native != 424242
                && enabled
                && application_syscall(libc::SYS_getpid, [0; 6]) == 424242
        }
        "reconfigure-refused" => {
            application_syscall(
                libc::SYS_prctl,
                [PR_SET_SYSCALL_USER_DISPATCH, 0, 0, 0, 0, 0],
            ) == -i64::from(libc::EPERM)
                && application_syscall(libc::SYS_getpid, [0; 6]) == 424242
        }
        "prctl-forwarded" => {
            let option = libc::PR_GET_DUMPABLE as u64;
            let expected = raw(libc::SYS_prctl, [option, 0, 0, 0, 0, 0]);
            expected >= 0
                && application_syscall(libc::SYS_prctl, [option, 0, 0, 0, 0, 0]) == expected
                && application_syscall(libc::SYS_prctl, [(1u64 << 32) | option, 0, 0, 0, 0, 0])
                    == expected
        }
        "foreign-sigsys" => {
            raw(
                libc::SYS_tgkill,
                [
                    raw(libc::SYS_getpid, [0; 6]) as u64,
                    raw(libc::SYS_gettid, [0; 6]) as u64,
                    libc::SIGSYS as u64,
                    0,
                    0,
                    0,
                ],
            );
            false
        }
        "mask-change-refused" => {
            let mask = 1u64 << (libc::SIGSYS - 1);
            application_syscall(
                libc::SYS_rt_sigprocmask,
                [libc::SIG_BLOCK as u64, (&raw const mask) as u64, 0, 8, 0, 0],
            ) == -i64::from(libc::EPERM)
                && application_syscall(libc::SYS_getpid, [0; 6]) == 424242
        }
        "ptrace-denied" => {
            raw(libc::SYS_ptrace, [0; 6]);
            false
        }
        "bad-selector" | "unreadable-selector" => {
            let invalid = 2u8;
            let selector = if case == "unreadable-selector" {
                raw(
                    libc::SYS_mmap,
                    [
                        0,
                        4096,
                        libc::PROT_NONE as u64,
                        (libc::MAP_PRIVATE | libc::MAP_ANONYMOUS) as u64,
                        u64::MAX,
                        0,
                    ],
                ) as u64
            } else {
                (&raw const invalid) as u64
            };
            let range = InProcessUserDispatch::trusted_range();
            if raw(
                libc::SYS_prctl,
                [
                    PR_SET_SYSCALL_USER_DISPATCH,
                    PR_SYS_DISPATCH_ON,
                    range.start as u64,
                    range.len() as u64,
                    selector,
                    0,
                ],
            ) != 0
            {
                finish(false);
            }
            application_syscall(libc::SYS_getpid, [0; 6]);
            false
        }
        _ => false,
    };
    finish(success);
}
