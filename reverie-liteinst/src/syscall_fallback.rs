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
static CALLBACK_MXCSR: u32 = 0x1f80;

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
}

fn configure_save() -> Result<(), &'static str> {
    let features = core::arch::x86_64::__cpuid(1);
    if features.ecx & ((1 << 26) | (1 << 27)) == ((1 << 26) | (1 << 27)) {
        // XCR0 includes every enabled user component, including components
        // unknown to this runtime. Never silently discard a component here.
        let enabled = unsafe { core::arch::x86_64::_xgetbv(0) };
        let state = core::arch::x86_64::__cpuid_count(0xD, 0);
        let supported = u64::from(state.eax) | (u64::from(state.edx) << 32);
        let (mask, bytes) = save_configuration(enabled, supported, state.ebx)?;
        SAVE_BYTES.store(bytes, Ordering::Relaxed);
        SAVE_MASK.store(mask, Ordering::Relaxed);
    }
    Ok(())
}

fn save_configuration(
    enabled: u64,
    supported: u64,
    bytes: u32,
) -> Result<(u64, u32), &'static str> {
    let bytes = bytes.checked_add(63).ok_or("XSAVE size overflow")? & !63;
    if enabled == 0 || enabled & !supported != 0 || bytes < 576 || bytes > i32::MAX as u32 {
        return Err("unsupported XSAVE configuration");
    }
    Ok((enabled, bytes))
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
    let instruction = continuation.checked_sub(2)?;
    PENDING.with(|pending| {
        if !READY.get() || pending.get().is_some() {
            return None;
        }
        pending.set(Some(Pending { instruction }));
        Some(fallback_entry as *const () as u64)
    })
}

unsafe extern "C" fn dispatch(context: *mut HookContext) {
    PENDING.with(|pending| {
        let Some(continuation) = pending.get() else {
            unsafe { raw_syscall6(libc::SYS_exit_group, [123, 0, 0, 0, 0, 0]) };
            std::process::abort();
        };
        if context.is_null() {
            unsafe { raw_syscall6(libc::SYS_exit_group, [123, 0, 0, 0, 0, 0]) };
            std::process::abort();
        }
        let errno = unsafe { libc::__errno_location() };
        let saved_errno = unsafe { *errno };
        unsafe {
            (*context).instruction_pointer = continuation.instruction;
            crate::runtime::dispatch_fallback_context(context);
            *errno = saved_errno;
        }
        // The saved RCX now owns the return address. No TLS is read on return,
        // so a subsequent continuation cannot redirect this activation.
        pending.set(None);
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
    // Rust callbacks require the ordinary empty x87 stack and masked FP
    // exceptions even when the guest supplied a different FP environment.
    fninit
    ldmxcsr [rip + {callback_mxcsr}]
    mov rdi, r12
    call {dispatch}
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
    .global fallback_entry_end
    .hidden fallback_entry_end
fallback_entry_end:
    .size fallback_entry, .-fallback_entry
    "#,
    save_bytes = sym SAVE_BYTES,
    save_mask = sym SAVE_MASK,
    dispatch = sym dispatch,
    callback_mxcsr = sym CALLBACK_MXCSR,
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_enabled_user_component_is_saved_without_a_fixed_mask() {
        // Include MPX, AMX, and a future high component bit: CPU-provided
        // layout and XCR0, rather than an instruction-set list, own the mask.
        let enabled = 0x2ff | (3 << 17) | (1 << 40);
        assert_eq!(
            save_configuration(enabled, enabled, 11_009),
            Ok((enabled, 11_072))
        );
        assert!(save_configuration(enabled, 0x2e7, 11_009).is_err());
        assert!(save_configuration(0, 0, 576).is_err());
        assert!(save_configuration(3, 3, 512).is_err());
        assert!(save_configuration(3, 3, u32::MAX).is_err());
    }

    #[test]
    fn pending_continuation_refuses_reentry_without_overwriting() {
        initialize().unwrap();
        assert!(prepare(0x1002).is_some());
        assert!(prepare(0x2002).is_none());
        assert_eq!(
            PENDING.get(),
            Some(Pending {
                instruction: 0x1000
            })
        );
        assert!(initialize().is_err());
        PENDING.set(None);
        assert!(prepare(0x3002).is_some());
        assert_eq!(
            PENDING.get(),
            Some(Pending {
                instruction: 0x3000
            })
        );
        PENDING.set(None);
    }
}
