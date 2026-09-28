//! The initial Tool handoff and the final return from runtime setup.
//!
//! Preparation APIs are valid only inside this assembly-owned activation. The
//! setup body, its Result handling and destructors, the root Tool lifecycle,
//! finish while the imported event is disabled and inherited asynchronous
//! handlers remain blocked. Only the assembly return below may enable the event,
//! publish its logical release, and restore the assembly-owned mask. An installed
//! runtime may deliberately clear only its reserved SIGSYS/SIGSEGV bits.

use core::arch::global_asm;
use core::cell::Cell;
use core::ffi::c_void;
use core::mem::ManuallyDrop;
use core::sync::atomic::AtomicBool;
use core::sync::atomic::Ordering;
use std::io;

use liteinst2::trampoline::HookContext;
use reverie_preload::trap::raw_syscall6;

use crate::rcb;
use crate::runtime::ToolCallbackGuard;

const EMPTY: u8 = 0;
const PREPARING: u8 = 1;
const PREPARED: u8 = 2;
const LIFECYCLE: u8 = 3;
static INSTALL_STARTED: AtomicBool = AtomicBool::new(false);

const ROOT_RUNTIME_SIGNALS: u64 =
    (1_u64 << (libc::SIGSYS - 1)) | (1_u64 << (libc::SIGSEGV - 1));

#[repr(C)]
struct RootMasks {
    original: u64,
    restore: u64,
}

struct Frame {
    owner: i32,
    phase: u8,
    acquired: bool,
    pause: Option<rcb::PauseToken>,
    masks: *mut RootMasks,
    restore_clear: u64,
    callback: Option<ToolCallbackGuard>,
}

thread_local! {
    static FRAME: Cell<*mut Frame> = const { Cell::new(core::ptr::null_mut()) };
}

fn tid() -> io::Result<i32> {
    let result = unsafe { raw_syscall6(libc::SYS_gettid, [0; 6]) };
    i32::try_from(result)
        .ok()
        .filter(|value| *value > 0)
        .ok_or_else(|| io::Error::from_raw_os_error(libc::ESRCH))
}

/// Refuse an unwrapped or repeated installation before its first global effect.
pub(crate) fn begin_install() -> io::Result<()> {
    let pointer = FRAME.get();
    if pointer.is_null() {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "LiteInst Tool installation requires an outer root activation entry",
        ));
    }
    let owner = tid()?;
    let frame = unsafe { &mut *pointer };
    if frame.phase != EMPTY || frame.owner != 0 {
        return Err(io::Error::from_raw_os_error(libc::EALREADY));
    }
    INSTALL_STARTED.compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .map_err(|_| io::Error::from_raw_os_error(libc::EALREADY))?;
    frame.owner = owner;
    frame.phase = PREPARING;
    Ok(())
}

/// Transfer the acquired root pause; it cannot belong to a signal or callback.
pub(crate) fn acquired(pause: Option<rcb::PauseToken>) -> io::Result<()> {
    let pointer = FRAME.get();
    if pointer.is_null() {
        return Err(io::Error::from_raw_os_error(libc::ESTALE));
    }
    let owner = tid()?;
    let frame = unsafe { &mut *pointer };
    if frame.phase != PREPARING || frame.owner != owner || frame.acquired {
        return Err(io::Error::from_raw_os_error(libc::ESTALE));
    }
    frame.pause = pause;
    frame.acquired = true;
    Ok(())
}

/// Deliberately make an installed runtime signal deliverable on final restore.
/// The assembly frame remains the sole owner of the captured and restore masks.
pub(crate) fn unblock_on_restore(mask: u64) -> io::Result<()> {
    if mask & !ROOT_RUNTIME_SIGNALS != 0 {
        return Err(io::Error::from_raw_os_error(libc::EINVAL));
    }
    let pointer = FRAME.get();
    if pointer.is_null() {
        return Err(io::Error::from_raw_os_error(libc::ESTALE));
    }
    let owner = tid()?;
    let frame = unsafe { &mut *pointer };
    if frame.phase != PREPARING || frame.owner != owner || frame.masks.is_null() {
        return Err(io::Error::from_raw_os_error(libc::ESTALE));
    }
    let masks = unsafe { &mut *frame.masks };
    if masks.restore != (masks.original & !frame.restore_clear) {
        return Err(io::Error::from_raw_os_error(libc::ESTALE));
    }
    frame.restore_clear |= mask;
    masks.restore = masks.original & !frame.restore_clear;
    Ok(())
}

/// Publish that setup is complete while assembly still owns the restore mask.
pub(crate) fn prepared() -> io::Result<()> {
    let pointer = FRAME.get();
    if pointer.is_null() {
        return Err(io::Error::from_raw_os_error(libc::ESTALE));
    }
    let owner = tid()?;
    let frame = unsafe { &mut *pointer };
    if frame.phase != PREPARING
        || frame.owner != owner
        || !frame.acquired
        || !restore_mask_valid(frame)
    {
        return Err(io::Error::from_raw_os_error(libc::ESTALE));
    }
    frame.phase = PREPARED;
    Ok(())
}

fn restore_mask_valid(frame: &Frame) -> bool {
    !frame.masks.is_null()
        && frame.restore_clear & !ROOT_RUNTIME_SIGNALS == 0
        && unsafe { (*frame.masks).restore == (*frame.masks).original & !frame.restore_clear }
}

pub(crate) fn lifecycle_active() -> bool {
    let pointer = FRAME.get();
    !pointer.is_null() && unsafe { (*pointer).phase == LIFECYCLE }
}

#[cfg(feature = "rcb-qualification")]
static ROOT_ENTRY_SIGNAL_PROBE: core::sync::atomic::AtomicI32 =
    core::sync::atomic::AtomicI32::new(0);

#[cfg(feature = "rcb-qualification")]
pub fn arm_entry_signal_probe(signal: i32) -> io::Result<()> {
    if !(1..=64).contains(&signal)
        || matches!(signal, libc::SIGKILL | libc::SIGSTOP | libc::SIGSYS | libc::SIGSEGV)
    {
        return Err(io::Error::from_raw_os_error(libc::EINVAL));
    }
    ROOT_ENTRY_SIGNAL_PROBE
        .compare_exchange(0, signal, Ordering::AcqRel, Ordering::Acquire)
        .map(|_| ())
        .map_err(|_| io::Error::from_raw_os_error(libc::EALREADY))
}

#[cfg(feature = "rcb-qualification")]
fn fire_entry_signal_probe() {
    let signal = ROOT_ENTRY_SIGNAL_PROBE.swap(0, Ordering::AcqRel);
    if signal == 0 {
        return;
    }
    let pid = unsafe { raw_syscall6(libc::SYS_getpid, [0; 6]) };
    let tid = unsafe { raw_syscall6(libc::SYS_gettid, [0; 6]) };
    let result = if pid > 0 && tid > 0 {
        unsafe {
            raw_syscall6(
                libc::SYS_tgkill,
                [pid as u64, tid as u64, signal as u64, 0, 0, 0],
            )
        }
    } else {
        -i64::from(libc::ESRCH)
    };
    if result != 0 {
        unsafe { raw_syscall6(libc::SYS_exit_group, [126, 0, 0, 0, 0, 0]) };
        loop {
            core::hint::spin_loop();
        }
    }
}

unsafe extern "C" fn begin(masks: *mut RootMasks) {
    #[cfg(feature = "rcb-qualification")]
    fire_entry_signal_probe();
    if !FRAME.get().is_null() || INSTALL_STARTED.load(Ordering::Acquire) {
        unsafe { failed(-i64::from(libc::EALREADY)) }
    }
    // Binding rejects a different pre-existing identity and terminates through
    // the allocation-free bridge failure path; an unbound caller must not try
    // to mutate the private record while reporting that failure.
    rcb::bind();
    // Binding changes only the private bridge identity. A constructor that
    // selects another backend acquires no counter; assembly restores its exact
    // incoming signal mask. The actual LiteInst install binds the RCB owner.
    let frame = Box::new(Frame {
        owner: 0,
        phase: EMPTY,
        acquired: false,
        pause: None,
        masks,
        restore_clear: 0,
        callback: Some(ToolCallbackGuard::enter()),
    });
    FRAME.set(Box::into_raw(frame));
}

unsafe extern "C" fn finish(context: *mut HookContext) -> i64 {
    match finish_inner(context) {
        Ok(None) => 0,
        Ok(Some(action)) => {
            // Zero is reserved for a constructor that did not select LiteInst.
            // One is an installed runtime with a genuinely unavailable event;
            // larger values encode fd+2 for an event requiring physical enable.
            action + 1
        }
        Err(error) => -i64::from(error.raw_os_error().unwrap_or(libc::EIO)),
    }
}

fn finish_inner(context: *mut HookContext) -> io::Result<Option<i64>> {
    let pointer = FRAME.get();
    if pointer.is_null() || context.is_null() {
        return Err(io::Error::from_raw_os_error(libc::ESTALE));
    }
    let phase = unsafe { (*pointer).phase };
    if phase == EMPTY {
        if !restore_mask_valid(unsafe { &*pointer }) {
            return Err(io::Error::from_raw_os_error(libc::ESTALE));
        }
        FRAME.set(core::ptr::null_mut());
        drop(unsafe { Box::from_raw(pointer) });
        return Ok(None);
    }
    if phase != PREPARED || unsafe { (*pointer).owner } != tid()? {
        return Err(io::Error::from_raw_os_error(libc::ESTALE));
    }
    unsafe { (*pointer).phase = LIFECYCLE };
    let continuation = unsafe { ((*context).instruction_pointer, (*context).stack_pointer) };
    // No Frame reference crosses Tool code, nested signals or a counter read.
    // Both lifecycle callbacks run with the real captured continuation and the
    // root pause still held. An available counter must be exactly zero.
    crate::runtime::verify_initial_rcb_clock()?;
    crate::tool_host::start_root(unsafe { &mut *context })?;
    if unsafe { ((*context).instruction_pointer, (*context).stack_pointer) } != continuation {
        return Err(io::Error::from_raw_os_error(libc::EOPNOTSUPP));
    }
    crate::runtime::verify_initial_rcb_clock()?;

    let mut frame = unsafe { Box::from_raw(pointer) };
    let pause = frame.pause.take();
    let callback = frame.callback.take();
    if !restore_mask_valid(&frame) {
        return Err(io::Error::from_raw_os_error(libc::ESTALE));
    }
    // The pointer belongs to the live assembly frame. Rust never restores from
    // it; final mask ownership remains below after all setup values are dropped.
    FRAME.set(core::ptr::null_mut());
    drop(frame);
    drop(callback);
    crate::runtime::verify_initial_rcb_clock()?;
    // Validation is read-only: physical enable and logical publication remain
    // one assembly-owned boundary while this exact mask is still blocked.
    let action = rcb::validate_root_release(pause.as_ref())
        .map_err(io::Error::from_raw_os_error)?;
    Ok(Some(action))
}

unsafe extern "C" fn failed(error: i64) -> ! {
    rcb::setup_failed(i32::try_from(-error).unwrap_or(libc::EIO));
    const MESSAGE: &[u8] = b"reverie-liteinst: root activation failed\n";
    unsafe {
        raw_syscall6(libc::SYS_write, [2, MESSAGE.as_ptr() as u64, MESSAGE.len() as u64, 0, 0, 0]);
        raw_syscall6(libc::SYS_exit_group, [126, 0, 0, 0, 0, 0]);
    }
    loop { core::hint::spin_loop(); }
}

/// A complete setup body, returning only after its Result checks and drops.
#[doc(hidden)]
pub type Setup = unsafe extern "C" fn(*mut c_void);

#[repr(C)]
#[doc(hidden)]
pub struct ConstructorArgs {
    pub argc: libc::c_int,
    pub argv: *mut *mut libc::c_char,
    pub envp: *mut *mut libc::c_char,
}

unsafe extern "C" {
    /// The actual assembly-owned root boundary. All code after its return is
    /// guest execution; a caller must put its complete setup inside `body`.
    ///
    /// # Safety
    /// The body consumes its argument exactly once, returns normally, creates
    /// no application thread, and runs only runtime setup. It must not unwind
    /// or escape the activation. A constructor must tail-jump to this symbol.
    #[doc(hidden)]
    pub fn reverie_liteinst_root_activation(body: Setup, argument: *mut c_void);
    #[doc(hidden)]
    pub fn reverie_liteinst_root_constructor();
    #[cfg(feature = "rcb-qualification")]
    static reverie_liteinst_root_activation_end: u8;
    #[cfg(feature = "rcb-qualification")]
    static reverie_liteinst_root_constructor_end: u8;
}

/// Put all Tool setup, Result handling and owned setup locals inside one actual
/// assembly-owned root activation. Code after the macro is guest execution.
///
/// # Safety
/// Calling this macro requires an enclosing `unsafe` block. The body runs only
/// setup, does not escape or create application threads, and obeys
/// `install_tool`'s process-global safety requirements. Its `extern "C"` thunk
/// cannot unwind across the assembly frame: a panic, including one from a
/// setup-owned destructor, aborts the process before guest execution resumes.
#[macro_export]
macro_rules! with_tool_root {
    ($body:block) => {{
        // The call site owns the outer activation contract. Keep narrower
        // unsafe blocks in the supplied body explicit and reviewable.
        #[allow(unused_unsafe)]
        let mut setup = ::core::mem::ManuallyDrop::new(|| $body);
        let (body, argument) = $crate::__root_setup(&mut setup);
        // `setup` is consumed and dropped inside body before activation. The
        // remaining ManuallyDrop/raw-pointer locals have no post-return drop.
        $crate::__root_activation(body, argument)
    }};
}

/// Register a complete argc/argv/envp constructor inside the root boundary.
/// Invoke once per preload DSO. The adapter after activation contains only
/// stack restoration and RET; no Rust constructor frame survives activation.
#[macro_export]
macro_rules! tool_root_constructor {
    ($initialize:path) => {
        const _: () = {
            unsafe extern "C" fn setup(argument: *mut ::core::ffi::c_void) {
                let args = unsafe { &*argument.cast::<$crate::__RootConstructorArgs>() };
                unsafe { $initialize(args.argc, args.argv, args.envp) };
            }
            unsafe extern "C" {
                fn __reverie_liteinst_root_constructor_entry();
            }
            #[used]
            #[unsafe(link_section = ".init_array")]
            static INITIALIZE: unsafe extern "C" fn() = __reverie_liteinst_root_constructor_entry;
            ::core::arch::global_asm!(
                ".text",
                ".global __reverie_liteinst_root_constructor_entry",
                ".hidden __reverie_liteinst_root_constructor_entry",
                ".type __reverie_liteinst_root_constructor_entry,@function",
                "__reverie_liteinst_root_constructor_entry:",
                "endbr64",
                "lea rcx,[rip+{setup}]",
                "jmp {entry}",
                ".size __reverie_liteinst_root_constructor_entry,.-__reverie_liteinst_root_constructor_entry",
                setup = sym setup,
                entry = sym $crate::__root_constructor,
            );
        };
    };
}

#[doc(hidden)]
pub fn setup<F: FnOnce()>(body: &mut ManuallyDrop<F>) -> (Setup, *mut c_void) {
    unsafe extern "C" fn invoke<F: FnOnce()>(argument: *mut c_void) {
        let body = unsafe { argument.cast::<F>().read() };
        body();
    }
    (invoke::<F>, (&mut **body as *mut F).cast())
}

/// Actual loaded root boundary bytes for the qualification inspector.
#[cfg(feature = "rcb-qualification")]
pub fn boundary() -> (u64, usize) {
    let start = reverie_liteinst_root_activation as *const () as usize;
    let end = core::ptr::addr_of!(reverie_liteinst_root_activation_end) as usize;
    (start as u64, end.checked_sub(start).expect("root boundary end"))
}

/// Actual loaded constructor adapter bytes; reading them does not invoke it.
#[cfg(feature = "rcb-qualification")]
pub fn constructor_boundary() -> (u64, usize) {
    let start = reverie_liteinst_root_constructor as *const () as usize;
    let end = core::ptr::addr_of!(reverie_liteinst_root_constructor_end) as usize;
    (start as u64, end.checked_sub(start).expect("root constructor end"))
}

#[cfg(test)]
pub(crate) fn assert_unwrapped_install_has_no_effects() {
    fn signals() -> ([[u64; 4]; 64], u64, i64) {
        let mut actions = [[0_u64; 4]; 64];
        for (index, action) in actions.iter_mut().enumerate() {
            assert_eq!(unsafe { raw_syscall6(libc::SYS_rt_sigaction,
                [(index + 1) as u64, 0, action.as_mut_ptr() as u64, 8, 0, 0]) }, 0);
        }
        let mut mask = 0_u64;
        assert_eq!(unsafe { raw_syscall6(libc::SYS_rt_sigprocmask,
            [libc::SIG_SETMASK as u64, 0, (&raw mut mask) as u64, 8, 0, 0]) }, 0);
        let seccomp = unsafe { raw_syscall6(libc::SYS_prctl, [libc::PR_GET_SECCOMP as u64, 0, 0, 0, 0, 0]) };
        (actions, mask, seccomp)
    }
    #[cfg(feature = "rcb-qualification")]
    {
        rcb::bind();
        rcb::assert_pristine();
    }
    let before = (crate::runtime::setup_snapshot(), crate::tool_host::handler_is_published(),
        signals(), INSTALL_STARTED.load(Ordering::Acquire), FRAME.get());
    assert!(before.4.is_null());
    let installers: [unsafe fn(&std::path::Path) -> io::Result<()>; 3] = [
        crate::install_tool::<()>, crate::install_tool_quiescent::<()>,
        crate::install_tool_from_bootstrap::<()>,
    ];
    for install in installers {
        let error = unsafe { install(std::path::Path::new("/unreachable-root-coordinator")) }.unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::Unsupported);
        assert_eq!(
            error.to_string(),
            "LiteInst Tool installation requires an outer root activation entry"
        );
        assert_eq!(before, (crate::runtime::setup_snapshot(), crate::tool_host::handler_is_published(),
            signals(), INSTALL_STARTED.load(Ordering::Acquire), FRAME.get()));
        #[cfg(feature = "rcb-qualification")]
        rcb::assert_pristine();
    }
    #[cfg(feature = "rcb-qualification")]
    rcb::assert_forgery_refused();
    // This is also the external-author contract: a DSO must use
    // `tool_root_constructor!`, while an ordinary in-tree fixture uses
    // `with_tool_root!`. Every raw public installer is refused before state,
    // signal disposition, seccomp, handler publication, or RCB ownership moves.
}

const _: () = {
    assert!(core::mem::size_of::<ConstructorArgs>() == 24);
    assert!(core::mem::offset_of!(ConstructorArgs, argc) == 0);
    assert!(core::mem::offset_of!(ConstructorArgs, argv) == 8);
    assert!(core::mem::offset_of!(ConstructorArgs, envp) == 16);
    assert!(core::mem::size_of::<HookContext>() == 144);
    assert!(core::mem::offset_of!(HookContext, instruction_pointer) == 0);
    assert!(core::mem::offset_of!(HookContext, stack_pointer) == 8);
    assert!(core::mem::offset_of!(HookContext, r15) == 16);
    assert!(core::mem::offset_of!(HookContext, rdi) == 80);
    assert!(core::mem::offset_of!(HookContext, rsi) == 88);
    assert!(core::mem::offset_of!(HookContext, rflags) == 136);
    assert!(core::mem::size_of::<RootMasks>() == 16);
    assert!(core::mem::offset_of!(RootMasks, original) == 0);
    assert!(core::mem::offset_of!(RootMasks, restore) == 8);
};

global_asm!(r#"
    .text
    .p2align 4
    .global reverie_liteinst_root_constructor
    .hidden reverie_liteinst_root_constructor
    .type reverie_liteinst_root_constructor,@function
reverie_liteinst_root_constructor:
    endbr64
    // Preserve all three real constructor arguments for the complete Rust
    // setup body. Entry RSP is 8 mod 16; the call below remains aligned.
    push rdx
    push rsi
    push rdi
    mov rsi,rsp
    mov rdi,rcx
    call reverie_liteinst_root_activation
    .global reverie_liteinst_root_constructor_return
    .hidden reverie_liteinst_root_constructor_return
reverie_liteinst_root_constructor_return:
    add rsp,24
    ret
    .global reverie_liteinst_root_constructor_end
    .hidden reverie_liteinst_root_constructor_end
reverie_liteinst_root_constructor_end:
    .size reverie_liteinst_root_constructor,.-reverie_liteinst_root_constructor
    .p2align 4
    .global reverie_liteinst_root_activation
    .hidden reverie_liteinst_root_activation
    .type reverie_liteinst_root_activation,@function
reverie_liteinst_root_activation:
    endbr64
    pushfq
    push rax
    push rcx
    push rdx
    push rbx
    push rbp
    push rsi
    push rdi
    push r8
    push r9
    push r10
    push r11
    push r12
    push r13
    push r14
    push r15
    lea rax,[rsp+136]
    push rax
    push qword ptr [rsp+136]
    mov r12,rsp
    // 512-byte FXSAVE, exact original/restore masks, and an all-signals input.
    sub rsp,536
    .global reverie_liteinst_root_activation_block_signals
    .hidden reverie_liteinst_root_activation_block_signals
reverie_liteinst_root_activation_block_signals:
    mov qword ptr [rsp+528],-1
    mov edi,{sigprocmask}
    mov esi,{block}
    lea rdx,[rsp+528]
    lea rcx,[rsp+512]
    mov r8d,8
    xor r9d,r9d
    call reverie_preload_trusted_syscall
    test rax,rax
    jz .Lroot_signals_blocked
    .global reverie_liteinst_root_activation_initial_mask_error
    .hidden reverie_liteinst_root_activation_initial_mask_error
reverie_liteinst_root_activation_initial_mask_error:
.Lroot_initial_mask_error:
    mov edi,{exit_group}
    mov esi,126
    xor edx,edx
    xor ecx,ecx
    xor r8d,r8d
    xor r9d,r9d
    call reverie_preload_trusted_syscall
    ud2
    .global reverie_liteinst_root_activation_signals_blocked
    .hidden reverie_liteinst_root_activation_signals_blocked
reverie_liteinst_root_activation_signals_blocked:
.Lroot_signals_blocked:
    mov rax,[rsp+512]
    mov [rsp+520],rax
    fxsave64 [rsp]
    cld
    lea rdi,[rsp+512]
    call {begin}
    mov rdi,[r12+88]
    call qword ptr [r12+80]
    mov rdi,r12
    call {finish}
    mov r13,rax
    call reverie_preload_rcb_record
    mov r15,rax
    mov rax,r13
    fxrstor64 [rsp]
    .global reverie_liteinst_root_activation_select
    .hidden reverie_liteinst_root_activation_select
reverie_liteinst_root_activation_select:
    test rax,rax
    js .Lroot_action_error
    jz .Lroot_restore_mask
    dec rax
    jz .Lroot_restore_mask
    .global reverie_liteinst_root_activation_enable
    .hidden reverie_liteinst_root_activation_enable
reverie_liteinst_root_activation_enable:
    lea rsi,[rax-1]
    mov edi,{ioctl}
    mov edx,{enable}
    xor ecx,ecx
    xor r8d,r8d
    xor r9d,r9d
    call reverie_preload_trusted_syscall
    mov rdi,rax
    lea r13,[rip+.Lroot_enable_error]
    lea r14,[rip+.Lroot_enabled]
    test rax,rax
    cmovz r13,r14
    jmp r13
.global reverie_liteinst_root_activation_action_error
.hidden reverie_liteinst_root_activation_action_error
reverie_liteinst_root_activation_action_error:
.Lroot_action_error:
    mov rdi,rax
    call {failed}
    ud2
.global reverie_liteinst_root_activation_enable_error
.hidden reverie_liteinst_root_activation_enable_error
reverie_liteinst_root_activation_enable_error:
.Lroot_enable_error:
    call {failed}
    ud2
    .global reverie_liteinst_root_activation_enabled
    .hidden reverie_liteinst_root_activation_enabled
reverie_liteinst_root_activation_enabled:
.Lroot_enabled:
    // Signals remain blocked. Publish only after the kernel accepted ENABLE,
    // so the first signal admitted by the restore sees one coherent state.
    mov qword ptr [r15+{held_offset}],0
    mov qword ptr [r15+{mode_offset}],{running}
    add qword ptr [r15+{enables_offset}],1
    .global reverie_liteinst_root_activation_published
    .hidden reverie_liteinst_root_activation_published
reverie_liteinst_root_activation_published:
    .global reverie_liteinst_root_activation_restore_mask
    .hidden reverie_liteinst_root_activation_restore_mask
reverie_liteinst_root_activation_restore_mask:
.Lroot_restore_mask:
    mov edi,{sigprocmask}
    mov esi,{setmask}
    lea rdx,[rsp+520]
    xor ecx,ecx
    mov r8d,8
    xor r9d,r9d
    call reverie_preload_trusted_syscall
    mov rdi,rax
    lea r13,[rip+.Lroot_mask_error]
    lea r14,[rip+.Lroot_mask_restored]
    test rax,rax
    cmovz r13,r14
    jmp r13
    .global reverie_liteinst_root_activation_mask_error
    .hidden reverie_liteinst_root_activation_mask_error
reverie_liteinst_root_activation_mask_error:
.Lroot_mask_error:
    call {failed}
    ud2
    .global reverie_liteinst_root_activation_mask_restored
    .hidden reverie_liteinst_root_activation_mask_restored
reverie_liteinst_root_activation_mask_restored:
.Lroot_mask_restored:
    .global reverie_liteinst_root_activation_return
    .hidden reverie_liteinst_root_activation_return
reverie_liteinst_root_activation_return:
.Lroot_return:
    add rsp,552
    pop r15
    pop r14
    pop r13
    pop r12
    pop r11
    pop r10
    pop r9
    pop r8
    pop rdi
    pop rsi
    pop rbp
    pop rbx
    pop rdx
    pop rcx
    pop rax
    popfq
    ret
    .global reverie_liteinst_root_activation_end
    .hidden reverie_liteinst_root_activation_end
reverie_liteinst_root_activation_end:
    .size reverie_liteinst_root_activation,.-reverie_liteinst_root_activation
"#, begin = sym begin, finish = sym finish, failed = sym failed,
    ioctl = const libc::SYS_ioctl, enable = const 0x2400u32,
    sigprocmask = const libc::SYS_rt_sigprocmask, block = const libc::SIG_BLOCK,
    setmask = const libc::SIG_SETMASK, exit_group = const libc::SYS_exit_group,
    mode_offset = const rcb::MODE_OFFSET, held_offset = const rcb::HELD_OFFSET,
    enables_offset = const rcb::ENABLES_OFFSET, running = const rcb::RUNNING_MODE);
