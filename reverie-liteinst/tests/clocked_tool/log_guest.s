.intel_syntax noprefix
.include "snapshot_redzone.s"
.section .bss
.p2align 6
proofs: .zero 42112
input: .zero 5
.section .rodata
output: .byte 79,85,84,0,255
errors: .byte 69,82,82,0,254
.text
.macro controls index, side
    lea r15, [rip + proofs + \index * 6016]
    mov eax, 14
    xor edi, edi
    xor esi, esi
    lea rdx, [r15 + 5520 + \side * 8]
    mov r10d, 8
    syscall
    mov [r15 + 5904 + \side * 40], rax
    .set field, 0
    .irp operation,0x1003,0x1004,0x1022
    mov eax, 158
    mov edi, \operation
    lea rsi, [r15 + 5536 + \side * 56 + field * 8]
    syscall
    mov [r15 + 5912 + \side * 40 + field * 8], rax
    .set field, field + 1
    .endr
    mov eax, 158
    mov edi, 0x1011
    xor esi, esi
    syscall
    mov [r15 + 5560 + \side * 56], rax
    mov eax, 157
    mov edi, 25
    lea rsi, [r15 + 5568 + \side * 56]
    syscall
    mov [r15 + 5936 + \side * 40], rax
    xor ecx, ecx
    xgetbv
    shl rdx, 32
    or rax, rdx
    mov [r15 + 5576 + \side * 56], rax
    xor ecx, ecx
    rdpkru
    mov [r15 + 5584 + \side * 56], rax
    mov rax, [rip + LOG_FIXTURE_ERRNO@GOTPCREL]
    mov rax, [rax]
    mov eax, [rax]
    mov [r15 + 5512 + \side * 4], eax
.endm
.macro snapshot index, side
    mov [rip + proofs + \index * 6016 + \side * 2752], rax
    mov [rip + proofs + \index * 6016 + \side * 2752 + 8], rbx
    mov [rip + proofs + \index * 6016 + \side * 2752 + 16], rcx
    mov [rip + proofs + \index * 6016 + \side * 2752 + 24], rdx
    mov [rip + proofs + \index * 6016 + \side * 2752 + 32], rsi
    mov [rip + proofs + \index * 6016 + \side * 2752 + 40], rdi
    mov [rip + proofs + \index * 6016 + \side * 2752 + 48], rbp
    mov [rip + proofs + \index * 6016 + \side * 2752 + 56], r8
    mov [rip + proofs + \index * 6016 + \side * 2752 + 64], r9
    mov [rip + proofs + \index * 6016 + \side * 2752 + 72], r10
    mov [rip + proofs + \index * 6016 + \side * 2752 + 80], r11
    mov [rip + proofs + \index * 6016 + \side * 2752 + 88], r12
    mov [rip + proofs + \index * 6016 + \side * 2752 + 96], r13
    mov [rip + proofs + \index * 6016 + \side * 2752 + 104], r14
    mov [rip + proofs + \index * 6016 + \side * 2752 + 112], r15
    mov [rip + proofs + \index * 6016 + \side * 2752 + 120], rsp
    mov qword ptr [rip + proofs + \index * 6016 + \side * 2752 + 128], 0
    snapshot_redzone_and_flags "rip + proofs + \index * 6016 + 5648 + \side * 128", "rip + proofs + \index * 6016 + \side * 2752 + 136"
    mov eax, 0x2e7
    xor edx, edx
    xsave64 [rip + proofs + \index * 6016 + \side * 2752 + 192]
    mov rax, [rip + proofs + \index * 6016 + \side * 2752]
    mov rdx, [rip + proofs + \index * 6016 + \side * 2752 + 24]
.endm
.macro sample index, branches
    .if \branches
    mov ecx, \branches
1:  dec ecx
    jnz 1b
    .endif
    mov rax, [rip + LOG_FIXTURE_ERRNO@GOTPCREL]
    mov rax, [rax]
    mov dword ptr [rax], 42
    controls \index, 0
    fninit
    fld1
    fldpi
    vpcmpeqd ymm0, ymm0, ymm0
    vpcmpeqd ymm1, ymm1, ymm1
    .irp register,16,17,18,19,20,21,22,23,24,25,26,27,28,29,30,31
    vpternlogd zmm\register, zmm\register, zmm\register, 0xff
    .endr
    mov eax, 39
    lea rdi, [rip + proofs + \index * 6016]
    mov esi, \index
    lea rdx, [rip + input]
    mov rbx, 0x2323232323232323
    mov rbp, 0x7272727272727272
    mov r8, 0x5555555555555555
    mov r9, 0x6666666666666666
    mov r10, 0x4444444444444444
    mov r11, 0x8888888888888888
    mov r12, 0x7171717171717171
    mov r13, 0x7373737373737373
    mov r14, 0x7474747474747474
    mov r15, 0x7575757575757575
    lea rcx, [rip + 2f]
    mov [rip + proofs + \index * 6016 + 5504], rcx
    .irp offset,16,24,32,40,48,56,64,72,80,88,96,104,112,120,128
    mov qword ptr [rsp - \offset], 123456
    .endr
    push 0xed7
    popfq
    snapshot \index, 0
    push 0xed7
    popfq
    syscall
2:  snapshot \index, 1
    cld
    controls \index, 1
.endm
.global _start
_start:
    sub rsp, 256
    sample 0, 0
    sample 1, 1
    sample 2, 1
    sample 3, 1
    sample 4, 7
    sample 5, 1
    sample 6, 3
    mov eax, 1
    mov edi, 1
    lea rsi, [rip + output]
    mov edx, 5
    syscall
    mov eax, 1
    mov edi, 2
    lea rsi, [rip + errors]
    mov edx, 5
    syscall
    mov eax, 231
    xor edi, edi
    syscall
    ud2
.section .note.GNU-stack,"",@progbits
