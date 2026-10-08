/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

/* No writes to the kernel-built initial stack, including its red zone. */
.section .text
.globl _start
.type _start, @function
_start:
    movq %rsp, %r13
    xorl %r12d, %r12d
    xorl %r14d, %r14d
    xorl %eax, %eax
    cpuid
    cmpl $7, %eax
    jb .Lpkru_saved
    movl $7, %eax
    xorl %ecx, %ecx
    cpuid
    testl $0x10, %ecx                 /* CPUID.7.0:ECX.OSPKE */
    jz .Lpkru_saved
    xorl %ecx, %ecx
    rdpkru
    movl %eax, %r14d
    movl $1, %r12d
.Lpkru_saved:
    movl $9, %eax                    /* mmap */
    movl $_scratch_start, %edi
    movl $_scratch_size, %esi
    movl $3, %edx                    /* PROT_READ | PROT_WRITE */
    movl $0x100021, %r10d            /* SHARED | ANON | FIXED_NOREPLACE */
    movq $-1, %r8
    xorl %r9d, %r9d
    syscall
    cmpq $_scratch_start, %rax
    jne .Labort
    movl %r14d, loader_record+16(%rip)
    movl %r12d, loader_record+20(%rip)
    leaq loader_stack+65536(%rip), %rsp
    movq %r13, %rdi
    call loader_main
    ud2
.size _start, .-_start

/* All calls and writable state end here. Only the record page survives,
 * read-only, to permit a jump with no live general-purpose register. */
.globl loader_enter
.type loader_enter, @function
loader_enter:
    leaq loader_record+4096(%rip), %rsp
    pushq $0x202
    movl $1, %eax
    cpuid
    andl $0x0c000000, %ecx            /* XSAVE and OSXSAVE */
    cmpl $0x0c000000, %ecx
    jne .Llegacy_fp
    xorl %ecx, %ecx
    xgetbv
    /* The zero XSTATE_BV requests the architectural initial state for every
     * component enabled in XCR0, including YMM, ZMM and opmask registers. */
    xrstor64 loader_xstate(%rip)
    /* XRSTOR reads MXCSR from the legacy area even when XSTATE_BV says the
     * SSE component is initial. Our zero image must not leave MXCSR zero. */
    ldmxcsr loader_mxcsr(%rip)
    jmp .Lrestore_pkru
.Llegacy_fp:
    fninit
    ldmxcsr loader_mxcsr(%rip)
    pxor %xmm0, %xmm0
    pxor %xmm1, %xmm1
    pxor %xmm2, %xmm2
    pxor %xmm3, %xmm3
    pxor %xmm4, %xmm4
    pxor %xmm5, %xmm5
    pxor %xmm6, %xmm6
    pxor %xmm7, %xmm7
    pxor %xmm8, %xmm8
    pxor %xmm9, %xmm9
    pxor %xmm10, %xmm10
    pxor %xmm11, %xmm11
    pxor %xmm12, %xmm12
    pxor %xmm13, %xmm13
    pxor %xmm14, %xmm14
    pxor %xmm15, %xmm15
.Lrestore_pkru:
    cmpl $0, loader_record+20(%rip)
    je .Lremove_scratch
    movl loader_record+16(%rip), %eax
    xorl %ecx, %ecx
    xorl %edx, %edx
    wrpkru
.Lremove_scratch:
    movl $11, %eax                   /* munmap */
    movl $_record_end, %edi
    movl $_transient_size, %esi
    syscall
    testq %rax, %rax
    jne .Labort
    movl $10, %eax                   /* mprotect */
    movl $_scratch_start, %edi
    movl $4096, %esi
    movl $1, %edx                    /* PROT_READ */
    syscall
    testq %rax, %rax
    jne .Labort
#ifdef LOADER_TEST_MUTATIONS
    testq $2, loader_record+24(%rip)
    jnz .Ldirty_entry
#endif
.Lclean_entry:
    popfq
    movl $0, %eax
    movl $0, %ebx
    movl $0, %ecx
    movl $0, %edx
    movl $0, %esi
    movl $0, %edi
    movl $0, %ebp
    movl $0, %r8d
    movl $0, %r9d
    movl $0, %r10d
    movl $0, %r11d
    movl $0, %r12d
    movl $0, %r13d
    movl $0, %r14d
    movl $0, %r15d
    movq loader_record(%rip), %rsp
    jmp *loader_record+8(%rip)
#ifdef LOADER_TEST_MUTATIONS
.Ldirty_entry:
    /* A separate build admits this deliberately faulty entry transfer. */
    popfq
    movl $0, %eax
    movl $0, %ebx
    movl $0, %ecx
    movl $0, %edx
    movl $0, %esi
    movl $0, %edi
    movl $0, %ebp
    movl $0, %r8d
    movl $0, %r9d
    movl $0, %r10d
    movl $0, %r11d
    movl $0x1234, %r12d
    movl $0, %r13d
    movl $0, %r14d
    movl $0, %r15d
    movq loader_record(%rip), %rsp
    jmp *loader_record+8(%rip)
#endif
.Labort:
    movl $231, %eax                  /* exit_group */
    movl $127, %edi
    syscall
    ud2
.size loader_enter, .-loader_enter

.section .note.GNU-stack,"",@progbits
