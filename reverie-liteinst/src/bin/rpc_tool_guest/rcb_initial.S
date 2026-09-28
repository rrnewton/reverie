/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 * Licensed under the BSD-style license in the repository root LICENSE.
 */

.text

/* Each probe receives setup in RDI, its argument in RSI, and a prebuilt
 * syscall site in RDX. Entry RSP is 8 mod 16. Saving RBX aligns the call
 * into the real root activation boundary, which consumes setup before
 * enabling the supervisor event. No Rust executes between that return
 * and the probe's first syscall.
 */
.p2align 4
.global rcb_initial_zero
.hidden rcb_initial_zero
.type rcb_initial_zero,@function
rcb_initial_zero:
    endbr64
    push rbx
    mov rbx, rdx
    call {root_activation}
.global rcb_initial_zero_guest
.hidden rcb_initial_zero_guest
rcb_initial_zero_guest:
    mov eax, 39
    call rbx
    pop rbx
    ret
.size rcb_initial_zero,.-rcb_initial_zero

.p2align 4
.global rcb_initial_branches
.hidden rcb_initial_branches
.type rcb_initial_branches,@function
rcb_initial_branches:
    endbr64
    push rbx
    mov rbx, rdx
    call {root_activation}
.global rcb_initial_branches_guest
.hidden rcb_initial_branches_guest
rcb_initial_branches_guest:
    mov ecx, 4096
.Lrcb_initial_guest_loop:
    dec ecx
    jnz .Lrcb_initial_guest_loop
    mov eax, 39
    call rbx
    pop rbx
    ret
.size rcb_initial_branches,.-rcb_initial_branches

/* Root return has already enabled and published RUNNING. Send the private
 * fixed-record request and wait in a trusted syscall for the external
 * supervisor to send blocked SIGWINCH and runtime-owned SIGSYS. The latter
 * interrupts this wait only after traversing its physical entry fence. No
 * conditional guest branch is added to the frozen first-clock vector. */
.p2align 4
.global rcb_initial_signal
.hidden rcb_initial_signal
.type rcb_initial_signal,@function
rcb_initial_signal:
    endbr64
    push rbx
    push r12
    sub rsp,8
    mov rbx,rdx
    mov r12,rcx
    call {root_activation}
    mov qword ptr [rsp],0
    mov edi,{sendto}
    mov rsi,[r12]
    lea rdx,[r12+8]
    mov ecx,64
    xor r8d,r8d
    xor r9d,r9d
    call reverie_preload_trusted_syscall
    mov qword ptr [rsp],0
    mov edi,{recvfrom}
    mov rsi,[r12]
    lea rdx,[r12+8]
    mov ecx,64
    xor r8d,r8d
    xor r9d,r9d
    call reverie_preload_trusted_syscall
    mov eax,39
    call rbx
    mov qword ptr [rsp],rax
    /* A second, distinct syscall site asks Linux to widen affinity. Runtime
     * admission must return EPERM before forwarding it; the event therefore
     * remains bound to the singleton CPU selected before profile capture. */
    xor edi,edi
    mov esi,8
    lea rdx,[rip+.Lrcb_initial_wide_affinity]
    mov eax,{sched_setaffinity}
    syscall
    mov qword ptr [rip+{affinity_result}],rax
    mov rax,qword ptr [rsp]
    add rsp,8
    pop r12
    pop rbx
    ret
.size rcb_initial_signal,.-rcb_initial_signal

.section .rodata
.p2align 3
.Lrcb_initial_wide_affinity:
    .quad -1
.text

.p2align 4
.global rcb_initial_fork
.hidden rcb_initial_fork
.type rcb_initial_fork,@function
rcb_initial_fork:
    endbr64
    push rbx
    mov rbx, rdx
    call {root_activation}
.global rcb_initial_fork_guest
.hidden rcb_initial_fork_guest
rcb_initial_fork_guest:
    mov eax, 57
    call rbx
.global rcb_initial_fork_child_guest
.hidden rcb_initial_fork_child_guest
rcb_initial_fork_child_guest:
    /* The reconstructed child returns to this exact continuation. The
     * jnz retires once, then the loop retires 4096 conditional branches.
     * All runtime reconstruction and its lifecycle callback precede it.
     */
    test rax, rax
    jnz .Lrcb_initial_parent
    mov ecx, 4096
.Lrcb_initial_child_loop:
    dec ecx
    jnz .Lrcb_initial_child_loop
    mov eax, 39
    call rbx
    mov eax, 231
    xor edi, edi
    syscall
    ud2
.Lrcb_initial_parent:
    pop rbx
    ret
.size rcb_initial_fork,.-rcb_initial_fork

.p2align 4
.global rcb_initial_setup_branches
.hidden rcb_initial_setup_branches
.type rcb_initial_setup_branches,@function
rcb_initial_setup_branches:
    endbr64
    mov ecx, 4096
.Lrcb_initial_setup_loop:
    dec ecx
    jnz .Lrcb_initial_setup_loop
    ret
.size rcb_initial_setup_branches,.-rcb_initial_setup_branches

/* A preexisting handler queued by root begin. It contains no conditional branch,
 * so the original 64-branch first-callback vector remains unchanged. */
.p2align 4
.global rcb_initial_root_entry_signal_handler
.hidden rcb_initial_root_entry_signal_handler
.type rcb_initial_root_entry_signal_handler,@function
rcb_initial_root_entry_signal_handler:
    endbr64
    lock inc qword ptr [rip+{root_entry_signal_entries}]
    ret
.size rcb_initial_root_entry_signal_handler,.-rcb_initial_root_entry_signal_handler

/* The signal is queued only after runtime signal reset, while root assembly
 * still owns the blocked mask. Root activation enables and publishes RUNNING
 * before SIG_SETMASK can deliver this handler. The exact loop therefore enters
 * the first Tool callback's absolute clock if and only if physical enable
 * preceded mask restoration. Record.mode and Record.held are captured before
 * the loop without a conditional branch.
 */
.p2align 4
.global rcb_initial_signal_handler
.hidden rcb_initial_signal_handler
.type rcb_initial_signal_handler,@function
rcb_initial_signal_handler:
    endbr64
    mov qword ptr [rip+{signal_stack_pointer}], rsp
    call reverie_preload_rcb_record
    mov edx, dword ptr [rax+{mode_offset}]
    mov qword ptr [rip+{signal_mode}], rdx
    mov edx, dword ptr [rax+{held_offset}]
    mov qword ptr [rip+{signal_held}], rdx
    lock inc qword ptr [rip+{signal_entries}]
    mov ecx, {signal_branches}
.Lrcb_initial_signal_loop:
    dec ecx
    jnz .Lrcb_initial_signal_loop
    ret
.global rcb_initial_signal_handler_end
.hidden rcb_initial_signal_handler_end
rcb_initial_signal_handler_end:
.size rcb_initial_signal_handler,.-rcb_initial_signal_handler

/* The supervisor queues preblocked SIGWINCH while the target is RUNNING. The
 * fixture observes it pending with zero handler entries at callback entry and
 * releases it only while PAUSED. These branches add zero guest RCBs. */
.p2align 4
.global rcb_initial_window_signal_handler
.hidden rcb_initial_window_signal_handler
.type rcb_initial_window_signal_handler,@function
rcb_initial_window_signal_handler:
    endbr64
    call reverie_preload_rcb_record
    mov edx,dword ptr [rax+{mode_offset}]
    mov qword ptr [rip+{window_signal_mode}],rdx
    lock inc qword ptr [rip+{window_signal_entries}]
    mov ecx,{signal_branches}
.Lrcb_initial_window_signal_loop:
    dec ecx
    jnz .Lrcb_initial_window_signal_loop
    ret
.global rcb_initial_window_signal_handler_end
.hidden rcb_initial_window_signal_handler_end
rcb_initial_window_signal_handler_end:
.size rcb_initial_window_signal_handler,.-rcb_initial_window_signal_handler

.p2align 4
.global rcb_initial_signal_restorer
.hidden rcb_initial_signal_restorer
.type rcb_initial_signal_restorer,@function
rcb_initial_signal_restorer:
    mov eax, 15
    syscall
    ud2
.global rcb_initial_signal_restorer_end
.hidden rcb_initial_signal_restorer_end
rcb_initial_signal_restorer_end:
.size rcb_initial_signal_restorer,.-rcb_initial_signal_restorer

.section .note.GNU-stack,"",@progbits
