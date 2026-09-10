.intel_syntax noprefix
.section .bss
.p2align 3
trajectory:
    .zero 48
.text
.global _start
_start:
    lea r12, [rip + trajectory]
    call syscall_sample
    mov [r12], rax
    xor eax, eax
    test eax, eax
    jz 1f
1:
    call syscall_sample
    mov [r12 + 8], rax
    test eax, eax
    jnz 2f
2:
    call instruction_sample
    mov [r12 + 16], rax
    test eax, eax
    jnz 3f
3:
    call instruction_sample
    mov [r12 + 24], rax
    mov ecx, 7
4:
    dec ecx
    jnz 4b
    call syscall_sample
    mov [r12 + 32], rax
    test eax, eax
    jnz 5f
5:
    call instruction_sample
    mov [r12 + 40], rax
    mov eax, 1
    mov edi, 1
    mov rsi, r12
    mov edx, 48
    syscall
    .fill 8,1,0x90
    mov eax, 231
    xor edi, edi
    syscall
    ud2
.p2align 3
syscall_sample:
    mov eax, 39
    .p2align 3
    syscall
    .fill 8,1,0x90
    ret
.p2align 3
instruction_sample:
    rdtsc
    .fill 8,1,0x90
    shl rdx, 32
    or rax, rdx
    ret
.section .note.GNU-stack,"",@progbits
