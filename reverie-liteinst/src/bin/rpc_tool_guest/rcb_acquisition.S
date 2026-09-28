/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 * Licensed under the BSD-style license in the repository root LICENSE.
 *
 * The only conditional branch in the measured interval is the 4096-trip JNZ.
 * Both inactive-event and live RDPMC paths are selected with CMOV and JMP.
 * Rust validates both real metadata snapshots after the measured interval.
 */
.text
.globl rcb_acquisition_operation
.hidden rcb_acquisition_operation
.type rcb_acquisition_operation,@function
rcb_acquisition_operation:
    push %rbp
    push %rbx
    push %r12
    push %r13
    push %r14
    push %r15
    sub $8,%rsp
    mov %rdi,%r14
    mov 0(%r14),%r12
    mov 8(%r14),%r13
    call *24(%r14)
    mov %rax,144(%r14)
    mov $157,%eax
    mov 16(%r14),%rdi
    xor %esi,%esi
    xor %edx,%edx
    xor %r10d,%r10d
    xor %r8d,%r8d
    xor %r9d,%r9d
    call *%r12
    mov %rax,32(%r14)
    mov 8(%r13),%eax
    mov %rax,40(%r14)
    mov 12(%r13),%ecx
    mov %rcx,48(%r14)
    movzwq 48(%r13),%rax
    mov %rax,56(%r14)
    mov 16(%r13),%rax
    mov %rax,72(%r14)
    mov 40(%r13),%rax
    mov %rax,128(%r14)
    lea .Lfirst_live(%rip),%rax
    lea .Lfirst_inactive(%rip),%rdx
    test %ecx,%ecx
    cmovz %rdx,%rax
    dec %ecx
    jmp *%rax
.Lfirst_live:
    lfence
    rdpmc
    lfence
    shl $32,%rdx
    or %rdx,%rax
    jmp .Lfirst_done
.Lfirst_inactive:
    xor %eax,%eax
.Lfirst_done:
    mov %rax,64(%r14)
    mov $4096,%ebx
.Lbranches:
    dec %ebx
    jnz .Lbranches
    mov 12(%r13),%ecx
    mov %rcx,88(%r14)
    lea .Llast_live(%rip),%rax
    lea .Llast_inactive(%rip),%rdx
    test %ecx,%ecx
    cmovz %rdx,%rax
    dec %ecx
    jmp *%rax
.Llast_live:
    lfence
    rdpmc
    lfence
    shl $32,%rdx
    or %rdx,%rax
    jmp .Llast_done
.Llast_inactive:
    xor %eax,%eax
.Llast_done:
    mov %rax,104(%r14)
    mov 16(%r13),%rax
    mov %rax,112(%r14)
    movzwq 48(%r13),%rax
    mov %rax,96(%r14)
    mov 40(%r13),%rax
    mov %rax,136(%r14)
    mov 8(%r13),%eax
    mov %rax,80(%r14)
    call *24(%r14)
    mov %rax,152(%r14)
    add $8,%rsp
    pop %r15
    pop %r14
    pop %r13
    pop %r12
    pop %rbx
    pop %rbp
    ret
.size rcb_acquisition_operation,.-rcb_acquisition_operation

.globl rcb_acquisition_sample
.hidden rcb_acquisition_sample
.type rcb_acquisition_sample,@function
rcb_acquisition_sample:
    mov $39,%eax
    jmp *%r12
.size rcb_acquisition_sample,.-rcb_acquisition_sample

.globl rcb_acquisition_no_sample
.hidden rcb_acquisition_no_sample
.type rcb_acquisition_no_sample,@function
rcb_acquisition_no_sample:
    xor %eax,%eax
    ret
.size rcb_acquisition_no_sample,.-rcb_acquisition_no_sample

.globl rcb_transport_clock_probe
.hidden rcb_transport_clock_probe
.type rcb_transport_clock_probe,@function
rcb_transport_clock_probe:
    push %rbx
    mov %rdi,%rbx
    mov $39,%eax
    call *%rbx
    mov $4096,%ecx
.Ltransport_branches:
    dec %ecx
    jnz .Ltransport_branches
    mov $39,%eax
    call *%rbx
    pop %rbx
    ret
.size rcb_transport_clock_probe,.-rcb_transport_clock_probe
.section .note.GNU-stack,"",@progbits
