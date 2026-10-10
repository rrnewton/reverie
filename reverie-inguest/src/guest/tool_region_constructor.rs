/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! Scalar constructor placement only. No PKRU, mask or handler transition.

use core::mem::offset_of;
use core::sync::atomic::AtomicUsize;

use super::*;

pub(super) const STACK_BYTES: usize = 8 * 1024 * 1024;
pub(super) const STACK_PAGES: usize = STACK_BYTES / PAGE + 2;
pub(super) const STACK_BOTTOM: usize = BASE + (DATA_FIRST + 1) * PAGE;
pub(super) const STACK_TOP: usize = STACK_BOTTOM + STACK_BYTES;
pub(super) const STACK_EXTENT_END: usize = BASE + (DATA_FIRST + STACK_PAGES) * PAGE;
pub(super) const RECORD_ADDRESS: usize = BASE + PAGE + ((size_of::<Control>() + 15) & !15);
const VERSION: usize = 1;
const NATIVE: usize = 1;
const ADOPTING: usize = 2;
const READY: usize = 3;
pub(super) const RETURNED: usize = 4;

#[repr(C)]
struct NativeRecord {
    caller_rsp: usize,
    rbx: usize,
    rbp: usize,
    r12: usize,
    r13: usize,
    r14: usize,
    r15: usize,
    body: usize,
    owner_tid: usize,
    phase: AtomicUsize,
    version: usize,
    first_rust_sample_rsp: AtomicUsize,
    body_call_sample_rsp: AtomicUsize,
}

const _: () = {
    assert!(DATA_FIRST & 63 == 0);
    assert!(STACK_PAGES == 2050);
    assert!(size_of::<AtomicBool>() == 1);
    assert!(size_of::<AtomicUsize>() == size_of::<usize>());
    assert!(offset_of!(Control, held) == 0);
    assert!(RECORD_ADDRESS & (align_of::<NativeRecord>() - 1) == 0);
    assert!(RECORD_ADDRESS + size_of::<NativeRecord>() <= BASE + CONTROL_BYTES);
    assert!(STACK_TOP & 15 == 0);
};

/// Placement diagnostics for a successfully adopted native constructor stack.
/// These samples are taken inside the adoption function and immediately before
/// its body call. They do not substitute for observing the actual body entry.
#[derive(Clone, Copy, Debug)]
pub struct ConstructorStack {
    /// First page of the allocation, including its lower guard.
    pub extent_start: usize,
    /// End of the allocation, including its upper guard.
    pub extent_end: usize,
    /// Readable/writable key-zero interior.
    pub bottom: usize,
    /// End of that interior, followed by the upper guard.
    pub top: usize,
    /// Scalar caller stack address retained by the native entry.
    pub caller_rsp: usize,
    /// Kernel TID which made this fresh reservation.
    pub owner_tid: u32,
    /// Address of the first Rust adoption entry, for external observation.
    pub adoption_entry: usize,
    /// Address supplied by the unsafe native caller.
    pub body_entry: usize,
    /// RSP sampled after the adoption function's Rust prologue.
    pub first_rust_sample_rsp: usize,
    /// RSP sampled before the real C-ABI body call.
    pub body_call_sample_rsp: usize,
    /// Whether that body has returned normally.
    pub returned: bool,
}

/// Query an adopted bootstrap without allocating or initializing a region.
/// Ordinary reservations and inert compatibility loads have no bootstrap.
pub fn constructor_stack() -> Option<ConstructorStack> {
    let region = REGION.get()?.as_ref().ok()?;
    let address = region.bootstrap?;
    // SAFETY: only typed fresh adoption publishes this process-lifetime record.
    let record = unsafe { &*(address as *const NativeRecord) };
    let phase = record.phase.load(Ordering::Acquire);
    if phase != READY && phase != RETURNED {
        return None;
    }
    Some(ConstructorStack {
        extent_start: BASE + DATA_FIRST * PAGE,
        extent_end: STACK_EXTENT_END,
        bottom: STACK_BOTTOM,
        top: STACK_TOP,
        caller_rsp: record.caller_rsp,
        owner_tid: record.owner_tid as u32,
        adoption_entry: reverie_inguest_constructor_adopt_entry as *const () as usize,
        body_entry: record.body,
        first_rust_sample_rsp: record.first_rust_sample_rsp.load(Ordering::Acquire),
        body_call_sample_rsp: record.body_call_sample_rsp.load(Ordering::Acquire),
        returned: phase == RETURNED,
    })
}

/// Fixed scalar ABI for the separate constructor-placement diagnostic.
/// This never changes or substitutes for the allocator or region-stack oracle.
#[cfg(feature = "allocator-fixture")]
#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub struct ConstructorStackRecord {
    /// Version one of this sixteen-word scalar record.
    pub layout_version: u64,
    /// Three while ready and four after ordinary body return.
    pub phase: u64,
    /// Kernel TID of the fresh native reservation, retained through COW.
    pub owner_tid: u64,
    /// Beginning of the full extent, including its lower guard.
    pub extent_start: u64,
    /// End of the full extent, including its upper guard.
    pub extent_end: u64,
    /// Beginning of the writable bootstrap interior.
    pub bottom: u64,
    /// End of the writable interior and beginning of the upper guard.
    pub top: u64,
    /// Native caller's original RSP.
    pub caller_rsp: u64,
    /// Stable first Rust adoption entry address.
    pub adoption_entry: u64,
    /// Supplied actual C-ABI body entry address.
    pub body_entry: u64,
    /// Post-prologue RSP sampled inside Rust adoption.
    pub first_rust_sample_rsp: u64,
    /// RSP sampled immediately before the genuine body call.
    pub body_call_sample_rsp: u64,
    /// Address of the fresh region's existing control structure.
    pub control_address: u64,
    /// Address of this native invocation's private owned record.
    pub record_address: u64,
    /// Literal fixed reservation beginning.
    pub region_base: u64,
    /// Literal fixed reservation end.
    pub region_end: u64,
}

/// Write a read-only constructor placement snapshot. Return zero on success;
/// return -1 without writing for null output or an absent/unpublished owner.
///
/// # Safety
/// A non-null output must point to writable storage for one complete record.
#[cfg(feature = "allocator-fixture")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn reverie_inguest_constructor_stack_query(
    output: *mut ConstructorStackRecord,
) -> i32 {
    if output.is_null() {
        return -1;
    }
    let Some(stack) = constructor_stack() else {
        return -1;
    };
    let record = ConstructorStackRecord {
        layout_version: VERSION as u64,
        phase: if stack.returned { RETURNED } else { READY } as u64,
        owner_tid: u64::from(stack.owner_tid),
        extent_start: stack.extent_start as u64,
        extent_end: stack.extent_end as u64,
        bottom: stack.bottom as u64,
        top: stack.top as u64,
        caller_rsp: stack.caller_rsp as u64,
        adoption_entry: stack.adoption_entry as u64,
        body_entry: stack.body_entry as u64,
        first_rust_sample_rsp: stack.first_rust_sample_rsp as u64,
        body_call_sample_rsp: stack.body_call_sample_rsp as u64,
        control_address: (BASE + PAGE) as u64,
        record_address: RECORD_ADDRESS as u64,
        region_base: BASE as u64,
        region_end: END as u64,
    };
    unsafe { output.write(record) };
    0
}

unsafe extern "C" {
    #[link_name = "reverie_inguest_trusted_syscall_ip"]
    fn native_gate();
}

/// Reserve and adopt a fresh fixed guarded stack before the first Rust call.
///
/// Only the supplied zero-argument scalar body runs on this bootstrap. The
/// complete lease remains occupied for process lifetime and through plain fork
/// COW. Setup failure exits 127 without a guest-stack or allocator fallback.
/// This is key-zero placement, not protected interiors, whole caller-window
/// equality, standalone activation, TLS isolation or arbitrary state capture.
/// It adds no mask, disposition, CPUID, PKRU or seccomp policy.
///
/// # Safety
///
/// Call once per image during single-threaded trusted startup, with no active
/// runtime filter, using the scalar SysV ABI (including clear DF). The body must
/// be a valid C-ABI function which preserves its callee-saved state and does not
/// unwind across this stack switch. A loader root may tail-jump here with the
/// body in RDI, retaining its genuine CALL/RET pair. Earlier handlers retain
/// current behavior and may access the unprotected interior; no signal-frame
/// byte-isolation or denied-key access guarantee is supplied.
#[unsafe(naked)]
#[unsafe(export_name = "reverie_inguest_constructor_entry")]
pub unsafe extern "C" fn constructor_entry(_body: unsafe extern "C" fn()) {
    core::arch::naked_asm!(
        "endbr64",
        "movq xmm0, rsp",
        "movq xmm1, rbx",
        "movq xmm2, rbp",
        "movq xmm3, r12",
        "movq xmm4, r13",
        "movq xmm5, r14",
        "movq xmm6, r15",
        "movq xmm7, rdi",
        // A scalar BSS locator is explicitly outside the region claim. No
        // contender spins or enters OnceLock while this gate is occupied.
        "xor eax, eax",
        "mov dl, 1",
        "lock cmpxchg byte ptr [rip + {initializing}], dl",
        "jne .Lconstructor_fatal",
        "mov eax, {mmap}",
        "movabs rdi, {base}",
        "movabs rsi, {bytes}",
        "xor edx, edx",
        "mov r10d, {map_flags}",
        "mov r8, -1",
        "xor r9d, r9d",
        "lea r12, [rip + .Lconstructor_mapped]",
        "jmp {gate}",
        ".Lconstructor_mapped:",
        "endbr64",
        "test rax, rax",
        "js .Lconstructor_fatal",
        "movabs r13, {base}",
        "cmp rax, r13",
        "je .Lconstructor_exact",
        // Old kernels can ignore NOREPLACE. Clean only the actual return.
        "mov rdi, rax",
        "movabs rsi, {bytes}",
        "mov eax, {munmap}",
        "lea r12, [rip + .Lconstructor_wrong_unmapped]",
        "jmp {gate}",
        ".Lconstructor_wrong_unmapped:",
        "endbr64",
        "jmp .Lconstructor_fatal",
        ".Lconstructor_exact:",
        "movabs rdi, {control}",
        "mov esi, {control_bytes}",
        "mov edx, {rw}",
        "mov eax, {mprotect}",
        "lea r12, [rip + .Lconstructor_control]",
        "jmp {gate}",
        ".Lconstructor_control:",
        "endbr64",
        "test rax, rax",
        "jne .Lconstructor_fatal",
        "movabs r13, {control}",
        "mov byte ptr [r13 + {held}], 0",
        // Anonymous fresh ownership establishes zero-filled metadata. Reserve
        // all 2050 pages, including both guards, before exposing any Rust API.
        "lea rdi, [r13 + {bitmap} + {first_word}]",
        "mov ecx, {full_words}",
        ".Lconstructor_bitmap:",
        "mov qword ptr [rdi], -1",
        "add rdi, 8",
        "dec ecx",
        "jne .Lconstructor_bitmap",
        "mov qword ptr [rdi], {last_bits}",
        "movabs r14, {record}",
        "movq rax, xmm0",
        "mov [r14 + {caller_rsp}], rax",
        "movq rax, xmm1",
        "mov [r14 + {rbx}], rax",
        "movq rax, xmm2",
        "mov [r14 + {rbp}], rax",
        "movq rax, xmm3",
        "mov [r14 + {r12}], rax",
        "movq rax, xmm4",
        "mov [r14 + {r13}], rax",
        "movq rax, xmm5",
        "mov [r14 + {r14}], rax",
        "movq rax, xmm6",
        "mov [r14 + {r15}], rax",
        "movq rax, xmm7",
        "mov [r14 + {body}], rax",
        "mov qword ptr [r14 + {version}], {version_value}",
        "mov qword ptr [r14 + {phase}], {native}",
        "mov eax, {gettid}",
        "lea r12, [rip + .Lconstructor_tid]",
        "jmp {gate}",
        ".Lconstructor_tid:",
        "endbr64",
        "test rax, rax",
        "jle .Lconstructor_fatal",
        "mov [r14 + {owner_tid}], rax",
        "movabs rdi, {bottom}",
        "mov esi, {stack_bytes}",
        "mov edx, {rw}",
        "mov eax, {mprotect}",
        "lea r12, [rip + .Lconstructor_stack]",
        "jmp {gate}",
        ".Lconstructor_stack:",
        "endbr64",
        "test rax, rax",
        "jne .Lconstructor_fatal",
        "movabs rsp, {top}",
        "mov rdi, r14",
        "call {adopt}",
        // Reload only from owned control storage. No Tool access follows the
        // exact caller-RSP restoration, and RET discharges the loader CALL.
        "movabs rax, {record}",
        "mov rbx, [rax + {rbx}]",
        "mov rbp, [rax + {rbp}]",
        "mov r12, [rax + {r12}]",
        "mov r13, [rax + {r13}]",
        "mov r14, [rax + {r14}]",
        "mov r15, [rax + {r15}]",
        "mov rsp, [rax + {caller_rsp}]",
        "ret",
        ".Lconstructor_fatal:",
        "mov eax, {write}",
        "mov edi, 2",
        "lea rsi, [rip + .Lconstructor_message]",
        "mov edx, {message_bytes}",
        "lea r12, [rip + .Lconstructor_exit]",
        "jmp {gate}",
        ".Lconstructor_exit:",
        "endbr64",
        "mov eax, {exit_group}",
        "mov edi, 127",
        "lea r12, [rip + .Lconstructor_exit_failed]",
        "jmp {gate}",
        ".Lconstructor_exit_failed:",
        "endbr64",
        "ud2",
        ".Lconstructor_message:",
        ".ascii \"reverie constructor bootstrap failed\\n\"",
        initializing = sym INITIALIZING,
        gate = sym native_gate,
        adopt = sym reverie_inguest_constructor_adopt_entry,
        mmap = const libc::SYS_mmap,
        munmap = const libc::SYS_munmap,
        mprotect = const libc::SYS_mprotect,
        gettid = const libc::SYS_gettid,
        write = const libc::SYS_write,
        exit_group = const libc::SYS_exit_group,
        map_flags = const libc::MAP_PRIVATE | libc::MAP_ANONYMOUS | libc::MAP_FIXED_NOREPLACE,
        rw = const libc::PROT_READ | libc::PROT_WRITE,
        base = const BASE,
        bytes = const BYTES,
        control = const BASE + PAGE,
        control_bytes = const CONTROL_BYTES - PAGE,
        held = const offset_of!(Control, held),
        bitmap = const offset_of!(Control, occupied),
        first_word = const DATA_FIRST / 64 * size_of::<u64>(),
        full_words = const STACK_PAGES / 64,
        last_bits = const (1_u64 << (STACK_PAGES % 64)) - 1,
        record = const RECORD_ADDRESS,
        caller_rsp = const offset_of!(NativeRecord, caller_rsp),
        rbx = const offset_of!(NativeRecord, rbx),
        rbp = const offset_of!(NativeRecord, rbp),
        r12 = const offset_of!(NativeRecord, r12),
        r13 = const offset_of!(NativeRecord, r13),
        r14 = const offset_of!(NativeRecord, r14),
        r15 = const offset_of!(NativeRecord, r15),
        body = const offset_of!(NativeRecord, body),
        owner_tid = const offset_of!(NativeRecord, owner_tid),
        phase = const offset_of!(NativeRecord, phase),
        version = const offset_of!(NativeRecord, version),
        version_value = const VERSION,
        native = const NATIVE,
        bottom = const STACK_BOTTOM,
        top = const STACK_TOP,
        stack_bytes = const STACK_BYTES,
        message_bytes = const b"reverie constructor bootstrap failed\n".len(),
    );
}

// Keep a stable hidden entry for hardware observation of the first real Rust
// instruction. This is not another constructor or a public adoption API.
core::arch::global_asm!(
    ".hidden reverie_inguest_constructor_entry",
    ".hidden reverie_inguest_constructor_adopt_entry",
);

struct NativeFreshOwner {
    record: *mut NativeRecord,
}

impl NativeFreshOwner {
    // This constructor is private. Only the native prefix's successful exact
    // NOREPLACE mapping supplies provenance; record magic never supplies it.
    unsafe fn from_native(record: *mut NativeRecord, adoption_rsp: usize) -> Result<Self, i32> {
        if record as usize != RECORD_ADDRESS
            || !INITIALIZING.load(Ordering::Acquire)
            || !(STACK_BOTTOM..STACK_TOP).contains(&adoption_rsp)
        {
            return Err(libc::EINVAL);
        }
        let current = unsafe { raw_syscall6(libc::SYS_gettid, [0; 6]) };
        let record_ref = unsafe { &*record };
        if current <= 0
            || record_ref.owner_tid != current as usize
            || record_ref.version != VERSION
            || record_ref.body == 0
            || record_ref.caller_rsp % 16 != 8
            || REGION.get().is_some()
        {
            return Err(libc::EINVAL);
        }
        record_ref
            .phase
            .compare_exchange(NATIVE, ADOPTING, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| libc::EINVAL)?;
        let control = unsafe { &*((BASE + PAGE) as *const Control) };
        if control.held.load(Ordering::Acquire) {
            return Err(libc::EAGAIN);
        }
        let bitmap = unsafe { &*control.occupied.get() };
        if !(DATA_FIRST..DATA_FIRST + STACK_PAGES)
            .all(|page| bitmap[page / 64] & (1_u64 << (page % 64)) != 0)
        {
            return Err(libc::EINVAL);
        }
        Ok(Self { record })
    }

    fn publish(self) -> Result<&'static ToolRegion, i32> {
        REGION
            .set(Ok(ToolRegion {
                control: (BASE + PAGE) as *mut Control,
                bootstrap: Some(self.record as usize),
            }))
            .map_err(|_| libc::EAGAIN)?;
        unsafe { &(*self.record).phase }.store(READY, Ordering::Release);
        INITIALIZING.store(false, Ordering::Release);
        REGION
            .get()
            .and_then(|result| result.as_ref().ok())
            .ok_or(libc::EAGAIN)
    }
}

#[unsafe(no_mangle)]
#[inline(never)]
unsafe extern "C" fn reverie_inguest_constructor_adopt_entry(record: *mut NativeRecord) {
    let rsp: usize;
    unsafe { core::arch::asm!("mov {}, rsp", out(reg) rsp, options(nostack, preserves_flags)) };
    let fresh = unsafe { NativeFreshOwner::from_native(record, rsp) };
    let Ok(fresh) = fresh else { fatal() };
    unsafe { &(*record).first_rust_sample_rsp }.store(rsp, Ordering::Release);
    if fresh.publish().is_err() {
        fatal();
    }
    let body: unsafe extern "C" fn() = unsafe { core::mem::transmute((*record).body) };
    let call_rsp: usize;
    unsafe {
        core::arch::asm!("mov {}, rsp", out(reg) call_rsp, options(nostack, preserves_flags));
        (*record)
            .body_call_sample_rsp
            .store(call_rsp, Ordering::Release);
        body();
        (*record).phase.store(RETURNED, Ordering::Release);
    }
}

fn fatal() -> ! {
    let message = b"reverie constructor bootstrap failed\n";
    unsafe {
        raw_syscall6(
            libc::SYS_write,
            [2, message.as_ptr() as u64, message.len() as u64, 0, 0, 0],
        );
    }
    unsafe { raw_syscall6(libc::SYS_exit_group, [127, 0, 0, 0, 0, 0]) };
    unsafe { core::arch::asm!("ud2", options(noreturn)) }
}

#[cfg(test)]
pub(super) fn malformed_ticket_control() {
    // Real fresh control mapping, but deliberately malformed private tickets.
    // These reject-only controls are not native constructor execution credit.
    let region = ToolRegion::reserve_inner().unwrap();
    let _guard = InitializationGuard::acquire().unwrap();
    let tid = unsafe { raw_syscall6(libc::SYS_gettid, [0; 6]) } as usize;
    let pointer = RECORD_ADDRESS as *mut NativeRecord;
    let value = NativeRecord {
        caller_rsp: 8,
        rbx: 0,
        rbp: 0,
        r12: 0,
        r13: 0,
        r14: 0,
        r15: 0,
        body: malformed_ticket_control as *const () as usize,
        owner_tid: tid,
        phase: AtomicUsize::new(NATIVE),
        version: VERSION,
        first_rust_sample_rsp: AtomicUsize::new(0),
        body_call_sample_rsp: AtomicUsize::new(0),
    };
    unsafe { pointer.write(value) };
    let attempt = move || {
        unsafe { NativeFreshOwner::from_native(pointer, STACK_TOP - 16) }
            .err()
            .unwrap()
    };
    assert_eq!(
        unsafe {
            NativeFreshOwner::from_native((RECORD_ADDRESS + 8) as *mut NativeRecord, STACK_TOP - 16)
        }
        .err()
        .unwrap(),
        libc::EINVAL
    );
    assert_eq!(
        unsafe { NativeFreshOwner::from_native(pointer, 8) }
            .err()
            .unwrap(),
        libc::EINVAL
    );
    unsafe { (*pointer).owner_tid = tid + 1 };
    assert_eq!(attempt(), libc::EINVAL);
    unsafe {
        (*pointer).owner_tid = tid;
        (*pointer).version = VERSION + 1
    };
    assert_eq!(attempt(), libc::EINVAL);
    unsafe {
        (*pointer).version = VERSION;
        (*pointer).phase.store(RETURNED, Ordering::Release)
    };
    assert_eq!(attempt(), libc::EINVAL);
    unsafe { (*pointer).phase.store(NATIVE, Ordering::Release) };
    // The complete bootstrap extent is still unoccupied: refuse, not publish.
    assert_eq!(attempt(), libc::EINVAL);
    assert!(REGION.get().is_none());
    let start = region.claim(STACK_PAGES).unwrap();
    assert_eq!(start, DATA_FIRST);
    unsafe {
        (*pointer).phase.store(NATIVE, Ordering::Release);
        (*pointer).body = 0
    };
    assert_eq!(attempt(), libc::EINVAL);
    assert!(REGION.get().is_none());
    // Consume no legitimate owner and publish no partial failure.
    assert_eq!(unsafe { (*pointer).body }, 0);
}
