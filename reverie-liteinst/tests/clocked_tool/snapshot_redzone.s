.macro snapshot_redzone_and_flags redzone, flags
    .set field, 0
    .irp offset,8,16,24,32,40,48,56,64,72,80,88,96,104,112,120,128
    mov rax, [rsp - \offset]
    mov [\redzone + field * 8], rax
    .set field, field + 1
    .endr
    pushfq
    pop qword ptr [\flags]
.endm
