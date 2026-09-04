use core::sync::atomic::AtomicI64;
use core::sync::atomic::AtomicU64;
use core::sync::atomic::Ordering;
use std::io;
use std::sync::OnceLock;

use crate::clock_boundary::Continuation;
use crate::trap;
use crate::user_dispatch::syscall_result;

/// A trusted, process-lifetime runtime signal entry, not a guest handler.
pub struct RuntimeSignal {
    pub signal: i32,
    pub disable_descriptor: Option<i32>,
    pub validate: unsafe extern "C" fn(i32, *mut libc::siginfo_t, *mut libc::c_void) -> bool,
    pub body: unsafe extern "C" fn(i32, *mut libc::siginfo_t, *mut libc::c_void) -> Continuation,
}

static SIGNALS: OnceLock<&'static [RuntimeSignal]> = OnceLock::new();
static MASK: AtomicU64 = AtomicU64::new(0);
static OWNER: AtomicI64 = AtomicI64::new(0);
static DESCRIPTORS: [AtomicI64; 65] = [const { AtomicI64::new(-1) }; 65];

/// Configure explicit closed-world runtime sources before SUD installation.
/// This does not install actions, unblock signals, or enable any source.
///
/// # Safety
/// Call once, before interception or additional threads. Sources must remain
/// disabled until installation finishes. Descriptor ownership, code and scalar
/// source state must remain live for the process lifetime. Validators must
/// establish source identity (not just signal number), reject unknown sources,
/// and consume the expected activation once. Bodies and validators must not
/// allocate, lock, unwind, call Tool/RPC/logging, or retain frame references.
/// Bodies classify the actual interrupted PC; count equality is not a witness.
/// This restricted registration is not arbitrary guest signal virtualization.
pub unsafe fn configure_runtime_signals(signals: &'static [RuntimeSignal]) -> io::Result<()> {
    if super::owned_trace::configured() {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "owned trace source configured",
        ));
    }
    let mut mask = 0u64;
    for action in signals {
        if !matches!(action.signal, libc::SIGTRAP | libc::SIGUSR2)
            || mask & (1u64 << (action.signal - 1)) != 0
            || action
                .disable_descriptor
                .is_some_and(|descriptor| descriptor < 0)
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid runtime source",
            ));
        }
        mask |= 1u64 << (action.signal - 1);
    }
    let mut guest_mask = 0u64;
    syscall_result(unsafe {
        trap::raw_syscall6(
            libc::SYS_rt_sigprocmask,
            [0, 0, (&raw mut guest_mask) as u64, 8, 0, 0],
        )
    })?;
    if guest_mask & (mask | (1u64 << (libc::SIGSYS - 1))) != 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "guest blocks a required runtime source",
        ));
    }
    for source in signals {
        let mut action = super::KernelSignalAction::default();
        syscall_result(unsafe {
            trap::raw_syscall6(
                libc::SYS_rt_sigaction,
                [source.signal as u64, 0, (&raw mut action) as u64, 8, 0, 0],
            )
        })?;
        if action.handler != libc::SIG_DFL {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "runtime signal disposition conflict",
            ));
        }
    }
    SIGNALS.set(signals).map_err(|_| {
        io::Error::new(
            io::ErrorKind::AlreadyExists,
            "runtime sources already configured",
        )
    })?;
    OWNER.store(
        unsafe { trap::raw_syscall6(libc::SYS_gettid, [0; 6]) },
        Ordering::Relaxed,
    );
    for action in signals {
        DESCRIPTORS[action.signal as usize].store(
            i64::from(action.disable_descriptor.unwrap_or(-1)),
            Ordering::Relaxed,
        );
    }
    MASK.store(mask, Ordering::Release);
    Ok(())
}

pub fn runtime_signals_configured() -> bool {
    SIGNALS.get().is_some()
}

pub fn runtime_signal_mask() -> u64 {
    MASK.load(Ordering::Acquire)
}

pub fn runtime_handler_mask() -> u64 {
    !runtime_signal_mask()
}

pub fn runtime_ordinary_mask() -> u64 {
    runtime_handler_mask() & !(1u64 << (libc::SIGSYS - 1))
}

pub fn runtime_signal_descriptors() -> [i32; 2] {
    [
        DESCRIPTORS[libc::SIGTRAP as usize].load(Ordering::Relaxed) as i32,
        DESCRIPTORS[libc::SIGUSR2 as usize].load(Ordering::Relaxed) as i32,
    ]
}

pub(super) unsafe fn install_runtime_signals() -> io::Result<()> {
    for source in SIGNALS.get().copied().unwrap_or(&[]) {
        let action = super::KernelSignalAction {
            handler: runtime_signal_entry as *const () as usize,
            flags: libc::SA_SIGINFO as u64 | 0x04000000,
            restorer: trap::trusted_sigreturn_restorer as *const () as usize,
            mask: runtime_ordinary_mask(),
        };
        syscall_result(unsafe {
            trap::raw_syscall6(
                libc::SYS_rt_sigaction,
                [source.signal as u64, (&raw const action) as u64, 0, 8, 0, 0],
            )
        })?;
        let mut installed = super::KernelSignalAction::default();
        syscall_result(unsafe {
            trap::raw_syscall6(
                libc::SYS_rt_sigaction,
                [
                    source.signal as u64,
                    0,
                    (&raw mut installed) as u64,
                    8,
                    0,
                    0,
                ],
            )
        })?;
        if installed.handler != action.handler
            || installed.flags != action.flags
            || installed.restorer != action.restorer
            || installed.mask
                != action.mask & !((1u64 << (libc::SIGKILL - 1)) | (1u64 << (libc::SIGSTOP - 1)))
        {
            return Err(io::Error::other("runtime action readback mismatch"));
        }
    }
    Ok(())
}

unsafe extern "C" fn runtime_signal_body(
    signal: i32,
    info: *mut libc::siginfo_t,
    frame: *mut libc::c_void,
) -> Continuation {
    if info.is_null() || frame.is_null() {
        unsafe { trap::terminal126("runtime-signal/frame", "predicate", None) };
    }
    let tid = unsafe { trap::raw_syscall6(libc::SYS_gettid, [0; 6]) };
    if tid != OWNER.load(Ordering::Relaxed) {
        unsafe { trap::terminal126("runtime-signal/owner", "raw-gettid", Some(tid)) };
    }
    let Some(source) = SIGNALS
        .get()
        .and_then(|sources| sources.iter().find(|source| source.signal == signal))
    else {
        unsafe { trap::terminal126("runtime-signal/source", "signal", Some(i64::from(signal))) };
    };
    if !unsafe { (source.validate)(signal, info, frame) } {
        unsafe { trap::terminal126("runtime-signal/validate", "signal", Some(i64::from(signal))) };
    }
    unsafe { (source.body)(signal, info, frame) }
}

crate::clocked_signal!(runtime_signal_clocked, runtime_signal_body);

unsafe extern "C" fn runtime_disable_failed(result: i64) -> ! {
    unsafe {
        trap::terminal126(
            "runtime-signal/PERF_EVENT_IOC_DISABLE",
            "raw-result",
            Some(result),
        )
    }
}

#[unsafe(naked)]
unsafe extern "C" fn runtime_signal_entry(
    signal: i32,
    info: *mut libc::siginfo_t,
    frame: *mut libc::c_void,
) {
    core::arch::naked_asm!(
        "push rdi", "push rsi", "push rdx", "push r12",
        "lea rax, [rip + {descriptors}]", "mov r12, [rax + rdi * 8]",
        "mov eax, 16", "mov rdi, r12", "mov esi, 0x2401", "xor edx, edx",
        "call reverie_preload_trusted_syscall_ip",
        "xor ecx, ecx", "mov rdx, -9", "cmp r12, -1", "cmove rcx, rdx",
        "lea rdx, [rip + 2f]", "lea r12, [rip + 3f]",
        "cmp rax, rcx", "cmovne rdx, r12", "jmp rdx",
        "2:", "pop r12", "pop rdx", "pop rsi", "pop rdi", "jmp {clocked}",
        "3:", "mov rdi, rax", "sub rsp, 8", "call {failed}", "ud2",
        descriptors = sym DESCRIPTORS, clocked = sym runtime_signal_clocked,
        failed = sym runtime_disable_failed,
    );
}
