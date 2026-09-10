use std::io;
use std::os::fd::AsRawFd;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;

use reverie::pmu::InGuestRcbCounter;
use reverie_preload::clock_boundary::BoundaryHooks;
use reverie_preload::trap::raw_syscall6;

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
    notification_fd_plus_one: AtomicU64,
    notification_armed: AtomicU64,
    notification_owner: AtomicU64,
    notification_revision: AtomicU64,
}

const _: () = assert!(std::mem::size_of::<Control>() == 96);

unsafe extern "C" {
    fn reverie_liteinst_domain_depth() -> u64;
    fn reverie_liteinst_clock_control() -> *const Control;
    pub(crate) fn reverie_liteinst_clock_enter(witness: u64) -> u64;
    pub(crate) fn reverie_liteinst_clock_leave(token: u64, kind: u64, witness: u64);
    pub(crate) fn reverie_liteinst_clock_invoke_hook();
    pub fn reverie_liteinst_clock_constructor_begin() -> u64;
    pub fn reverie_liteinst_clock_constructor_finish(selected: i32, prior_phase: u64);
}

/// Retain the already paused constructor domain through private GNU startup.
///
/// # Safety
/// Call only after a successful actual Tool initializer, in its original
/// constructor domain, from the retained private CRT owner. This is not an
/// ownership or Tool-readiness check. All guest execution remains prohibited
/// until that same owner completes lifecycle callbacks and the final assembly
/// `clock_leave(1, 0, 0)` transfer. Never use this to postpone guest counting.
pub unsafe fn defer_private_constructor() -> io::Result<()> {
    if !requested()
        || !active()
        || !paused()
        || !handoff_clear()
        || unsafe { reverie_liteinst_domain_depth() } != 1
    {
        return Err(io::Error::other(
            "private constructor has no paused outer clock domain",
        ));
    }
    control().requested.store(0, Ordering::Release);
    Ok(())
}

fn control() -> &'static Control {
    unsafe { &*reverie_liteinst_clock_control() }
}

/// Host-test setup for the shared clock control block.
///
/// This module exists only under `cfg(test)`, so it is absent from every
/// shipped build and is not a runtime capability.
#[cfg(test)]
pub(crate) mod test_control {
    use super::Ordering;
    use super::control;

    pub(crate) fn snapshot() -> [u64; 4] {
        let state = control();
        [
            state.requested.load(Ordering::Relaxed),
            state.ready.load(Ordering::Relaxed),
            state.running.load(Ordering::Relaxed),
            state.notification_armed.load(Ordering::Relaxed),
        ]
    }

    pub(crate) fn restore(values: [u64; 4]) {
        let state = control();
        state.requested.store(values[0], Ordering::Relaxed);
        state.ready.store(values[1], Ordering::Release);
        state.running.store(values[2], Ordering::Relaxed);
        state.notification_armed.store(values[3], Ordering::Release);
    }

    pub(crate) fn set_requested(value: u64) {
        control().requested.store(value, Ordering::Relaxed);
    }

    pub(crate) fn set_ready(value: u64) {
        control().ready.store(value, Ordering::Release);
    }

    pub(crate) fn set_running(value: u64) {
        control().running.store(value, Ordering::Relaxed);
    }

    pub(crate) fn set_notification_armed(value: u64) {
        control().notification_armed.store(value, Ordering::Release);
    }
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

pub(crate) fn notification_free() -> bool {
    let state = control();
    [
        &state.notification_fd_plus_one,
        &state.notification_armed,
        &state.notification_owner,
        &state.notification_revision,
    ]
    .into_iter()
    .all(|field| field.load(Ordering::Acquire) == 0)
}

pub(crate) fn handoff_clear() -> bool {
    let state = control();
    state.pending_witness.load(Ordering::Relaxed) == 0
        && state.pending_token.load(Ordering::Relaxed) == 0
}

pub(crate) fn notification_descriptor() -> i32 {
    control()
        .notification_fd_plus_one
        .load(Ordering::Relaxed)
        .wrapping_sub(1) as i32
}

pub(crate) fn notification_owner() -> *mut crate::timer::TimerOwner {
    control().notification_owner.load(Ordering::Acquire) as *mut crate::timer::TimerOwner
}

pub(crate) fn cancel_notification_resume() {
    control().notification_armed.store(0, Ordering::Release);
}

pub(crate) fn notification_revision() -> u64 {
    control().notification_revision.load(Ordering::Acquire)
}

pub(crate) fn finish_notification_update(revision: u64) {
    control()
        .notification_revision
        .store(revision, Ordering::Release);
}

pub(crate) fn publish_notification(owner: Box<crate::timer::TimerOwner>) -> io::Result<()> {
    if !active() || !paused() || !notification_owner().is_null() {
        return Err(io::Error::other("notification owner cannot be rebound"));
    }
    let descriptor = owner.descriptor();
    let pointer = Box::into_raw(owner);
    control()
        .notification_fd_plus_one
        .store(descriptor as u64 + 1, Ordering::Relaxed);
    control()
        .notification_owner
        .store(pointer as u64, Ordering::Release);
    Ok(())
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

#[cfg(test)]
mod notification_tests;

#[cfg(test)]
mod interrupted_notification_tests;

#[cfg(test)]
mod counting_only_tests {
    use super::*;

    #[test]
    fn every_notification_field_prevents_counting_only_admission() {
        let state = control();
        assert!(notification_free());
        for field in [
            &state.notification_fd_plus_one,
            &state.notification_armed,
            &state.notification_owner,
            &state.notification_revision,
        ] {
            field.store(1, Ordering::Relaxed);
            assert!(!notification_free());
            field.store(0, Ordering::Relaxed);
        }
        assert!(notification_free());
        assert!(handoff_clear());
        for field in [&state.pending_token, &state.pending_witness] {
            field.store(1, Ordering::Relaxed);
            assert!(!handoff_clear());
            field.store(0, Ordering::Relaxed);
        }
        assert!(handoff_clear());
    }
}
