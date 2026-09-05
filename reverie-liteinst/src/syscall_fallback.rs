use core::arch::global_asm;
use core::cell::Cell;
use core::sync::atomic::AtomicU32;
use core::sync::atomic::AtomicU64;
use core::sync::atomic::Ordering;
use std::io;
use std::sync::OnceLock;

use liteinst2::trampoline::HookContext;
use reverie_preload::trap::raw_syscall6;

static SAVE_BYTES: AtomicU32 = AtomicU32::new(512);
static SAVE_MASK: AtomicU64 = AtomicU64::new(0);
static SAVE_CONFIG: OnceLock<Result<(), &'static str>> = OnceLock::new();

const _: () = {
    assert!(core::mem::size_of::<HookContext>() == 144);
    assert!(core::mem::offset_of!(HookContext, r11) == 48);
    assert!(core::mem::offset_of!(HookContext, rflags) == 136);
};

thread_local! {
    static READY: Cell<bool> = const { Cell::new(false) };
    static PENDING: Cell<Option<Pending>> = const { Cell::new(None) };
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Pending {
    instruction: u64,
    guest_mask: Option<u64>,
}

#[repr(C)]
struct MaskReturn {
    guest_mask: u64,
    restore_mask: u64,
}

pub(crate) fn runtime_mask() -> u64 {
    reverie_preload::signal::runtime_ordinary_mask()
}

fn configure_save() -> Result<(), &'static str> {
    let features = core::arch::x86_64::__cpuid(1);
    if features.ecx & ((1 << 26) | (1 << 27)) == ((1 << 26) | (1 << 27)) {
        let mask = unsafe { core::arch::x86_64::_xgetbv(0) } & 0x2e7;
        let state = core::arch::x86_64::__cpuid_count(0xD, 0);
        let bytes = state.ebx.checked_add(63).ok_or("XSAVE size overflow")? & !63;
        if mask == 0 || bytes < 576 || bytes > i32::MAX as u32 {
            return Err("unsupported XSAVE configuration");
        }
        SAVE_BYTES.store(bytes, Ordering::Relaxed);
        SAVE_MASK.store(mask, Ordering::Relaxed);
    }
    Ok(())
}

pub(crate) fn initialize() -> io::Result<()> {
    SAVE_CONFIG
        .get_or_init(configure_save)
        .as_ref()
        .map_err(|error| io::Error::other(*error))?;
    if PENDING.get().is_some() {
        return Err(io::Error::other("a syscall continuation is still active"));
    }
    READY.set(true);
    Ok(())
}

/// Retain only the original instruction address until ordinary dispatch ends.
/// Reentry before then refuses without overwriting the active continuation.
/// Afterwards the saved RCX owns the resume address; return reads no TLS state.
pub(crate) fn prepare(continuation: u64) -> Option<u64> {
    prepare_with_mask(continuation, None)
}

pub(crate) fn prepare_masked(continuation: u64, mask: u64) -> Option<u64> {
    prepare_with_mask(continuation, Some(mask))
}

fn prepare_with_mask(continuation: u64, guest_mask: Option<u64>) -> Option<u64> {
    let instruction = continuation.checked_sub(2)?;
    PENDING.with(|pending| {
        if !READY.get() || pending.get().is_some() {
            return None;
        }
        pending.set(Some(Pending {
            instruction,
            guest_mask,
        }));
        Some(fallback_entry as *const () as u64)
    })
}

unsafe extern "C" fn dispatch(context: *mut HookContext) -> MaskReturn {
    PENDING.with(|pending| {
        let continuation = pending.get();
        if continuation.is_none() || context.is_null() {
            unsafe { raw_syscall6(libc::SYS_exit_group, [123, 0, 0, 0, 0, 0]) };
            std::process::abort();
        }
        let errno = unsafe { libc::__errno_location() };
        let saved_errno = unsafe { *errno };
        let continuation = continuation.unwrap();
        let mut guest_mask = continuation.guest_mask.unwrap_or(0);
        unsafe {
            (*context).instruction_pointer = continuation.instruction;
            if continuation.guest_mask.is_some() {
                reverie_preload::user_dispatch::with_ordinary_dispatch_mask(
                    &mut guest_mask,
                    runtime_mask(),
                    || {
                        crate::runtime::dispatch_fallback_context(context);
                    },
                );
            } else {
                crate::runtime::dispatch_fallback_context(context);
            }
            *errno = saved_errno;
        }
        pending.set(None);
        if continuation.guest_mask.is_some()
            && guest_mask
                & ((1u64 << (libc::SIGSYS - 1)) | reverie_preload::signal::runtime_signal_mask())
                != 0
        {
            unsafe { crate::runtime::exit_now(125) };
        }
        MaskReturn {
            guest_mask,
            restore_mask: u64::from(continuation.guest_mask.is_some()),
        }
    })
}

unsafe extern "C" {
    fn fallback_entry();
}

global_asm!(
    r#"
    .text
    .global fallback_entry
    .hidden fallback_entry
    .type fallback_entry,@function
fallback_entry:
    lea rsp, [rsp - 128]
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
    lea rax, [rsp + 256]
    push rax
    push 0
    mov r12, rsp
    and rsp, -64
    mov r10d, dword ptr [rip + {save_bytes}]
    sub rsp, r10
    lea rdi, [rip + fallback_entry]
    call {clock_enter}
    mov r13, rax
    mov rax, qword ptr [rip + {save_mask}]
    test rax, rax
    jz 2f
    .irp offset,512,520,528,536,544,552,560,568
    mov qword ptr [rsp + \offset], 0
    .endr
    mov rdx, rax
    shr rdx, 32
    xsave64 [rsp]
    jmp 3f
2:
    fxsave64 [rsp]
3:
    cld
    mov rdi, r12
    call {dispatch}
    mov r14, rax
    mov r15, rdx
    mov rax, qword ptr [rip + {save_mask}]
    test rax, rax
    jz 4f
    mov rdx, rax
    shr rdx, 32
    xrstor64 [rsp]
    jmp 5f
4:
    fxrstor64 [rsp]
5:
    test r15, r15
    jz 6f
    sub rsp, 16
    mov [rsp], r14
    mov eax, 14
    mov edi, 2
    mov rsi, rsp
    xor edx, edx
    mov r10d, 8
    .global reverie_liteinst_fallback_mask_restore
    .hidden reverie_liteinst_fallback_mask_restore
reverie_liteinst_fallback_mask_restore:
    call reverie_preload_trusted_syscall_ip
    .global reverie_liteinst_fallback_mask_restored
    .hidden reverie_liteinst_fallback_mask_restored
reverie_liteinst_fallback_mask_restored:
    test rax, rax
    jnz 7f
    add rsp, 16
6:
    mov rdi, r13
    xor esi, esi
    xor edx, edx
    call {clock_leave}
    mov rsp, r12
    add rsp, 16
    pop r15
    pop r14
    pop r13
    pop r12
    add rsp, 8
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
    mov r11, [rsp - 88]
    popfq
    lea rsp, [rsp + 128]
    notrack jmp rcx
7:
    mov eax, 231
    mov edi, 125
    call reverie_preload_trusted_syscall_ip
    ud2
    .global fallback_entry_end
    .hidden fallback_entry_end
fallback_entry_end:
    .size fallback_entry, .-fallback_entry
    "#,
    save_bytes = sym SAVE_BYTES,
    save_mask = sym SAVE_MASK,
    dispatch = sym dispatch,
    clock_enter = sym crate::clock_control::reverie_liteinst_clock_enter,
    clock_leave = sym crate::clock_control::reverie_liteinst_clock_leave,
);

#[cfg(test)]
mod tests {
    #[test]
    fn pending_continuation_refuses_reentry_without_overwriting() {
        super::READY.set(true);
        assert!(super::prepare(0x1002).is_some());
        assert!(super::prepare(0x2002).is_none());
        assert_eq!(super::PENDING.get().unwrap().instruction, 0x1000);
        assert_eq!(super::PENDING.get().unwrap().guest_mask, None);
        assert!(super::initialize().is_err());
        super::PENDING.set(None);
        assert!(super::prepare(0x3002).is_some());
        assert_eq!(super::PENDING.get().unwrap().instruction, 0x3000);
        super::PENDING.set(None);
    }

    #[test]
    fn zero_mask_is_owned_and_reentry_does_not_replace_it() {
        super::READY.set(true);
        assert!(super::prepare_masked(0x4002, 0).is_some());
        assert!(super::prepare_masked(0x5002, 0x100).is_none());
        assert_eq!(
            super::PENDING.get(),
            Some(super::Pending {
                instruction: 0x4000,
                guest_mask: Some(0),
            })
        );
        super::PENDING.set(None);
    }
}
