/* The guest entry and resumed prefix use no original guest stack storage. */
#include "abi.h"
.text
.globl trusted_call
.type trusted_call,@function
trusted_call:
    mov %rdi,%rax
    mov %rsi,%rdi
    mov %rdx,%rsi
    mov %rcx,%rdx
    mov %r8,%r10
    mov %r9,%r8
    mov 8(%rsp),%r9
.globl trusted_syscall
trusted_syscall:
    syscall
.globl trusted_return
trusted_return:
    ret
.size trusted_call,.-trusted_call

/* Same concrete two-integer return ABI as the reviewed native gate. All
 * memory access is before guest rights or after restoring caller rights. */
.globl guest_effect
.type guest_effect,@function
guest_effect:
    push %r12
    push %r13
    push %r14
    push %r15
    mov %edx,%r12d
    mov %rdi,%r13
    mov 16(%rsi),%r14
    mov (%rsi),%rdi
    mov 8(%rsi),%rdx
    mov 24(%rsi),%r10
    mov 32(%rsi),%r8
    mov 40(%rsi),%r9
    mov %rdx,%rsi
    xor %ecx,%ecx
    rdpkru
    mov %eax,%r15d
    mov %r12d,%eax
    xor %edx,%edx
    wrpkru
    lfence
    mov %r13,%rax
    mov %r14,%rdx
.globl effect_syscall
effect_syscall:
    syscall
.globl effect_return
effect_return:
    mov %rax,%r12
    xor %ecx,%ecx
    rdpkru
    mov %eax,%r14d
    mov %r15d,%eax
    xor %ecx,%ecx
    xor %edx,%edx
    wrpkru
    lfence
    mov %r12,%rax
    mov %r14d,%edx
    pop %r15
    pop %r14
    pop %r13
    pop %r12
    ret
.size guest_effect,.-guest_effect

/* Kernel SA_SIGINFO arguments survive; no stack/global/TLS access precedes
 * WRPKRU. Tail-jump preserves the real kernel restorer return address. */
.globl signal_entry
.type signal_entry,@function
signal_entry:
    mov %rdx,%r8
    xor %eax,%eax
    xor %ecx,%ecx
    xor %edx,%edx
    wrpkru
    lfence
    mov %r8,%rdx
    jmp handle_sigsys
.size signal_entry,.-signal_entry

.globl signal_restorer
.type signal_restorer,@function
signal_restorer:
    incq restorer_requests(%rip)
    mov $15,%eax
.globl restorer_syscall
restorer_syscall:
    syscall
    ud2
.size signal_restorer,.-signal_restorer

/* A first genuine sigreturn enters here with aligned owned ordinary RSP.
 * The CALL/RET completes before the second genuine signal is requested. */
.globl callback_entry
.type callback_entry,@function
callback_entry:
    call ordinary_callback
.globl completion_request
completion_request:
    mov $39,%eax
.globl completion_syscall
completion_syscall:
    syscall
.globl completion_return
completion_return:
    ud2
.size callback_entry,.-callback_entry

.globl wrong_site
.type wrong_site,@function
wrong_site:
    mov $39,%eax
.globl wrong_syscall
wrong_syscall:
    syscall
.globl wrong_return
wrong_return:
    ud2
.size wrong_site,.-wrong_site

/* All setup and seeding runs before guest PKRU/RSP. Native and mediated
 * executions use the identical guest SYSCALL and identical observation code. */
.globl enter_guest
.type enter_guest,@function
enter_guest:
    mov %rsp,OFF(F_HOST_SP)(%rdi)
    mov %rbx,OFF(F_HOST_BX)(%rdi)
    mov %rbp,OFF(F_HOST_BP)(%rdi)
    mov %r12,OFF(F_HOST_R12)(%rdi)
    mov %r13,OFF(F_HOST_R13)(%rdi)
    mov %r14,OFF(F_HOST_R14)(%rdi)
    mov %r15,OFF(F_HOST_R15)(%rdi)
    mov %rdi,%r15
    fninit
    fld1
    fldpi
    fldl2e
    fldcw OFF(F_X87CW)(%r15)
    ldmxcsr OFF(F_MXCSR)(%r15)
    movdqu SEED_OFFSET(%r15),%xmm0
    movdqu SEED_OFFSET(%r15),%xmm1
    movdqu SEED_OFFSET(%r15),%xmm2
    movdqu SEED_OFFSET(%r15),%xmm3
    movdqu SEED_OFFSET(%r15),%xmm4
    movdqu SEED_OFFSET(%r15),%xmm5
    movdqu SEED_OFFSET(%r15),%xmm6
    movdqu SEED_OFFSET(%r15),%xmm7
    movdqu SEED_OFFSET(%r15),%xmm8
    movdqu SEED_OFFSET(%r15),%xmm9
    movdqu SEED_OFFSET(%r15),%xmm10
    movdqu SEED_OFFSET(%r15),%xmm11
    movdqu SEED_OFFSET(%r15),%xmm12
    movdqu SEED_OFFSET(%r15),%xmm13
    movdqu SEED_OFFSET(%r15),%xmm14
    movdqu SEED_OFFSET(%r15),%xmm15
    cmpq $0,OFF(F_AVX)(%r15)
    je .Lseed_done
    vmovdqu SEED_OFFSET(%r15),%ymm0
    vmovdqu SEED_OFFSET(%r15),%ymm1
    vmovdqu SEED_OFFSET(%r15),%ymm2
    vmovdqu SEED_OFFSET(%r15),%ymm3
    vmovdqu SEED_OFFSET(%r15),%ymm4
    vmovdqu SEED_OFFSET(%r15),%ymm5
    vmovdqu SEED_OFFSET(%r15),%ymm6
    vmovdqu SEED_OFFSET(%r15),%ymm7
    vmovdqu SEED_OFFSET(%r15),%ymm8
    vmovdqu SEED_OFFSET(%r15),%ymm9
    vmovdqu SEED_OFFSET(%r15),%ymm10
    vmovdqu SEED_OFFSET(%r15),%ymm11
    vmovdqu SEED_OFFSET(%r15),%ymm12
    vmovdqu SEED_OFFSET(%r15),%ymm13
    vmovdqu SEED_OFFSET(%r15),%ymm14
    vmovdqu SEED_OFFSET(%r15),%ymm15
    cmpq $0,OFF(F_AVX512)(%r15)
    je .Lseed_done
    vmovdqu64 SEED_OFFSET(%r15),%zmm0
    vmovdqu64 SEED_OFFSET(%r15),%zmm1
    vmovdqu64 SEED_OFFSET(%r15),%zmm2
    vmovdqu64 SEED_OFFSET(%r15),%zmm3
    vmovdqu64 SEED_OFFSET(%r15),%zmm4
    vmovdqu64 SEED_OFFSET(%r15),%zmm5
    vmovdqu64 SEED_OFFSET(%r15),%zmm6
    vmovdqu64 SEED_OFFSET(%r15),%zmm7
    vmovdqu64 SEED_OFFSET(%r15),%zmm8
    vmovdqu64 SEED_OFFSET(%r15),%zmm9
    vmovdqu64 SEED_OFFSET(%r15),%zmm10
    vmovdqu64 SEED_OFFSET(%r15),%zmm11
    vmovdqu64 SEED_OFFSET(%r15),%zmm12
    vmovdqu64 SEED_OFFSET(%r15),%zmm13
    vmovdqu64 SEED_OFFSET(%r15),%zmm14
    vmovdqu64 SEED_OFFSET(%r15),%zmm15
    vmovdqu64 SEED_OFFSET(%r15),%zmm16
    vmovdqu64 SEED_OFFSET(%r15),%zmm17
    vmovdqu64 SEED_OFFSET(%r15),%zmm18
    vmovdqu64 SEED_OFFSET(%r15),%zmm19
    vmovdqu64 SEED_OFFSET(%r15),%zmm20
    vmovdqu64 SEED_OFFSET(%r15),%zmm21
    vmovdqu64 SEED_OFFSET(%r15),%zmm22
    vmovdqu64 SEED_OFFSET(%r15),%zmm23
    vmovdqu64 SEED_OFFSET(%r15),%zmm24
    vmovdqu64 SEED_OFFSET(%r15),%zmm25
    vmovdqu64 SEED_OFFSET(%r15),%zmm26
    vmovdqu64 SEED_OFFSET(%r15),%zmm27
    vmovdqu64 SEED_OFFSET(%r15),%zmm28
    vmovdqu64 SEED_OFFSET(%r15),%zmm29
    vmovdqu64 SEED_OFFSET(%r15),%zmm30
    vmovdqu64 SEED_OFFSET(%r15),%zmm31
    movabs $0x123456789abcdef0,%rax
    kmovq %rax,%k0
    kmovq %rax,%k1
    kmovq %rax,%k2
    kmovq %rax,%k3
    kmovq %rax,%k4
    kmovq %rax,%k5
    kmovq %rax,%k6
    kmovq %rax,%k7
.Lseed_done:
    /* Seed arithmetic flags and DF; IF remains user-enabled, TF/RF excluded. */
    pushq $0xed7
    popfq
    mov OFF(F_GUEST_SP)(%r15),%rsp
    mov OFF(F_GUEST_PKRU)(%r15),%eax
    mov $0,%ecx
    mov $0,%edx
    wrpkru
    lfence
    /* r15 is on the accessible observation key, even with key zero denied. */
    mov OFF(F_NR)(%r15),%rax
    mov OFF(F_ARGS+0)(%r15),%rdi
    mov OFF(F_ARGS+1)(%r15),%rsi
    mov OFF(F_ARGS+2)(%r15),%rdx
    mov OFF(F_ARGS+3)(%r15),%r10
    mov OFF(F_ARGS+4)(%r15),%r8
    mov OFF(F_ARGS+5)(%r15),%r9
    movabs $0x1212121212121212,%r12
    movabs $0x1313131313131313,%r13
    movabs $0x1414141414141414,%r14
    movabs $0xb0b0b0b0b0b0b0b0,%rbp
    movabs $0xb1b1b1b1b1b1b1b1,%rbx
    movabs $0xcccccccccccccccc,%rcx
    movabs $0x1111111111111111,%r11
.globl guest_syscall
guest_syscall:
    syscall
.globl guest_resume
guest_resume:
    mov %r8,OFF(F_REGS+0)(%r15)
    mov %r9,OFF(F_REGS+1)(%r15)
    mov %r10,OFF(F_REGS+2)(%r15)
    mov %r11,OFF(F_REGS+3)(%r15)
    mov %r12,OFF(F_REGS+4)(%r15)
    mov %r13,OFF(F_REGS+5)(%r15)
    mov %r14,OFF(F_REGS+6)(%r15)
    mov %r15,OFF(F_REGS+7)(%r15)
    mov %rdi,OFF(F_REGS+8)(%r15)
    mov %rsi,OFF(F_REGS+9)(%r15)
    mov %rbp,OFF(F_REGS+10)(%r15)
    mov %rbx,OFF(F_REGS+11)(%r15)
    mov %rdx,OFF(F_REGS+12)(%r15)
    mov %rax,OFF(F_REGS+13)(%r15)
    mov %rcx,OFF(F_REGS+14)(%r15)
    mov %rsp,OFF(F_REGS+15)(%r15)
    lea guest_resume(%rip),%rax
    mov %rax,OFF(F_REGS+16)(%r15)
    /* The original RSP has been observed; it is never dereferenced. */
    mov OFF(F_OBSERVER_SP)(%r15),%rsp
    pushfq
    pop %rax
    mov %rax,OFF(F_REGS+17)(%r15)
    cld
    xor %eax,%eax
    mov %ss,%ax
    shl $16,%rax
    mov %fs,%ax
    shl $16,%rax
    mov %gs,%ax
    shl $16,%rax
    mov %cs,%ax
    mov %rax,OFF(F_REGS+18)(%r15)
    xor %ecx,%ecx
    rdpkru
    mov %rax,OFF(F_OBSERVED_PKRU)(%r15)
    mov OFF(F_XCR0)(%r15),%rax
    mov %rax,%rdx
    shr $32,%rdx
    xsave64 XSAVE_OFFSET(%r15)
    movq $1,OFF(F_GUEST_REACHED)(%r15)
    /* Measurement is complete. Both native and mediated controls now leave
     * guest observation and restore harness rights before touching its stack. */
    mov OFF(F_HOST_PKRU)(%r15),%eax
    xor %ecx,%ecx
    xor %edx,%edx
    wrpkru
    lfence
    fninit
    movl $0x1f80,OFF(F_MXCSR)(%r15)
    ldmxcsr OFF(F_MXCSR)(%r15)
    mov OFF(F_HOST_SP)(%r15),%rsp
    mov OFF(F_HOST_BX)(%r15),%rbx
    mov OFF(F_HOST_BP)(%r15),%rbp
    mov OFF(F_HOST_R12)(%r15),%r12
    mov OFF(F_HOST_R13)(%r15),%r13
    mov OFF(F_HOST_R14)(%r15),%r14
    mov OFF(F_HOST_R15)(%r15),%r15
    ret
.size enter_guest,.-enter_guest

.globl clobber_guest_fp
.type clobber_guest_fp,@function
clobber_guest_fp:
    /* Ordinary owned stack; deliberately clobber MXCSR as well as vectors. */
    sub $8,%rsp
    movl $0x1f80,(%rsp)
    ldmxcsr (%rsp)
    add $8,%rsp
    fninit
    pxor %xmm0,%xmm0
    pxor %xmm1,%xmm1
    pxor %xmm2,%xmm2
    pxor %xmm3,%xmm3
    pxor %xmm4,%xmm4
    pxor %xmm5,%xmm5
    pxor %xmm6,%xmm6
    pxor %xmm7,%xmm7
    pxor %xmm8,%xmm8
    pxor %xmm9,%xmm9
    pxor %xmm10,%xmm10
    pxor %xmm11,%xmm11
    pxor %xmm12,%xmm12
    pxor %xmm13,%xmm13
    pxor %xmm14,%xmm14
    pxor %xmm15,%xmm15
    test %edi,%edi
    jz .Lclobber_done
    vzeroall
    test %esi,%esi
    jz .Lclobber_done
    vpxord %zmm0,%zmm0,%zmm0
    vpxord %zmm1,%zmm1,%zmm1
    vpxord %zmm2,%zmm2,%zmm2
    vpxord %zmm3,%zmm3,%zmm3
    vpxord %zmm4,%zmm4,%zmm4
    vpxord %zmm5,%zmm5,%zmm5
    vpxord %zmm6,%zmm6,%zmm6
    vpxord %zmm7,%zmm7,%zmm7
    vpxord %zmm8,%zmm8,%zmm8
    vpxord %zmm9,%zmm9,%zmm9
    vpxord %zmm10,%zmm10,%zmm10
    vpxord %zmm11,%zmm11,%zmm11
    vpxord %zmm12,%zmm12,%zmm12
    vpxord %zmm13,%zmm13,%zmm13
    vpxord %zmm14,%zmm14,%zmm14
    vpxord %zmm15,%zmm15,%zmm15
    vpxord %zmm16,%zmm16,%zmm16
    vpxord %zmm17,%zmm17,%zmm17
    vpxord %zmm18,%zmm18,%zmm18
    vpxord %zmm19,%zmm19,%zmm19
    vpxord %zmm20,%zmm20,%zmm20
    vpxord %zmm21,%zmm21,%zmm21
    vpxord %zmm22,%zmm22,%zmm22
    vpxord %zmm23,%zmm23,%zmm23
    vpxord %zmm24,%zmm24,%zmm24
    vpxord %zmm25,%zmm25,%zmm25
    vpxord %zmm26,%zmm26,%zmm26
    vpxord %zmm27,%zmm27,%zmm27
    vpxord %zmm28,%zmm28,%zmm28
    vpxord %zmm29,%zmm29,%zmm29
    vpxord %zmm30,%zmm30,%zmm30
    vpxord %zmm31,%zmm31,%zmm31
    kxorq %k0,%k0,%k0
    kxorq %k1,%k1,%k1
    kxorq %k2,%k2,%k2
    kxorq %k3,%k3,%k3
    kxorq %k4,%k4,%k4
    kxorq %k5,%k5,%k5
    kxorq %k6,%k6,%k6
    kxorq %k7,%k7,%k7
.Lclobber_done:
    ret
.size clobber_guest_fp,.-clobber_guest_fp
.section .note.GNU-stack,"",@progbits
