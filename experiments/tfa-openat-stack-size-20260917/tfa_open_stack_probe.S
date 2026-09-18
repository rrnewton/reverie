    .text
    .globl raw_openat_on_stack
    .type raw_openat_on_stack,@function
raw_openat_on_stack:
    pushq %r12
    movq %rsp, %r12
    movq %rdi, %rsp
    movq %rsi, %rsi
    movq $257, %rax
    movq $-100, %rdi
    movq $0, %rdx
    movq $0, %r10
    syscall
    movq %r12, %rsp
    popq %r12
    ret
    .size raw_openat_on_stack, .-raw_openat_on_stack
    .section .note.GNU-stack,"",@progbits
