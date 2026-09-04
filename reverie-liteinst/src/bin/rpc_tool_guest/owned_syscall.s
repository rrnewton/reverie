.text
.global owned_syscall_entry
.hidden owned_syscall_entry
.type owned_syscall_entry,@function
owned_syscall_entry:
    sub rsp, 24
    mov [rsp], rdi
    call {clock_begin}
    mov [rsp + 8], rax
    mov rdi, [rsp]
    call {syscall_initialize}
    mov edi, eax
    mov rsi, [rsp + 8]
    call {clock_finish}
    mov rdi, [rsp]
    lea rsi, [rdi + {syscall_before_offset}]
    lea rdx, [rdi + {syscall_after_offset}]
    add rdi, {syscall_registers_offset}
    call owned_syscall_probe
    mov rdi, [rsp]
    call {syscall_verify}
    ud2
.size owned_syscall_entry, .-owned_syscall_entry

.macro syscall_site name
    .global owned_syscall_\name
    .hidden owned_syscall_\name
owned_syscall_\name:
.endm
.type owned_syscall_probe,@function
owned_syscall_probe:
    .irp register,16,17,18,19,20,21,22,23,24,25,26,27,28,29,30,31
    vpternlogd zmm\register, zmm\register, zmm\register, 0xff
    .endr
    owned_probe_prefix
    mov rbx, 0x2222222222222222
    mov rdi, [rip + {syscall_fd}]
    mov rsi, [rip + {syscall_buffer}]
    mov edx, 39
    mov eax, 39
    syscall_site first
    syscall
    syscall_site first_return
    mov ecx, 3
    syscall_site loop
    dec ecx
    jnz owned_syscall_loop
    nop
    syscall_site one
    mov [rip + {syscall_results}], rax
    mov eax, 0
    push 0xed7
    popfq
    syscall_site read
    syscall
    syscall_site read_return
    nop
    syscall_site interrupt
    syscall
    syscall_site interrupt_return
    nop
    syscall_site two
    mov [rip + {syscall_results} + 8], rax
    mov [rip + {syscall_results} + 16], rdi
    mov [rip + {syscall_results} + 24], rsi
    mov [rip + {syscall_results} + 32], rdx
    mov rdi, [rsp]
    mov rsi, [rsp + 8]
    owned_probe_suffix 1
    syscall_site end
.size owned_syscall_probe, .-owned_syscall_probe
