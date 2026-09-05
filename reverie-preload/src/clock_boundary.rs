//! Scalar ownership transfer at backend-controlled machine boundaries.

use std::io;
use std::sync::atomic::AtomicPtr;
use std::sync::atomic::Ordering;

#[repr(C)]
pub struct BoundaryHooks {
    pub enter: unsafe extern "C" fn(witness: u64) -> u64,
    pub leave: unsafe extern "C" fn(token: u64, kind: u64, witness: u64),
}

/// Scalars only: no signal-frame reference may survive its rt_sigreturn.
#[repr(C)]
pub struct Continuation {
    pub kind: u64,
    pub witness: u64,
}

impl Continuation {
    pub const GUEST: Self = Self {
        kind: 0,
        witness: 0,
    };
    pub const RUNTIME: Self = Self {
        kind: 1,
        witness: 0,
    };

    /// Transfer the paused outer activation to this backend-validated entry.
    pub const fn hook(witness: u64) -> Self {
        Self { kind: 2, witness }
    }
}

unsafe extern "C" fn no_enter(_witness: u64) -> u64 {
    0
}
unsafe extern "C" fn no_leave(_token: u64, _kind: u64, _witness: u64) {}

static DEFAULT: BoundaryHooks = BoundaryHooks {
    enter: no_enter,
    leave: no_leave,
};
static HOOKS: AtomicPtr<BoundaryHooks> = AtomicPtr::new((&raw const DEFAULT).cast_mut());

/// Balanced controls around one original signal-frame activation.
#[repr(C)]
pub struct SignalScope {
    pub enter: unsafe extern "C" fn() -> u64,
    pub leave: unsafe extern "C" fn(token: u64),
}

unsafe extern "C" fn no_signal_enter() -> u64 {
    0
}
unsafe extern "C" fn no_signal_leave(_token: u64) {}

static DEFAULT_SIGNAL_SCOPE: SignalScope = SignalScope {
    enter: no_signal_enter,
    leave: no_signal_leave,
};
static SIGNAL_SCOPE: AtomicPtr<SignalScope> =
    AtomicPtr::new((&raw const DEFAULT_SIGNAL_SCOPE).cast_mut());

/// Install a process-lifetime scope before enabling interception or sources.
///
/// # Safety
/// Both functions are signal-safe, integer-only, balanced, and use no masks,
/// allocation, locks, lazy TLS or unwinding. Each nested scope must restore its
/// exact inherited controls. No scope may run guest code or persist a control
/// change. The returned token belongs solely to this activation's stack frame.
/// Registration does not establish source, stack or instruction admission.
pub unsafe fn register_signal_scope(scope: &'static SignalScope) -> io::Result<()> {
    let pointer = (scope as *const SignalScope).cast_mut();
    match SIGNAL_SCOPE.compare_exchange(
        (&raw const DEFAULT_SIGNAL_SCOPE).cast_mut(),
        pointer,
        Ordering::AcqRel,
        Ordering::Acquire,
    ) {
        Ok(_) => Ok(()),
        Err(current) if current == pointer => Ok(()),
        Err(_) => Err(io::Error::other("signal scope already registered")),
    }
}

/// Register immutable process-lifetime hooks before interception.
///
/// # Safety
/// Hooks preserve the SysV ABI, use no SIMD state, never unwind and require no
/// allocation, locks or lazy TLS. Entry stops counting before conditional runtime
/// work; leave's enabled suffix must be branchless and reentrant. Callers retain
/// each exact activation token and classify the continuation before leaving.
pub unsafe fn register(hooks: &'static BoundaryHooks) -> io::Result<()> {
    let pointer = (hooks as *const BoundaryHooks).cast_mut();
    match HOOKS.compare_exchange(
        (&raw const DEFAULT).cast_mut(),
        pointer,
        Ordering::AcqRel,
        Ordering::Acquire,
    ) {
        Ok(_) => Ok(()),
        Err(current) if current == pointer => Ok(()),
        Err(_) => Err(io::Error::other("clock boundary hooks already registered")),
    }
}

#[doc(hidden)]
/// # Safety
/// Enter only through `clocked_signal!`, with a body returning `Continuation`
/// and the original kernel signal-handler argument and return-stack ABI.
#[unsafe(naked)]
pub unsafe extern "C" fn invoke_signal() {
    core::arch::naked_asm!(
        "push r12", "push r13", "push r14", "sub rsp, 64",
        "mov [rsp], rdi", "mov [rsp + 8], rsi", "mov [rsp + 16], rdx",
        "mov [rsp + 24], rax", "mov r13, [rip + {hooks}]",
        "xor edi, edi", "call qword ptr [r13]", "mov r12, rax",
        "mov r14, [rip + {scope}]", "call qword ptr [r14]", "mov [rsp + 32], rax",
        "mov rdi, [rsp]", "mov rsi, [rsp + 8]", "mov rdx, [rsp + 16]",
        "mov rcx, [rsp + 32]", "call qword ptr [rsp + 24]",
        "mov [rsp + 40], rax", "mov [rsp + 48], rdx",
        "mov rdi, [rsp + 32]", "call qword ptr [r14 + 8]",
        "mov rdi, r12", "mov rsi, [rsp + 40]", "mov rdx, [rsp + 48]",
        "call qword ptr [r13 + 8]",
        "add rsp, 64", "pop r14", "pop r13", "pop r12", "ret",
        hooks = sym HOOKS,
        scope = sym SIGNAL_SCOPE,
    );
}

/// Wrap a signal body returning `Continuation` with scalar clock ownership.
/// The live kernel frame is borrowed by the body only. Return preserves the
/// original restorer stack position; no new signal frame is manufactured.
#[macro_export]
macro_rules! clocked_signal {
    ($name:ident, $body:path, scope_token) => {
        const _: unsafe extern "C" fn(i32, *mut libc::siginfo_t, *mut libc::c_void, u64)
            -> $crate::clock_boundary::Continuation = $body;
        #[unsafe(naked)]
        pub(crate) unsafe extern "C" fn $name(
            signal: i32, info: *mut libc::siginfo_t, context: *mut libc::c_void,
        ) {
            core::arch::naked_asm!(
                "lea rax, [rip + {body}]", "jmp {invoke}",
                body = sym $body, invoke = sym $crate::clock_boundary::invoke_signal,
            );
        }
    };
    ($name:ident, $body:path) => {
        const _: unsafe extern "C" fn(i32, *mut libc::siginfo_t, *mut libc::c_void)
            -> $crate::clock_boundary::Continuation = $body;
        #[unsafe(naked)]
        pub(crate) unsafe extern "C" fn $name(
            signal: i32, info: *mut libc::siginfo_t, context: *mut libc::c_void,
        ) {
            core::arch::naked_asm!(
                "lea rax, [rip + {body}]", "jmp {invoke}",
                body = sym $body, invoke = sym $crate::clock_boundary::invoke_signal,
            );
        }
    };
}
