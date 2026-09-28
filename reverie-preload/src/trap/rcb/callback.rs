/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 * Licensed under the BSD-style license in the repository root LICENSE.
 */

//! Complete installed-callback boundaries, including nested and copied stacks.
//!
//! The assembly wrapper captures and blocks the exact incoming signal mask,
//! stops a running event before calling any Rust, physically enables it before
//! publishing RUNNING, and restores the mask only after publication. A stack
//! frame owns only that pause, never the fd. A nested callback borrows the pause.
//! Fork reconstruction binds every still-live copied frame to the child's new
//! generation before any frame can return; only the outermost frame can resume.

use core::arch::global_asm;
use core::mem::offset_of;
use core::mem::size_of;

use super::BROKEN;
use super::BUILDING;
use super::PAUSED;
use super::PauseToken;
use super::RUNNING;
use super::Record;
use super::UNAVAILABLE;

#[repr(C)]
struct Frame {
    previous: *mut Frame,
    owner: i32,
    owns_pause: u32,
    generation: u64,
    pause: u64,
}

const _: () = {
    assert!(size_of::<Frame>() == 32);
    assert!(offset_of!(Frame, previous) == 0);
    assert!(offset_of!(Frame, owner) == 8);
    assert!(offset_of!(Frame, owns_pause) == 12);
    assert!(offset_of!(Frame, generation) == 16);
    assert!(offset_of!(Frame, pause) == 24);
};

unsafe extern "C" {
    #[cfg(feature = "rcb-qualification")]
    static reverie_preload_rcb_callback_end: u8;

    /// Call a generated hook's body within the complete physical RCB boundary.
    ///
    /// # Safety
    /// The body and context obey the installed hook ABI, return normally to this
    /// activation, and cannot outlive its stack. The saved guest image is owned
    /// by the caller's generated trampoline. Only admitted ordinary fork may
    /// copy the activation, and it must perform the existing child rebind.
    /// Mode 1 is reserved for the ptrace host callback: only with no private
    /// event may that callback run without private clock bookkeeping. Other
    /// modes retain the ordinary boundary; the generated table binds the mode.
    fn reverie_preload_rcb_callback(
        context: *mut libc::c_void,
        body: unsafe extern "C" fn(*mut libc::c_void),
        mode: u32,
    );
}

/// Addresses of this loaded assembly boundary for emitted-code inspection.
#[cfg(feature = "rcb-qualification")]
#[doc(hidden)]
pub(super) fn boundary() -> (u64, usize) {
    let start = reverie_preload_rcb_callback as *const () as usize;
    let end = core::ptr::addr_of!(reverie_preload_rcb_callback_end) as usize;
    (
        start as u64,
        end.checked_sub(start)
            .expect("RCB callback end follows entry"),
    )
}

/// The current ordinary stack still has an installed callback to return from.
pub(super) fn active() -> bool {
    unsafe { (*super::record()).callback_top != 0 }
}

/// Rebind the still-live, assembly-owned stack chain without retaining a Rust
/// reference across callbacks. Setup alone may invalidate inherited permissions.
/// Every successful registration updates the copied generation before return.
pub(super) fn rebind(state: &mut Record, generation: u64, pause: u64) -> Result<(), i32> {
    let mut pointer = state.callback_top as *mut Frame;
    for _ in 0..state.callback_depth {
        if pointer.is_null() {
            return Err(libc::ESTALE);
        }
        // Only wrapper entry publishes these live stack addresses; the chain
        // remains live across an admitted fork because it is copied with RSP.
        let frame = unsafe { &mut *pointer };
        frame.owner = state.owner;
        frame.generation = generation;
        frame.pause = pause;
        frame.owns_pause = 0;
        pointer = frame.previous;
    }
    if !pointer.is_null() {
        return Err(libc::ESTALE);
    }
    Ok(())
}

/// Rebind an admitted built-in/compatibility fork that has no counter at all.
/// Active, held, incomplete, or failed clocks cannot use this path. An actual
/// boundary error terminates the child rather than fabricating a fork failure.
pub(super) fn rebind_unavailable_fork_child() {
    let result = super::owner()
        .and_then(|tid| rebind_unavailable_with(unsafe { &mut *super::record() }, tid));
    if let Err(errno) = result {
        unsafe { super::failed(-i64::from(errno)) }
    }
}

fn rebind_unavailable_with(state: &mut Record, tid: i32) -> Result<(), i32> {
    if tid <= 0
        || state.mode != UNAVAILABLE
        || state.fd != -1
        || state.entry_owned != 0
        || state.held != 0
        || state.release != 0
    {
        return Err(libc::ESTALE);
    }
    let mut pointer = state.callback_top as *mut Frame;
    for _ in 0..state.callback_depth {
        if pointer.is_null() {
            return Err(libc::ESTALE);
        }
        let frame = unsafe { &*pointer };
        if frame.owner != state.owner
            || frame.generation != state.generation
            || frame.pause != state.pause_serial
            || frame.owns_pause != 0
        {
            return Err(libc::ESTALE);
        }
        pointer = frame.previous;
    }
    if !pointer.is_null() {
        return Err(libc::ESTALE);
    }
    state.owner = tid;
    let generation = state.generation;
    let pause = state.pause_serial;
    rebind(state, generation, pause)
}

/// Give a newly registered child pause to its outermost installed activation.
/// A held fallback continuation instead keeps its own token and never calls this.
pub(super) fn adopt_pause(token: PauseToken) -> Result<(), i32> {
    let tid = super::owner()?;
    adopt(unsafe { &mut *super::record() }, token, tid)
}

fn adopt(state: &mut Record, token: PauseToken, tid: i32) -> Result<(), i32> {
    if state.mode != PAUSED
        || state.owner != tid
        || token.owner != tid
        || token.generation != state.generation
        || token.pause != state.pause_serial
        || state.held != 1
        || state.release != 0
        || state.entry_owned != 0
        || state.fd < 0
        || state.callback_depth == 0
    {
        return Err(libc::ESTALE);
    }
    let mut pointer = state.callback_top as *mut Frame;
    let mut outermost = core::ptr::null_mut();
    for _ in 0..state.callback_depth {
        if pointer.is_null() {
            return Err(libc::ESTALE);
        }
        let frame = unsafe { &*pointer };
        if frame.owner != tid
            || frame.generation != state.generation
            || frame.pause != state.pause_serial
            || frame.owns_pause != 0
        {
            return Err(libc::ESTALE);
        }
        outermost = pointer;
        pointer = frame.previous;
    }
    if !pointer.is_null() || outermost.is_null() {
        return Err(libc::ESTALE);
    }
    unsafe { (*outermost).owns_pause = 1 };
    state.held = 0;
    Ok(())
}

unsafe extern "C" fn begin(pointer: *mut Frame) -> i64 {
    let result = super::owner().and_then(|tid| {
        // No borrow of Record or Frame crosses the indirect body call.
        begin_with(unsafe { &mut *super::record() }, pointer, tid)
    });
    match result {
        Ok(()) => 0,
        Err(errno) => -i64::from(errno),
    }
}

fn begin_with(state: &mut Record, pointer: *mut Frame, tid: i32) -> Result<(), i32> {
    if pointer.is_null() || tid <= 0 {
        return Err(libc::EINVAL);
    }
    // A shared built-in can install hooks without registering an RCB event.
    // Bind its first absent-clock activation to the actual current thread too.
    if state.mode == UNAVAILABLE && state.owner == 0 && state.callback_top == 0 {
        state.owner = tid;
    }
    if state.owner != tid || !matches!(state.mode, UNAVAILABLE | BUILDING | PAUSED) {
        return Err(libc::ESTALE);
    }
    let depth = state.callback_depth.checked_add(1).ok_or(libc::EOVERFLOW)?;
    let frame = unsafe { &mut *pointer };
    if frame.owns_pause > 1
        || (frame.owns_pause == 1
            && (state.mode != PAUSED
                || state.fd < 0
                || state.held != 0
                || state.release != 0
                || state.callback_top != 0))
    {
        return Err(libc::ESTALE);
    }
    frame.previous = state.callback_top as *mut Frame;
    frame.owner = tid;
    frame.generation = state.generation;
    frame.pause = state.pause_serial;
    state.callback_top = pointer as usize;
    state.callback_depth = depth;
    Ok(())
}

unsafe extern "C" fn finish(pointer: *mut Frame) -> i64 {
    let result = super::owner().and_then(|tid| {
        let state = unsafe { &mut *super::record() };
        let action = finish_with(state, pointer, tid)?;
        if action > 0 {
            super::validate_cpu(state)?;
        }
        Ok(action)
    });
    match result {
        Ok(action) => action,
        Err(errno) => -i64::from(errno),
    }
}

fn finish_with(state: &mut Record, pointer: *mut Frame, tid: i32) -> Result<i64, i32> {
    if state.mode == BROKEN {
        return Err(state
            .error
            .checked_neg()
            .and_then(|value| i32::try_from(value).ok())
            .filter(|value| *value > 0)
            .unwrap_or(libc::EIO));
    }
    if pointer.is_null()
        || state.callback_top != pointer as usize
        || state.callback_depth == 0
        || state.owner != tid
    {
        return Err(libc::ESTALE);
    }
    let frame = unsafe { &mut *pointer };
    if frame.owner != tid
        || frame.generation != state.generation
        || frame.pause != state.pause_serial
        || frame.owns_pause > 1
        || !matches!(state.mode, UNAVAILABLE | BUILDING | PAUSED)
    {
        return Err(libc::ESTALE);
    }
    let action = if frame.owns_pause == 1 {
        if state.mode != PAUSED
            || state.fd < 0
            || state.held != 0
            || state.release != 0
            || !frame.previous.is_null()
            || state.callback_depth != 1
        {
            return Err(libc::ESTALE);
        }
        i64::from(state.fd) + 1
    } else {
        0
    };
    state.callback_top = frame.previous as usize;
    state.callback_depth -= 1;
    frame.owns_pause = 0;
    // A positive action leaves PAUSED published until assembly has physically
    // enabled the event. Signals stay blocked across that operation and the
    // immediately following RUNNING publication.
    Ok(action)
}

global_asm!(r#"
    .text
    .p2align 4
    .global reverie_preload_rcb_callback
    .hidden reverie_preload_rcb_callback
    .type reverie_preload_rcb_callback,@function
reverie_preload_rcb_callback:
    endbr64
    push r12
    push r13
    push r14
    push r15
    // Entry RSP is 8 mod 16. Frame[0..32], context, body, mode, exact
    // incoming mask, all-signals input, and padding.
    sub rsp,88
    mov qword ptr [rsp],0
    mov dword ptr [rsp+12],0
    mov [rsp+32],rdi
    mov [rsp+40],rsi
    mov [rsp+48],edx
    // Reach the initial-exec record and issue DISABLE before the first retired
    // conditional branch. Calls, straight-line setup, SETcc, CMOV and the final
    // indirect jump below do not retire conditional branches. If a RUNNING
    // event cannot be disabled, exit without executing a conditional branch.
    call reverie_preload_rcb_record
    mov r12,rax
    mov edi,{ioctl}
    movsxd rsi,dword ptr [r12]
    mov edx,{disable}
    xor ecx,ecx
    xor r8d,r8d
    xor r9d,r9d
    call reverie_preload_trusted_syscall
    mov r15,rax
    cmp dword ptr [r12+16],{running}
    sete al
    test r15,r15
    setne dl
    and al,dl
    lea r13,[rip+.Lcallback_block_signals]
    lea r14,[rip+.Lcallback_early_disable_fatal]
    test al,al
    cmovne r13,r14
    jmp r13
.Lcallback_early_disable_fatal:
    mov edi,{exit_group}
    mov esi,125
    xor edx,edx
    xor ecx,ecx
    xor r8d,r8d
    xor r9d,r9d
    call reverie_preload_trusted_syscall
    ud2
    .global reverie_preload_rcb_callback_block_signals
    .hidden reverie_preload_rcb_callback_block_signals
reverie_preload_rcb_callback_block_signals:
.Lcallback_block_signals:
    mov qword ptr [rsp+64],-1
    mov edi,{sigprocmask}
    mov esi,{block}
    lea rdx,[rsp+64]
    lea rcx,[rsp+56]
    mov r8d,8
    xor r9d,r9d
    call reverie_preload_trusted_syscall
    mov rdi,rax
    lea r13,[rip+.Lcallback_initial_mask_error]
    lea r14,[rip+.Lcallback_signals_blocked]
    test rax,rax
    cmovz r13,r14
    jmp r13
    .global reverie_preload_rcb_callback_initial_mask_error
    .hidden reverie_preload_rcb_callback_initial_mask_error
reverie_preload_rcb_callback_initial_mask_error:
.Lcallback_initial_mask_error:
    mov edi,{exit_group}
    mov esi,125
    xor edx,edx
    xor ecx,ecx
    xor r8d,r8d
    xor r9d,r9d
    call reverie_preload_trusted_syscall
    ud2
    .global reverie_preload_rcb_callback_signals_blocked
    .hidden reverie_preload_rcb_callback_signals_blocked
reverie_preload_rcb_callback_signals_blocked:
.Lcallback_signals_blocked:
    cld
    .global reverie_preload_rcb_callback_select
    .hidden reverie_preload_rcb_callback_select
reverie_preload_rcb_callback_select:
    lea r13,[rip+.Lcallback_mode]
    lea r14,[rip+.Lcallback_host_absent]
    cmp dword ptr [r12+16],{unavailable}
    cmovne r14,r13
    cmp dword ptr [rsp+48],1
    cmove r13,r14
    jmp r13
    .global reverie_preload_rcb_callback_mode
    .hidden reverie_preload_rcb_callback_mode
reverie_preload_rcb_callback_mode:
.Lcallback_mode:
    lea r13,[rip+.Lcallback_begin]
    lea r14,[rip+.Lcallback_owner]
    cmp dword ptr [r12+16],{running}
    cmove r13,r14
    jmp r13
.Lcallback_owner:
    mov edi,{gettid}
    xor esi,esi
    xor edx,edx
    xor ecx,ecx
    xor r8d,r8d
    xor r9d,r9d
    call reverie_preload_trusted_syscall
    add qword ptr [r12+64],1
    lea r13,[rip+.Lcallback_owner_error]
    lea r14,[rip+.Lcallback_disable]
    cmp eax,dword ptr [r12+4]
    cmove r13,r14
    jmp r13
.Lcallback_owner_error:
    mov rdi,-{stale}
    test rax,rax
    cmovs rdi,rax
    call {failed}
    ud2
    .global reverie_preload_rcb_callback_disable
    .hidden reverie_preload_rcb_callback_disable
reverie_preload_rcb_callback_disable:
.Lcallback_disable:
    // The entry fence already performed the one physical DISABLE. The owner
    // path can reach here only with its saved result equal to zero. Reject a
    // migrated target before Tool code can produce an externally visible side
    // effect; finish validates the same CPU again immediately before ENABLE.
    mov dword ptr [rsp+72],-1
    mov edi,{getcpu}
    lea rsi,[rsp+72]
    xor edx,edx
    xor ecx,ecx
    xor r8d,r8d
    xor r9d,r9d
    call reverie_preload_trusted_syscall
    mov rdi,-{stale}
    test rax,rax
    cmovs rdi,rax
    lea r13,[rip+.Lcallback_error]
    lea r14,[rip+.Lcallback_cpu]
    test rax,rax
    cmovz r13,r14
    jmp r13
.Lcallback_cpu:
    mov eax,dword ptr [rsp+72]
    lea r13,[rip+.Lcallback_error]
    lea r14,[rip+.Lcallback_disabled]
    cmp eax,dword ptr [r12+{cpu_offset}]
    cmove r13,r14
    jmp r13
.Lcallback_error:
    call {failed}
    ud2
    .global reverie_preload_rcb_callback_disabled
    .hidden reverie_preload_rcb_callback_disabled
reverie_preload_rcb_callback_disabled:
.Lcallback_disabled:
    mov dword ptr [r12+16],{paused}
    mov dword ptr [rsp+12],1
    add qword ptr [r12+48],1
    add qword ptr [r12+40],1
    lea r13,[rip+.Lcallback_begin]
    lea r14,[rip+.Lcallback_serial_error]
    cmp qword ptr [r12+40],0
    cmove r13,r14
    jmp r13
.Lcallback_serial_error:
    mov rdi,-{overflow}
    call {failed}
    ud2
    // The ptrace host controls its own counter. With no private event, avoid
    // gettid syscalls that ptrace could interpret and patch as new guest sites.
    // No Record or live Frame field is published/mutated on this path. A COW
    // child's inherited absent-owner value is consequently irrelevant here.
    .global reverie_preload_rcb_callback_host_absent
    .hidden reverie_preload_rcb_callback_host_absent
reverie_preload_rcb_callback_host_absent:
.Lcallback_host_absent:
    mov rdi,[rsp+32]
    call qword ptr [rsp+40]
    jmp .Lcallback_restore_mask
    .global reverie_preload_rcb_callback_host_absent_end
    .hidden reverie_preload_rcb_callback_host_absent_end
reverie_preload_rcb_callback_host_absent_end:
    .global reverie_preload_rcb_callback_begin
    .hidden reverie_preload_rcb_callback_begin
reverie_preload_rcb_callback_begin:
.Lcallback_begin:
    mov rdi,rsp
    call {begin}
    mov rdi,rax
    lea r13,[rip+.Lcallback_error]
    lea r14,[rip+.Lcallback_body]
    test rax,rax
    cmovz r13,r14
    jmp r13
    .global reverie_preload_rcb_callback_body
    .hidden reverie_preload_rcb_callback_body
reverie_preload_rcb_callback_body:
.Lcallback_body:
    mov rdi,[rsp+32]
    call qword ptr [rsp+40]
    mov rdi,rsp
    call {finish}
    lea r13,[rip+.Lcallback_restore_mask]
    lea r14,[rip+.Lcallback_enable]
    test rax,rax
    cmovg r13,r14
    lea r14,[rip+.Lcallback_action_error]
    cmovs r13,r14
    jmp r13
.Lcallback_action_error:
    mov rdi,rax
    call {failed}
    ud2
    .global reverie_preload_rcb_callback_enable
    .hidden reverie_preload_rcb_callback_enable
reverie_preload_rcb_callback_enable:
.Lcallback_enable:
    mov qword ptr [rsp],0
    lea rsi,[rax-1]
    mov edi,{ioctl}
    mov edx,{enable}
    xor ecx,ecx
    xor r8d,r8d
    xor r9d,r9d
    call reverie_preload_trusted_syscall
    mov rdi,rax
    lea r13,[rip+.Lcallback_error]
    lea r14,[rip+.Lcallback_enabled]
    test rax,rax
    cmovz r13,r14
    jmp r13
    .global reverie_preload_rcb_callback_enabled
    .hidden reverie_preload_rcb_callback_enabled
reverie_preload_rcb_callback_enabled:
.Lcallback_enabled:
    mov dword ptr [r12+16],{running}
    add qword ptr [r12+56],1
    .global reverie_preload_rcb_callback_published
    .hidden reverie_preload_rcb_callback_published
reverie_preload_rcb_callback_published:
    .global reverie_preload_rcb_callback_restore_mask
    .hidden reverie_preload_rcb_callback_restore_mask
reverie_preload_rcb_callback_restore_mask:
.Lcallback_restore_mask:
    mov edi,{sigprocmask}
    mov esi,{setmask}
    lea rdx,[rsp+56]
    xor ecx,ecx
    mov r8d,8
    xor r9d,r9d
    call reverie_preload_trusted_syscall
    mov rdi,rax
    lea r13,[rip+.Lcallback_mask_error]
    lea r14,[rip+.Lcallback_mask_restored]
    test rax,rax
    cmovz r13,r14
    jmp r13
    .global reverie_preload_rcb_callback_mask_error
    .hidden reverie_preload_rcb_callback_mask_error
reverie_preload_rcb_callback_mask_error:
.Lcallback_mask_error:
    call {failed}
    ud2
    .global reverie_preload_rcb_callback_mask_restored
    .hidden reverie_preload_rcb_callback_mask_restored
reverie_preload_rcb_callback_mask_restored:
.Lcallback_mask_restored:
    .global reverie_preload_rcb_callback_return
    .hidden reverie_preload_rcb_callback_return
reverie_preload_rcb_callback_return:
.Lcallback_return:
    add rsp,88
    pop r15
    pop r14
    pop r13
    pop r12
    ret
    .global reverie_preload_rcb_callback_end
    .hidden reverie_preload_rcb_callback_end
reverie_preload_rcb_callback_end:
    .size reverie_preload_rcb_callback,.-reverie_preload_rcb_callback
"#,
    unavailable = const UNAVAILABLE,
    running = const RUNNING,
    paused = const PAUSED,
    gettid = const libc::SYS_gettid,
    getcpu = const libc::SYS_getcpu,
    ioctl = const libc::SYS_ioctl,
    enable = const 0x2400u32,
    disable = const 0x2401u32,
    sigprocmask = const libc::SYS_rt_sigprocmask,
    block = const libc::SIG_BLOCK,
    setmask = const libc::SIG_SETMASK,
    exit_group = const libc::SYS_exit_group,
    stale = const libc::ESTALE,
    cpu_offset = const super::CPU_OFFSET,
    overflow = const libc::EOVERFLOW,
    failed = sym super::failed,
    begin = sym begin,
    finish = sym finish,
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rcb_accounting_error_is_terminal_across_callback_and_fork() {
        let mut state = state();
        let mut outer = frame(1);
        begin_with(&mut state, &raw mut outer, 100).unwrap();
        super::super::accounting_failed_with(&mut state, libc::EOVERFLOW);
        assert_eq!(state.mode, BROKEN);
        assert_eq!(state.fd, 47, "terminal ownership must retain cleanup FD");
        assert_eq!(super::super::error_from(&state), libc::EOVERFLOW);
        assert_eq!(
            finish_with(&mut state, &raw mut outer, 100),
            Err(libc::EOVERFLOW),
            "a callback must not manufacture an enable action"
        );

        let before = (
            state.fd,
            state.owner,
            state.generation,
            state.mode,
            state.entry_owned,
            state.held,
            state.release,
            state.error,
            state.callback_top,
            state.callback_depth,
        );
        assert_eq!(
            super::super::begin_setup_with(&mut state, 200),
            Err(libc::EOVERFLOW)
        );
        assert_eq!(
            (
                state.fd,
                state.owner,
                state.generation,
                state.mode,
                state.entry_owned,
                state.held,
                state.release,
                state.error,
                state.callback_top,
                state.callback_depth,
            ),
            before,
            "fork setup cleared terminal state"
        );
        super::super::accounting_failed_with(&mut state, libc::ESTALE);
        assert_eq!(
            super::super::error_from(&state),
            libc::EOVERFLOW,
            "the first terminal error must remain stable"
        );
    }

    fn state() -> Record {
        // Every field is an integer. These tests exercise permission transitions;
        // no fake fd is passed to an ioctl and no PMU result is claimed.
        let mut state: Record = unsafe { core::mem::zeroed() };
        state.fd = 47;
        state.owner = 100;
        state.generation = 9;
        state.pause_serial = 3;
        state.mode = PAUSED;
        state
    }

    fn frame(owns_pause: u32) -> Frame {
        Frame {
            previous: core::ptr::null_mut(),
            owner: 0,
            owns_pause,
            generation: 0,
            pause: 0,
        }
    }

    #[test]
    fn nested_callback_cannot_release_outer_pause_or_return_out_of_order() {
        let mut state = state();
        let mut outer = frame(1);
        let mut inner = frame(0);
        begin_with(&mut state, &raw mut outer, 100).unwrap();
        begin_with(&mut state, &raw mut inner, 100).unwrap();
        assert_eq!(
            finish_with(&mut state, &raw mut outer, 100),
            Err(libc::ESTALE)
        );
        assert_eq!(state.mode, PAUSED);
        assert_eq!(state.callback_depth, 2);
        assert_eq!(finish_with(&mut state, &raw mut inner, 100), Ok(0));
        assert_eq!(state.mode, PAUSED);
        assert_eq!(state.callback_top, (&raw mut outer) as usize);
        assert_eq!(finish_with(&mut state, &raw mut outer, 100), Ok(48));
        assert_eq!(state.mode, PAUSED, "assembly has not enabled or published yet");
        assert_eq!(state.callback_depth, 0);
        assert_eq!(state.callback_top, 0);
        assert_eq!(
            finish_with(&mut state, &raw mut outer, 100),
            Err(libc::ESTALE)
        );
    }

    #[test]
    fn installed_callbacks_borrow_a_held_signal_pause_without_enabling() {
        let mut state = state();
        state.held = 1;
        let mut outer = frame(0);
        let mut inner = frame(0);
        begin_with(&mut state, &raw mut outer, 100).unwrap();
        begin_with(&mut state, &raw mut inner, 100).unwrap();
        assert_eq!(finish_with(&mut state, &raw mut inner, 100), Ok(0));
        assert_eq!(finish_with(&mut state, &raw mut outer, 100), Ok(0));
        assert_eq!(state.mode, PAUSED);
        assert_eq!(state.held, 1);
        assert_eq!(state.release, 0);
        assert_eq!(state.pause_serial, 3);
    }

    #[test]
    fn stale_owner_generation_and_pause_never_produce_an_enable_action() {
        super::super::assert_root_release_ownership();
        for changed in 0..3 {
            let mut state = state();
            let mut outer = frame(1);
            begin_with(&mut state, &raw mut outer, 100).unwrap();
            match changed {
                0 => state.owner = 200,
                1 => state.generation = 10,
                2 => state.pause_serial = 4,
                _ => unreachable!(),
            }
            assert_eq!(
                finish_with(&mut state, &raw mut outer, 100),
                Err(libc::ESTALE)
            );
            assert_eq!(state.mode, PAUSED);
            assert_eq!(state.callback_top, (&raw mut outer) as usize);
            assert_eq!(outer.owns_pause, 1);
        }
    }

    #[test]
    fn absent_clock_fork_rebinds_only_absent_borrowed_frames() {
        let mut state = state();
        state.fd = -1;
        state.mode = UNAVAILABLE;
        let mut outer = frame(0);
        let mut inner = frame(0);
        begin_with(&mut state, &raw mut outer, 100).unwrap();
        begin_with(&mut state, &raw mut inner, 100).unwrap();
        assert_eq!(
            finish_with(&mut state, &raw mut inner, 200),
            Err(libc::ESTALE)
        );
        for mode in [RUNNING, PAUSED, BUILDING, BROKEN] {
            state.mode = mode;
            assert_eq!(rebind_unavailable_with(&mut state, 200), Err(libc::ESTALE));
            assert_eq!(state.owner, 100);
        }
        state.mode = UNAVAILABLE;
        state.fd = 47;
        assert_eq!(rebind_unavailable_with(&mut state, 200), Err(libc::ESTALE));
        state.fd = -1;
        outer.owns_pause = 1;
        assert_eq!(rebind_unavailable_with(&mut state, 200), Err(libc::ESTALE));
        outer.owns_pause = 0;
        rebind_unavailable_with(&mut state, 200).unwrap();
        assert_eq!(finish_with(&mut state, &raw mut inner, 200), Ok(0));
        assert_eq!(finish_with(&mut state, &raw mut outer, 200), Ok(0));
        assert_eq!(state.mode, UNAVAILABLE);
        assert_eq!(state.fd, -1);
        assert_eq!(state.callback_top, 0);
    }

    #[test]
    fn copied_child_stack_requires_new_permission_even_when_fd_number_is_reused() {
        let mut state = state();
        let mut outer = frame(1);
        let mut inner = frame(0);
        begin_with(&mut state, &raw mut outer, 100).unwrap();
        begin_with(&mut state, &raw mut inner, 100).unwrap();
        // Ordinary child reconstruction revokes the parent's permission first.
        state.mode = BUILDING;
        state.fd = -1;
        state.owner = 200;
        rebind(&mut state, 9, 3).unwrap();
        assert_eq!(outer.owns_pause, 0);
        assert_eq!(inner.owns_pause, 0);
        // The new authenticated event can reuse the numeric slot. Generation
        // and pause identity, not numeric equality, grant the eventual action.
        rebind(&mut state, 10, 4).unwrap();
        state.fd = 47;
        state.generation = 10;
        state.pause_serial = 4;
        state.mode = PAUSED;
        state.held = 1;
        let parent = PauseToken {
            owner: 100,
            generation: 9,
            pause: 3,
        };
        assert_eq!(adopt(&mut state, parent, 200), Err(libc::ESTALE));
        assert_eq!(outer.owns_pause, 0);
        let child = PauseToken {
            owner: 200,
            generation: 10,
            pause: 4,
        };
        adopt(&mut state, child, 200).unwrap();
        assert_eq!(outer.owns_pause, 1);
        assert_eq!(inner.owns_pause, 0);
        assert_eq!(finish_with(&mut state, &raw mut inner, 200), Ok(0));
        assert_eq!(state.mode, PAUSED);
        assert_eq!(
            finish_with(&mut state, &raw mut outer, 100),
            Err(libc::ESTALE)
        );
        assert_eq!(finish_with(&mut state, &raw mut outer, 200), Ok(48));
    }
}
