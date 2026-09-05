use core::arch::global_asm;
use core::cell::Cell;
use core::sync::atomic::AtomicU32;
use core::sync::atomic::AtomicU64;
use core::sync::atomic::Ordering;
use std::io;
use std::ptr;
use std::sync::OnceLock;

use liteinst2::trampoline::HookContext;
use reverie_preload::trap::raw_syscall6;

static SAVE_BYTES: AtomicU32 = AtomicU32::new(512);
static SAVE_MASK: AtomicU64 = AtomicU64::new(0);
static SAVE_CONFIG: OnceLock<Result<(), &'static str>> = OnceLock::new();

#[repr(C)]
struct Pending {
    continuation: u64,
    instruction: u64,
    return_stub: usize,
}

const _: () = {
    assert!(core::mem::size_of::<HookContext>() == 144);
    assert!(core::mem::offset_of!(HookContext, r11) == 48);
    assert!(core::mem::offset_of!(HookContext, rflags) == 136);
    assert!(core::mem::offset_of!(Pending, continuation) == 0);
};

thread_local! {
    static PENDING: Cell<*mut Pending> = const { Cell::new(ptr::null_mut()) };
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
    PENDING.with(|pending| {
        if !pending.get().is_null() {
            return Ok(());
        }
        let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
        let page = usize::try_from(page).map_err(io::Error::other)?;
        let length = page
            .checked_mul(2)
            .ok_or_else(|| io::Error::other("page size overflow"))?;
        let start = fallback_return_template as *const u8;
        let end = ptr::addr_of!(fallback_return_template_end);
        let code_len = end as usize - start as usize;
        let displacement = page
            .checked_sub(code_len)
            .and_then(|value| i32::try_from(value).ok())
            .ok_or_else(|| io::Error::other("fallback return code does not fit a page"))?;
        let mapping = unsafe {
            libc::mmap(
                ptr::null_mut(),
                length,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
                -1,
                0,
            )
        };
        if mapping == libc::MAP_FAILED {
            return Err(io::Error::last_os_error());
        }
        let destination = mapping.cast::<u8>();
        let state = unsafe { destination.add(page).cast::<Pending>() };
        unsafe {
            ptr::copy_nonoverlapping(start, destination, code_len);
            destination
                .add(code_len - 4)
                .cast::<i32>()
                .write_unaligned(displacement);
            state.write(Pending {
                continuation: 0,
                instruction: 0,
                return_stub: mapping as usize,
            });
        }
        if unsafe { libc::mprotect(mapping, page, libc::PROT_READ | libc::PROT_EXEC) } != 0 {
            let error = io::Error::last_os_error();
            unsafe { libc::munmap(mapping, length) };
            return Err(error);
        }
        pending.set(state);
        Ok(())
    })
}

pub(crate) fn prepare(continuation: u64) -> Option<u64> {
    let instruction = continuation.checked_sub(2)?;
    PENDING.with(|pending| {
        let state = pending.get();
        if state.is_null() {
            return None;
        }
        unsafe {
            (*state).continuation = continuation;
            (*state).instruction = instruction;
        }
        Some(fallback_entry as *const () as u64)
    })
}

unsafe extern "C" fn dispatch(context: *mut HookContext) -> usize {
    PENDING.with(|pending| {
        let state = pending.get();
        if state.is_null() || context.is_null() {
            unsafe { raw_syscall6(libc::SYS_exit_group, [123, 0, 0, 0, 0, 0]) };
            std::process::abort();
        }
        let errno = unsafe { libc::__errno_location() };
        let saved_errno = unsafe { *errno };
        unsafe {
            (*context).instruction_pointer = (*state).instruction;
            crate::runtime::dispatch_fallback_context(context);
            *errno = saved_errno;
            (*state).return_stub
        }
    })
}

unsafe extern "C" {
    fn fallback_entry();
    fn fallback_return_template();
    static fallback_return_template_end: u8;
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
    mov rdi, r13
    xor esi, esi
    xor edx, edx
    call {clock_leave}
    mov r11, r14
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
    notrack jmp r11
    .global fallback_entry_end
    .hidden fallback_entry_end
fallback_entry_end:
    .size fallback_entry, .-fallback_entry

    .global fallback_return_template
    .hidden fallback_return_template
    .global fallback_return_template_end
    .hidden fallback_return_template_end
    .type fallback_return_template,@function
fallback_return_template:
    mov r11, [rsp - 88]
    popfq
    lea rsp, [rsp + 128]
    .byte 0x3e, 0xff, 0x25
    .long 0
fallback_return_template_end:
    .size fallback_return_template, .-fallback_return_template
    "#,
    save_bytes = sym SAVE_BYTES,
    save_mask = sym SAVE_MASK,
    dispatch = sym dispatch,
    clock_enter = sym crate::clock_control::reverie_liteinst_clock_enter,
    clock_leave = sym crate::clock_control::reverie_liteinst_clock_leave,
);
