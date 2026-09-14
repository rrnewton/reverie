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
static PKRU_OFFSET: AtomicU32 = AtomicU32::new(0);
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
        // Choose the PKRU-aware entry before returning from SIGSYS. It must
        // open runtime memory before reading any global or TLS storage.
        let features = core::arch::x86_64::__cpuid_count(7, 0);
        if mask & (1 << 9) != 0 && features.ecx & (1 << 4) != 0 {
            let component = core::arch::x86_64::__cpuid_count(0xD, 9);
            let offset = pkru_offset(component.ebx, component.eax, state.ebx)?;
            PKRU_OFFSET.store(offset, Ordering::Relaxed);
        }
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

fn pkru_offset(offset: u32, size: u32, bytes: u32) -> Result<u32, &'static str> {
    if size != 8 || offset < 576 || offset.checked_add(size).is_none_or(|end| end > bytes) {
        return Err("unsupported PKRU XSAVE layout");
    }
    Ok(offset)
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
pub(crate) fn prepare(instruction: u64) -> Option<u64> {
    PENDING.with(|pending| {
        if !READY.get() || pending.get().is_some() {
            return None;
        }
        pending.set(Some(Pending { instruction }));
        Some(if PKRU_OFFSET.load(Ordering::Relaxed) != 0 {
            fallback_entry_pkru as *const () as u64
        } else {
            fallback_entry as *const () as u64
        })
    })
}

/// Linux gives a signal handler default PKRU, which can deny the nondefault
/// key holding the fallback's callback stack. A nested Tool syscall must regain
/// runtime access before the trusted gate reads its arguments. The kernel's
/// saved interrupted PKRU is untouched and sigreturn restores it afterwards.
pub(crate) fn enable_nested_runtime_access() {
    if PKRU_OFFSET.load(Ordering::Relaxed) != 0 && PENDING.get().is_some() {
        unsafe {
            core::arch::asm!(
                "wrpkru",
                "lfence",
                in("eax") 0u32,
                in("ecx") 0u32,
                in("edx") 0u32,
                options(nostack, preserves_flags),
            );
        }
    }
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
    fn fallback_entry_pkru();
}

global_asm!(
    r#"
    .text
    .macro save_registers
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
    .endm

    .global fallback_entry
    .hidden fallback_entry
    .type fallback_entry,@function
fallback_entry:
    save_registers
    xor r14d, r14d
    jmp 1f

    .global fallback_entry_pkru
    .hidden fallback_entry_pkru
    .type fallback_entry_pkru,@function
fallback_entry_pkru:
    save_registers
    // All guest registers are saved on its accessible stack. Neither the
    // feature decision nor a memory read may precede this permissions change.
    xor ecx, ecx
    rdpkru
    mov r13d, eax
    xor eax, eax
    xor edx, edx
    wrpkru
    lfence
    mov r14d, 1
1:
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
    test r14d, r14d
    jz 3f
    // XSAVE observed the temporary runtime PKRU. Put the guest value and its
    // initial-state bit back without changing any other component or header.
    mov r10d, dword ptr [rip + {pkru_offset}]
    mov qword ptr [rsp + r10], r13
    and qword ptr [rsp + 512], -513
    test r13d, r13d
    jz 3f
    or qword ptr [rsp + 512], 512
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
    // Finish all runtime/global/TLS access before restoring guest PKRU.
    // Future runtime boundaries must also precede this restore. The suffix
    // uses only registers and the guest-accessible frame, even with key0 denied.
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
    pkru_offset = sym PKRU_OFFSET,
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
    fn pkru_layout_must_fit_the_standard_save_area() {
        assert_eq!(pkru_offset(2688, 8, 2696), Ok(2688));
        assert!(pkru_offset(2688, 8, 2695).is_err());
        assert!(pkru_offset(512, 8, 2696).is_err());
        assert!(pkru_offset(2688, 4, 2696).is_err());
        assert!(pkru_offset(u32::MAX - 3, 8, u32::MAX).is_err());
    }

    #[test]
    fn pending_continuation_refuses_reentry_without_overwriting() {
        initialize().unwrap();
        assert!(prepare(0x1000).is_some());
        assert!(prepare(0x2000).is_none());
        assert_eq!(
            PENDING.get(),
            Some(Pending {
                instruction: 0x1000
            })
        );
        assert!(initialize().is_err());
        PENDING.set(None);
        assert!(prepare(0x3000).is_some());
        assert_eq!(
            PENDING.get(),
            Some(Pending {
                instruction: 0x3000
            })
        );
        PENDING.set(None);
    }
}
