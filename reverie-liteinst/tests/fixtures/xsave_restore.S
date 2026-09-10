.intel_syntax noprefix
.section .rodata
.balign 64
ones:
    .rept 8
    .quad -1
    .endr
.bss
.balign 64
before:
    .skip 2560
.balign 64
clobbered:
    .skip 2560
.balign 64
restore_image:
    .skip 2560
.balign 64
after:
    .skip 2560
.text
.global _start
_start:
    mov eax, 1
    cpuid
    and ecx, 0x0c000000
    cmp ecx, 0x0c000000
    jne unsupported
    xor ecx, ecx
    xgetbv
    cmp eax, 0x2e7
    jne unsupported
    test edx, edx
    jne unsupported
    mov eax, 0xd
    xor ecx, ecx
    cpuid
    cmp eax, 0x2e7
    jne unsupported
    cmp ebx, 2440
    jne unsupported
    test edx, edx
    jne unsupported
    mov eax, 0xd
    mov ecx, 7
    cpuid
    cmp eax, 1024
    jne unsupported
    cmp ebx, 1408
    jne unsupported
    mov eax, 7
    xor ecx, ecx
    cpuid
    and ebx, 0x10020
    cmp ebx, 0x10020
    jne unsupported
    lea r12, [rip + before]
    lea r13, [rip + clobbered]
    lea r14, [rip + restore_image]
    lea r15, [rip + after]
#ifdef NONZERO
    lea rbx, [rip + ones]
#else
    mov rbx, r13
#endif
    .irp register,16,17,18,19,20,21,22,23,24,25,26,27,28,29,30,31
    vmovdqu64 zmm\register, [rbx]
    .endr
    fninit
    fld1
    fldpi
    vpcmpeqd ymm0, ymm0, ymm0
    vpcmpeqd ymm1, ymm1, ymm1
    mov eax, 0xffff
    kmovw k1, eax
    mov eax, 0x2e7
    xor edx, edx
    xsave64 [r12]
    mov rsi, r12
    mov rdi, r14
    mov ecx, 305
    rep movsq
#ifdef CLOBBER
#ifdef NONZERO
    mov rbx, r15
#else
    lea rbx, [rip + ones]
#endif
    .irp register,16,17,18,19,20,21,22,23,24,25,26,27,28,29,30,31
    vmovdqu64 zmm\register, [rbx]
    .endr
#endif
    xsave64 [r13]
#ifdef CORRUPT
    xor byte ptr [r14 + 1408], 1
#endif
#ifdef RESTORE
    xrstor64 [r14]
#endif
    xsave64 [r15]
    mov rsi, r12
    call write_image
    mov rsi, r13
    call write_image
    mov rsi, r14
    call write_image
    mov rsi, r15
    call write_image
    mov rsi, r12
    mov rdi, r15
    mov ecx, 2440
    repe cmpsb
    setne dil
    movzx edi, dil
    mov eax, 60
    syscall
unsupported:
    mov edi, 77
    mov eax, 60
    syscall
write_image:
    mov edx, 2440
write_more:
    mov eax, 1
    mov edi, 1
    syscall
    cmp rax, -4
    je write_more
    test rax, rax
    jle write_failed
    cmp rax, rdx
    ja write_failed
    add rsi, rax
    sub rdx, rax
    jne write_more
    ret
write_failed:
    mov edi, 125
    mov eax, 60
    syscall
.section .note.GNU-stack,"",@progbits
