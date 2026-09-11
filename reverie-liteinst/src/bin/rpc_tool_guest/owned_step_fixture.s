.text
.global owned_step_fixture_entry
.hidden owned_step_fixture_entry
.type owned_step_fixture_entry,@function
owned_step_fixture_entry:
    sub rsp, 24
    mov [rsp], rdi
    call {clock_begin}
    mov [rsp + 8], rax
    mov rdi, [rsp]
    call {step_initialize}
    mov edi, eax
    mov rsi, [rsp + 8]
    call {clock_finish}
    mov rdi, [rsp]
    lea rsi, [rdi + {step_before_offset}]
    lea rdx, [rdi + {step_after_offset}]
    add rdi, {step_registers_offset}
    call owned_step_fixture_probe
    mov rdi, [rsp]
    call {step_verify}
    ud2
.size owned_step_fixture_entry, .-owned_step_fixture_entry

.type owned_step_fixture_probe,@function
owned_step_fixture_probe:
    .irp register,16,17,18,19,20,21,22,23,24,25,26,27,28,29,30,31
    vpternlogd zmm\register, zmm\register, zmm\register, 0xff
    .endr
    owned_probe_prefix
.global owned_step_first
.hidden owned_step_first
owned_step_first:
    cpuid
    nop
    cpuid
    nop
.global owned_step_last
.hidden owned_step_last
owned_step_last:
    owned_probe_suffix 1
.size owned_step_fixture_probe, .-owned_step_fixture_probe
