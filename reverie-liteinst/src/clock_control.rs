use std::io;
use std::os::fd::AsRawFd;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;

use reverie_preload::clock_boundary::BoundaryHooks;
use reverie_preload::trap::raw_syscall6;
use reverie_ptrace::InGuestRcbCounter;

#[repr(C)]
struct Control {
    requested: AtomicU64,
    ready: AtomicU64,
    descriptor: AtomicU64,
    gate: AtomicU64,
    running: AtomicU64,
    owner: AtomicU64,
    pending_witness: AtomicU64,
    pending_token: AtomicU64,
}

const _: () = assert!(std::mem::size_of::<Control>() == 64);

unsafe extern "C" {
    fn reverie_liteinst_clock_control() -> *const Control;
    pub(crate) fn reverie_liteinst_clock_enter(witness: u64) -> u64;
    pub(crate) fn reverie_liteinst_clock_leave(token: u64, kind: u64, witness: u64);
    pub(crate) fn reverie_liteinst_clock_invoke_hook();
    pub fn reverie_liteinst_clock_constructor_begin() -> u64;
    pub fn reverie_liteinst_clock_constructor_finish(selected: i32, prior_phase: u64);
}

fn control() -> &'static Control {
    unsafe { &*reverie_liteinst_clock_control() }
}

pub(crate) fn requested() -> bool {
    control().requested.load(Ordering::Relaxed) != 0
}

pub(crate) fn active() -> bool {
    control().ready.load(Ordering::Acquire) != 0
}

pub(crate) fn descriptor() -> i32 {
    if active() {
        control().descriptor.load(Ordering::Relaxed) as i32
    } else {
        -1
    }
}

pub(crate) fn paused() -> bool {
    control().running.load(Ordering::Relaxed) == 0
}

pub(crate) fn callback_return_pc(
    trampoline: &liteinst2::trampoline::ExecutableTrampoline,
) -> io::Result<u64> {
    let layout = trampoline.layout();
    if layout.instrumentation_len < 2
        || layout.restore_len == 0
        || layout.instrumentation_len >= trampoline.code_len()
    {
        return Err(io::Error::other("invalid callback restoration boundary"));
    }
    let witness = trampoline
        .address()
        .checked_add(layout.instrumentation_len as u64)
        .ok_or_else(|| io::Error::other("callback boundary overflow"))?;
    let call = unsafe { std::slice::from_raw_parts((witness - 2) as *const u8, 2) };
    if call != [0xff, 0xd0] {
        return Err(io::Error::other("callback boundary is not after CALL RAX"));
    }
    Ok(witness)
}

pub(crate) fn publish(clock: &InGuestRcbCounter) -> io::Result<()> {
    let state = control();
    if !requested() || active() {
        return Err(io::Error::other("clock constructor boundary unavailable"));
    }
    unsafe { clock.read_paused_once() }.map_err(io::Error::other)?;
    let descriptor = unsafe { clock.boundary_fd() }.as_raw_fd();
    let owner = unsafe { raw_syscall6(libc::SYS_gettid, [0; 6]) };
    if descriptor < 0 || owner <= 0 {
        return Err(io::Error::other("clock control owner unavailable"));
    }
    unsafe { reverie_preload::clock_boundary::register(&BOUNDARY_HOOKS)? };
    state.descriptor.store(descriptor as u64, Ordering::Relaxed);
    state.gate.store(
        reverie_preload::trap::trusted_gate().syscall_ip,
        Ordering::Relaxed,
    );
    state.owner.store(owner as u64, Ordering::Relaxed);
    state.ready.store(1, Ordering::Release);
    Ok(())
}

static BOUNDARY_HOOKS: BoundaryHooks = BoundaryHooks {
    enter: reverie_liteinst_clock_enter,
    leave: reverie_liteinst_clock_leave,
};

unsafe extern "C" fn fail() -> ! {
    unsafe { raw_syscall6(libc::SYS_exit_group, [127, 0, 0, 0, 0, 0]) };
    unsafe { core::arch::asm!("ud2", options(noreturn)) }
}

core::arch::global_asm!(include_str!("clock_control.s"), fail = sym fail);

/// Install one clocked preload constructor with an assembly-owned final return.
///
/// The initializer must have type `unsafe extern "C" fn() -> i32`. It returns
/// zero without installation when unselected, one after successful typed Tool
/// installation, and any other value on failure. All Rust cleanup belongs in
/// that initializer; no code may run between its return and this assembly tail.
/// The selected path currently requires the installing thread for its lifetime.
/// Production timer delivery remains unavailable. Do not unwind from the callback.
#[macro_export]
macro_rules! clocked_initializer {
    ($name:ident, $initialize:path) => {
        #[used]
        #[unsafe(link_section = ".init_array")]
        static $name: unsafe extern "C" fn() = {
            const _: unsafe extern "C" fn() -> i32 = $initialize;
            #[unsafe(naked)]
            unsafe extern "C" fn entry() {
                core::arch::naked_asm!(
                    "sub rsp, 24",
                    "call {begin}",
                    "mov [rsp], rax",
                    "call {initialize}",
                    "mov edi, eax",
                    "mov rsi, [rsp]",
                    "add rsp, 24",
                    "jmp {finish}",
                    begin = sym $crate::__clock_constructor_begin,
                    initialize = sym $initialize,
                    finish = sym $crate::__clock_constructor_finish,
                );
            }
            entry
        };
    };
}

macro_rules! installed_hook {
    ($name:ident, $body:ident) => {
        #[unsafe(naked)]
        unsafe extern "C" fn $name(context: *mut HookContext) {
            core::arch::naked_asm!(
                "lea rax, [rip + {body}]",
                "jmp {invoke}",
                body = sym $body,
                invoke = sym crate::clock_control::reverie_liteinst_clock_invoke_hook,
            );
        }
    };
}

pub(crate) use installed_hook;

#[cfg(test)]
mod tests;
