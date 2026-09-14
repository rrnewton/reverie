use core::arch::global_asm;
use core::sync::atomic::AtomicPtr;
use core::sync::atomic::Ordering;
use std::alloc::Layout;
use std::path::Path;

static CALLBACK_STATE: AtomicPtr<State> = AtomicPtr::new(std::ptr::null_mut());

#[repr(C)]
struct State {
    original: *mut u8,
    before: *mut u8,
    after: *mut u8,
    clear: *mut u8,
    mask: u64,
    site: *mut u8,
    omit_restore: u64,
}

struct Image {
    pointer: *mut u8,
    layout: Layout,
}

impl Image {
    fn new(bytes: usize) -> Self {
        let layout = Layout::from_size_align(bytes, 64).unwrap();
        let pointer = unsafe { std::alloc::alloc_zeroed(layout) };
        assert!(!pointer.is_null());
        Self { pointer, layout }
    }

    fn bytes(&self) -> &[u8] {
        unsafe { std::slice::from_raw_parts(self.pointer, self.layout.size()) }
    }
}

impl Drop for Image {
    fn drop(&mut self) {
        unsafe { std::alloc::dealloc(self.pointer, self.layout) };
    }
}

pub(super) fn clobber_callback_state() {
    let state = CALLBACK_STATE.load(Ordering::Relaxed);
    if !state.is_null() {
        // Called as an ordinary ABI function: vector registers are volatile.
        // The assembly resets all enabled xstate, including FP controls.
        unsafe { fallback_xstate_clobber(state) };
    }
}

pub(super) fn check_callback_environment() {
    if !CALLBACK_STATE.load(Ordering::Relaxed).is_null() {
        let mut control = 0u16;
        let mut mxcsr = 0u32;
        unsafe {
            core::arch::asm!(
                "fnstcw [{control}]",
                "stmxcsr [{mxcsr}]",
                control = in(reg) &mut control,
                mxcsr = in(reg) &mut mxcsr,
                options(nostack, preserves_flags),
            );
        }
        assert_eq!(control, 0x037f, "callback x87 environment");
        assert_eq!(mxcsr, 0x1f80, "callback MXCSR environment");
    }
}

pub(super) fn run(path: &Path) {
    let (site, native_pid) = super::prepare_site();
    let features = core::arch::x86_64::__cpuid(1);
    let (mask, bytes) = if features.ecx & (3 << 26) == 3 << 26 {
        (
            unsafe { core::arch::x86_64::_xgetbv(0) },
            core::arch::x86_64::__cpuid_count(0xd, 0).ebx as usize,
        )
    } else {
        (0, 512)
    };
    let original = Image::new(bytes);
    let before = Image::new(bytes);
    let after = Image::new(bytes);
    let clear = Image::new(bytes);
    // The legacy FXSAVE restore has no XSTATE_BV to request initial state.
    unsafe {
        clear.pointer.cast::<u16>().write(0x037f);
        clear.pointer.add(24).cast::<u32>().write(0x1f80);
    }
    let mut state = State {
        original: original.pointer,
        before: before.pointer,
        after: after.pointer,
        clear: clear.pointer,
        mask,
        site,
        omit_restore: 0,
    };

    // Native syscall: the seeded state must remain byte-exact.
    assert_eq!(
        unsafe { fallback_xstate_call(&state) },
        i64::from(native_pid)
    );
    assert_eq!(
        before.bytes(),
        after.bytes(),
        "native syscall changed xstate"
    );
    // Native omitted-restore control: the same real clobber used in the Tool
    // must be visible to this exact comparator, without runtime interception.
    state.omit_restore = 1;
    unsafe { fallback_xstate_call(&state) };
    assert_ne!(
        before.bytes(),
        after.bytes(),
        "state clobber was not observed"
    );
    assert_ne!(
        &before.bytes()[..32],
        &after.bytes()[..32],
        "FP control clobber"
    );
    assert_ne!(
        &before.bytes()[160..176],
        &after.bytes()[160..176],
        "XMM clobber"
    );
    for (bit, component) in [
        (2, "YMM"),
        (5, "opmask"),
        (6, "ZMM"),
        (7, "Hi16_ZMM"),
        (9, "PKRU"),
    ] {
        if mask & (1 << bit) != 0 {
            let layout = core::arch::x86_64::__cpuid_count(0xd, bit);
            let start = layout.ebx as usize;
            let end = start + layout.eax as usize;
            assert_ne!(
                &before.bytes()[start..end],
                &after.bytes()[start..end],
                "{component} clobber"
            );
        }
    }

    state.omit_restore = 0;
    CALLBACK_STATE.store(&mut state, Ordering::Relaxed);
    unsafe { reverie_liteinst::install_tool::<super::FallbackTool>(path) }.unwrap();
    assert_eq!(unsafe { fallback_xstate_call(&state) }, 424_242);
    assert_eq!(
        before.bytes(),
        after.bytes(),
        "Tool dispatch changed guest xstate"
    );
    assert_eq!(
        unsafe { std::slice::from_raw_parts(site, 3) },
        [0x0f, 0x05, 0xc3]
    );
    assert_eq!(
        reverie_liteinst::reverie_liteinst_site_hook_count(site as u64),
        0
    );
    assert_eq!(
        reverie_liteinst::reverie_liteinst_site_trap_count(site as u64),
        1
    );
    assert_eq!(super::super::LAST_TOTAL.load(Ordering::Relaxed), 1);
    assert_eq!(super::super::LAST_SENDERS.load(Ordering::Relaxed), 1);
    CALLBACK_STATE.store(std::ptr::null_mut(), Ordering::Relaxed);
    println!(
        "fallback xstate: mask={mask:#x} bytes={bytes} native=preserved clobber=detected tool=preserved"
    );
}

unsafe extern "C" {
    fn fallback_xstate_call(state: *const State) -> i64;
    fn fallback_xstate_clobber(state: *const State);
}

global_asm!(
    r#"
    .text
    .macro save_image field
    mov rdi, [r12 + \field]
    mov rax, [r12 + 32]
    test rax, rax
    jz 20f
    mov rdx, rax
    shr rdx, 32
    xsave64 [rdi]
    jmp 21f
20:
    fxsave64 [rdi]
21:
    .endm
    .macro restore_image field
    mov rdi, [r12 + \field]
    mov rax, [r12 + 32]
    test rax, rax
    jz 22f
    mov rdx, rax
    shr rdx, 32
    xrstor64 [rdi]
    jmp 23f
22:
    fxrstor64 [rdi]
23:
    .endm

    .global fallback_xstate_call
    .hidden fallback_xstate_call
    .type fallback_xstate_call,@function
fallback_xstate_call:
    push r12
    push r13
    sub rsp, 8
    mov r12, rdi
    save_image 0
    fninit
    fld1
    fldcw [rip + seed_cw]
    ldmxcsr [rip + seed_mxcsr]
    pcmpeqd xmm0, xmm0
    test qword ptr [r12 + 32], 4
    jz 1f
    vpcmpeqd ymm1, ymm1, ymm1
1:
    mov rax, [r12 + 32]
    and eax, 0xe0
    cmp eax, 0xe0
    jne 2f
    kxnorw k1, k1, k1
    vpternlogd zmm2, zmm2, zmm2, 0xff
    vpternlogd zmm16, zmm16, zmm16, 0xff
2:
    test qword ptr [r12 + 32], 512
    jz 3f
    mov eax, 0x55555554
    xor ecx, ecx
    xor edx, edx
    wrpkru
3:
    save_image 8
    cmp qword ptr [r12 + 48], 0
    je 4f
    mov rdi, r12
    call fallback_xstate_clobber
    jmp 5f
4:
    mov eax, 39
    mov edi, 11
    mov esi, 22
    mov edx, 33
    mov r10d, 44
    mov r8d, 55
    mov r9d, 66
    call [r12 + 40]
5:
    mov r13, rax
    save_image 16
    restore_image 0
    mov rax, r13
    add rsp, 8
    pop r13
    pop r12
    ret
    .size fallback_xstate_call, .-fallback_xstate_call

    .global fallback_xstate_clobber
    .hidden fallback_xstate_clobber
    .type fallback_xstate_clobber,@function
fallback_xstate_clobber:
    push r12
    mov r12, rdi
    restore_image 24
    pop r12
    ret
    .size fallback_xstate_clobber, .-fallback_xstate_clobber

    .section .rodata
seed_cw:
    .short 0x077f
seed_mxcsr:
    .long 0x3f80
    "#
);
