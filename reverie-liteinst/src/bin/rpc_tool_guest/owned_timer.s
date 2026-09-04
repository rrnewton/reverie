.text
.global owned_timer_entry
.hidden owned_timer_entry
.type owned_timer_entry,@function
owned_timer_entry:
    sub rsp, 24
    mov [rsp], rdi
    call {clock_begin}
    mov [rsp + 8], rax
    mov rdi, [rsp]
    call {timer_initialize}
    mov edi, eax
    mov rsi, [rsp + 8]
    call {clock_finish}
    mov rdi, [rsp]
    lea rsi, [rdi + {timer_before_offset}]
    lea rdx, [rdi + {timer_after_offset}]
    add rdi, {timer_registers_offset}
    call owned_timer_probe
    mov rdi, [rsp]
    call {timer_verify}
    ud2
.size owned_timer_entry, .-owned_timer_entry

.macro timer_site name
    .global owned_timer_\name
    .hidden owned_timer_\name
owned_timer_\name:
.endm
.type owned_timer_probe,@function
owned_timer_probe:
    .irp register,16,17,18,19,20,21,22,23,24,25,26,27,28,29,30,31
    vpternlogd zmm\register, zmm\register, zmm\register, 0xff
    .endr
    owned_probe_prefix
    timer_site first
    cpuid
    timer_site loop
    dec ecx
    jnz owned_timer_loop
    nop
    timer_site one
    jmp owned_timer_jump
    timer_site jump
    nop
    timer_site two
    nop
    timer_site interrupt
    cpuid
    nop
    timer_site three
    nop
    push 0xed7
    popfq
    timer_site last
    cpuid
    owned_probe_suffix 1
.size owned_timer_probe, .-owned_timer_probe
