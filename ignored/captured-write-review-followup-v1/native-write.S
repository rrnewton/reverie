/* Native x86-64 Linux control. RDI is deliberately a full 64-bit value. */
.global _start
.section .text
_start:
    movabs $0x100000001, %rdi
    lea payload(%rip), %rsi
    mov $3, %edx
    mov $0x63617077, %r10d
    mov $17, %r8d
    mov $0x9876, %r9d
    mov $1, %eax
    syscall
    mov %rax, raw_rax(%rip)
    /* Retain RAX unchanged before writing the independent binary report. */
    mov $3, %edi
    lea record(%rip), %rsi
    mov $72, %edx
    mov $1, %eax
    syscall
    cmp $72, %rax
    jne failed
    cmpq $3, raw_rax(%rip)
    jne failed
    xor %edi, %edi
    mov $60, %eax
    syscall
failed:
    mov $1, %edi
    mov $60, %eax
    syscall
.section .rodata
payload:
    .ascii "abc"
.section .data
record:
    .quad 1
    .quad 0x100000001
    .quad payload
    .quad 3
    .quad 0x63617077
    .quad 17
    .quad 0x9876
    .quad 64
raw_rax:
    .quad 0
.section .note.GNU-stack,"",@progbits
