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

/// The outer context of one authentic signal activation, outside the clock hooks.
#[repr(C)]
pub struct ExecutionContext {
    pub enter: unsafe extern "C" fn() -> u64,
    pub leave: unsafe extern "C" fn(token: u64, kind: u64),
    pub finish_deferred: unsafe extern "C" fn(),
}

const _: () = assert!(std::mem::size_of::<ExecutionContext>() == 24);

unsafe extern "C" fn no_context_enter() -> u64 {
    0
}
unsafe extern "C" fn no_context_leave(_token: u64, _kind: u64) {}
unsafe extern "C" fn no_context_finish_deferred() {}

static DEFAULT_CONTEXT: ExecutionContext = ExecutionContext {
    enter: no_context_enter,
    leave: no_context_leave,
    finish_deferred: no_context_finish_deferred,
};
static CONTEXT: AtomicPtr<ExecutionContext> =
    AtomicPtr::new((&raw const DEFAULT_CONTEXT).cast_mut());

/// Register context establishment before the earliest clock or runtime TLS access.
///
/// # Safety
/// Install before interception or asynchronous sources. Both functions preserve
/// the SysV ABI, use integer registers only, and cannot unwind, allocate, log or
/// require the interrupted thread's TLS. Entry establishes the actual runtime
/// context; leave runs after the last clock/TLS use. Code before clock entry and
/// after clock exit must add no conditional branches. The token belongs only to
/// this original activation. For a deferred runtime continuation, its real owner
/// must retain the guest context and restore it at the final owned return through
/// `finish_deferred`, which runs after the last clock/TLS use and before the
/// original owned signal return. It has the same branchless, integer-only
/// requirements as `leave` and must validate the real retained activation. This
/// registration alone neither supplies nor validates that continuation owner.
pub unsafe fn register_execution_context(context: &'static ExecutionContext) -> io::Result<()> {
    let pointer = (context as *const ExecutionContext).cast_mut();
    match CONTEXT.compare_exchange(
        (&raw const DEFAULT_CONTEXT).cast_mut(),
        pointer,
        Ordering::AcqRel,
        Ordering::Acquire,
    ) {
        Ok(_) => Ok(()),
        Err(current) if current == pointer => Ok(()),
        Err(_) => Err(io::Error::other("execution context already registered")),
    }
}

/// Restore the context retained by this exact owned deferred activation.
///
/// # Safety
/// Call only from the final owned return after its last clock/TLS access. The
/// continuation owner must be the one that retained the context in `leave`.
/// Neither Rust nor other TLS-dependent code may run after this function.
#[unsafe(naked)]
pub unsafe extern "C" fn finish_deferred_context() {
    core::arch::naked_asm!(
        "mov rax, [rip + {context}]", "jmp qword ptr [rax + 16]",
        context = sym CONTEXT,
    );
}

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
        "push r12", "push r13", "push r14", "push r15", "sub rsp, 72",
        "mov [rsp], rdi", "mov [rsp + 8], rsi", "mov [rsp + 16], rdx",
        "mov [rsp + 24], rax", "mov r15, [rip + {context}]",
        "call qword ptr [r15]", "mov [rsp + 56], rax",
        "mov r13, [rip + {hooks}]",
        "xor edi, edi", "call qword ptr [r13]", "mov r12, rax",
        "mov r14, [rip + {scope}]", "call qword ptr [r14]", "mov [rsp + 32], rax",
        "mov rdi, [rsp]", "mov rsi, [rsp + 8]", "mov rdx, [rsp + 16]",
        "mov rcx, [rsp + 32]", "lea r8, [rsp + 104]", "call qword ptr [rsp + 24]",
        "mov [rsp + 40], rax", "mov [rsp + 48], rdx",
        "mov rdi, [rsp + 32]", "call qword ptr [r14 + 8]",
        "mov rdi, r12", "mov rsi, [rsp + 40]", "mov rdx, [rsp + 48]",
        "call qword ptr [r13 + 8]",
        "mov rdi, [rsp + 56]", "mov rsi, [rsp + 40]", "call qword ptr [r15 + 8]",
        "add rsp, 72", "pop r15", "pop r14", "pop r13", "pop r12", "ret",
        hooks = sym HOOKS,
        scope = sym SIGNAL_SCOPE,
        context = sym CONTEXT,
    );
}

/// Wrap a signal body returning `Continuation` with scalar clock ownership.
/// The live kernel frame is borrowed by the body only. Return preserves the
/// original restorer stack position; no new signal frame is manufactured.
#[macro_export]
macro_rules! clocked_signal {
    ($name:ident, $body:path, frame_entry) => {
        const _: unsafe extern "C" fn(i32, *mut libc::siginfo_t, *mut libc::c_void, u64, usize)
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

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicUsize;

    use super::*;

    static EXPECTED_SP: AtomicUsize = AtomicUsize::new(0);
    static BODY: AtomicUsize = AtomicUsize::new(0);
    static BALANCE: AtomicUsize = AtomicUsize::new(0);

    unsafe extern "C" fn enter(witness: u64) -> u64 {
        assert_eq!(witness, 0);
        assert_eq!(BALANCE.fetch_add(1, Ordering::Relaxed), 0);
        0x17
    }
    unsafe extern "C" fn leave(token: u64, kind: u64, witness: u64) {
        assert_eq!(
            (token, kind, witness),
            (0x17, 2, BODY.load(Ordering::Relaxed) as u64)
        );
        assert_eq!(BALANCE.fetch_sub(1, Ordering::Relaxed), 1);
    }
    unsafe extern "C" fn scope_enter() -> u64 {
        assert_eq!(BALANCE.fetch_add(1, Ordering::Relaxed), 1);
        0x31
    }
    unsafe extern "C" fn scope_leave(token: u64) {
        assert_eq!(token, 0x31);
        assert_eq!(BALANCE.fetch_sub(1, Ordering::Relaxed), 2);
    }
    static CLOCK: BoundaryHooks = BoundaryHooks { enter, leave };
    static SCOPE: SignalScope = SignalScope {
        enter: scope_enter,
        leave: scope_leave,
    };

    unsafe extern "C" fn body_three(
        signal: i32,
        info: *mut libc::siginfo_t,
        context: *mut libc::c_void,
    ) -> Continuation {
        assert_eq!((signal, info as usize, context as usize), (19, 23, 29));
        assert_eq!(BALANCE.load(Ordering::Relaxed), 2);
        Continuation::hook(BODY.load(Ordering::Relaxed) as u64)
    }
    unsafe extern "C" fn body_four(
        signal: i32,
        info: *mut libc::siginfo_t,
        context: *mut libc::c_void,
        token: u64,
    ) -> Continuation {
        assert_eq!(token, 0x31);
        unsafe { body_three(signal, info, context) }
    }
    unsafe extern "C" fn body_five(
        signal: i32,
        info: *mut libc::siginfo_t,
        context: *mut libc::c_void,
        token: u64,
        entry: usize,
    ) -> Continuation {
        assert_eq!(entry, EXPECTED_SP.load(Ordering::Relaxed));
        assert_eq!(entry & 15, 8);
        unsafe { body_four(signal, info, context, token) }
    }
    crate::clocked_signal!(three, body_three);
    crate::clocked_signal!(four, body_four, scope_token);
    crate::clocked_signal!(five, body_five, frame_entry);

    #[unsafe(naked)]
    unsafe extern "C" fn call_body(
        body: unsafe extern "C" fn(i32, *mut libc::siginfo_t, *mut libc::c_void),
    ) -> u64 {
        core::arch::naked_asm!(
            "push r12", "push r13", "push r14", "sub rsp, 16", "mov [rsp], rsp",
            "mov rax, rdi", "lea r8, [rsp - 8]", "mov [rip + {expected}], r8",
            "mov r12, 37", "mov r13, 41", "mov r14, 43",
            "mov edi, 19", "mov esi, 23", "mov edx, 29", "call rax",
            "xor eax, eax", "cmp rsp, [rsp]", "jne 2f", "cmp r12, 37", "jne 2f",
            "cmp r13, 41", "jne 2f", "cmp r14, 43", "jne 2f", "mov eax, 1",
            "2:", "add rsp, 16", "pop r14", "pop r13", "pop r12", "ret",
            expected = sym EXPECTED_SP,
        );
    }

    #[test]
    fn original_entry_three_four_five_argument_abis() {
        const CHILD: &str = "REVERIE_BOUNDARY_ABI_CHILD";
        if std::env::var_os(CHILD).is_none() {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "clock_boundary::tests::original_entry_three_four_five_argument_abis",
                    "--nocapture",
                ])
                .env(CHILD, "1")
                .output()
                .unwrap();
            assert!(output.status.success(), "{output:?}");
            return;
        }
        unsafe {
            register(&CLOCK).unwrap();
            register_signal_scope(&SCOPE).unwrap();
        }
        for (index, body) in [three, four, five].into_iter().enumerate() {
            BODY.store(index + 3, Ordering::Relaxed);
            assert_eq!(unsafe { call_body(body) }, 1);
            assert_eq!(BALANCE.load(Ordering::Relaxed), 0);
            unsafe { finish_deferred_context() };
            assert_eq!(FINISHED.load(Ordering::Relaxed), 0);
        }
    }

    static CONTEXT_BALANCE: AtomicUsize = AtomicUsize::new(0);

    unsafe extern "C" fn context_enter() -> u64 {
        assert_eq!(BALANCE.load(Ordering::Relaxed), 0);
        assert_eq!(CONTEXT_BALANCE.swap(1, Ordering::Relaxed), 0);
        0x67
    }

    unsafe extern "C" fn context_leave(token: u64, kind: u64) {
        assert_eq!((token, kind), (0x67, 2));
        assert_eq!(BALANCE.load(Ordering::Relaxed), 0);
        assert_eq!(CONTEXT_BALANCE.swap(0, Ordering::Relaxed), 1);
    }

    unsafe extern "C" fn context_clock_enter(witness: u64) -> u64 {
        assert_eq!(CONTEXT_BALANCE.load(Ordering::Relaxed), 1);
        unsafe { enter(witness) }
    }

    unsafe extern "C" fn context_clock_leave(token: u64, kind: u64, witness: u64) {
        assert_eq!(CONTEXT_BALANCE.load(Ordering::Relaxed), 1);
        unsafe { leave(token, kind, witness) }
    }

    unsafe extern "C" fn context_finish_deferred() {
        assert_eq!(BALANCE.load(Ordering::Relaxed), 0);
        assert_eq!(CONTEXT_BALANCE.load(Ordering::Relaxed), 0);
        FINISHED.fetch_add(1, Ordering::Relaxed);
    }

    static FINISHED: AtomicUsize = AtomicUsize::new(0);

    static OUTER_CONTEXT: ExecutionContext = ExecutionContext {
        enter: context_enter,
        leave: context_leave,
        finish_deferred: context_finish_deferred,
    };
    static CONTEXT_CLOCK: BoundaryHooks = BoundaryHooks {
        enter: context_clock_enter,
        leave: context_clock_leave,
    };

    #[test]
    fn context_surrounds_clock_and_preserves_original_signal_entry() {
        const CHILD: &str = "REVERIE_EXECUTION_CONTEXT_ABI_CHILD";
        if std::env::var_os(CHILD).is_none() {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "clock_boundary::tests::context_surrounds_clock_and_preserves_original_signal_entry",
                    "--nocapture",
                ])
                .env(CHILD, "1")
                .output()
                .unwrap();
            assert!(output.status.success(), "{output:?}");
            return;
        }
        unsafe {
            register_execution_context(&OUTER_CONTEXT).unwrap();
            register(&CONTEXT_CLOCK).unwrap();
            register_signal_scope(&SCOPE).unwrap();
        }
        for (index, body) in [three, four, five].into_iter().enumerate() {
            BODY.store(index + 3, Ordering::Relaxed);
            assert_eq!(unsafe { call_body(body) }, 1);
            assert_eq!(BALANCE.load(Ordering::Relaxed), 0);
            assert_eq!(CONTEXT_BALANCE.load(Ordering::Relaxed), 0);
            unsafe { finish_deferred_context() };
            assert_eq!(FINISHED.load(Ordering::Relaxed), index + 1);
        }
    }
}
