.text
.global owned_clock_entry
.hidden owned_clock_entry
.type owned_clock_entry,@function
owned_clock_entry:
    sub rsp, 24
    mov [rsp], rdi
    call {clock_begin}
    mov [rsp + 8], rax
    mov rdi, [rsp]
    call {clock_initialize}
    mov edi, eax
    mov rsi, [rsp + 8]
    call {clock_finish}
    mov rdi, [rsp]
    lea rsi, [rdi + {clock_before_offset}]
    lea rdx, [rdi + {clock_after_offset}]
    add rdi, {clock_registers_offset}
    call owned_clock_probe
    mov rdi, [rsp]
    call {clock_verify}
    ud2
.size owned_clock_entry, .-owned_clock_entry

.macro owned_clock_sample index, branches, instruction
    .if \branches
    mov eax, \branches
1:
    dec eax
    jnz 1b
    .endif
    .if (\index % 3) == 0
    mov rax, 0xfedcba980000feed
    mov rcx, 0xfedcba980000baad
    .endif
    push 0xed7
    popfq
    .global owned_clock_site_\index
    .hidden owned_clock_site_\index
owned_clock_site_\index:
    \instruction
    mov [rip + {clock_outputs} + \index * 32], rax
    mov [rip + {clock_outputs} + \index * 32 + 8], rbx
    mov [rip + {clock_outputs} + \index * 32 + 16], rcx
    mov [rip + {clock_outputs} + \index * 32 + 24], rdx
.endm

.type owned_clock_probe,@function
owned_clock_probe:
    .irp register,16,17,18,19,20,21,22,23,24,25,26,27,28,29,30,31
    vpternlogd zmm\register, zmm\register, zmm\register, 0xff
    .endr
    owned_probe_prefix
    owned_clock_sample 0, 7, cpuid
    owned_clock_sample 1, 1, rdtsc
    owned_clock_sample 2, 1, rdtscp
    owned_clock_sample 3, 1, cpuid
    owned_clock_sample 4, 7, rdtsc
    owned_clock_sample 5, 1, rdtscp
    owned_clock_sample 6, 1, cpuid
    owned_clock_sample 7, 0, rdtsc
    owned_clock_sample 8, 2, rdtscp
    owned_probe_suffix 1
.size owned_clock_probe, .-owned_clock_probe
