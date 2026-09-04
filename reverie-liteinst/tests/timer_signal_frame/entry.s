.section .tbss,"awT",@nobits
.p2align 3
.type timer_frame_record,@tls_object
.size timer_frame_record,8
timer_frame_record:
    .zero 8
.text
.global timer_frame_set_record
.hidden timer_frame_set_record
timer_frame_set_record:
    mov qword ptr fs:[timer_frame_record@TPOFF], rdi
    ret
.global timer_frame_get_record
.hidden timer_frame_get_record
timer_frame_get_record:
    mov rax, qword ptr fs:[timer_frame_record@TPOFF]
    ret

.macro gate_control command
    mov edi, 16
    mov rsi, [r15]
    mov edx, \command
    xor ecx, ecx
    xor r8d, r8d
    xor r9d, r9d
    mov qword ptr [rsp], 0
    call reverie_preload_trusted_syscall
.endm

.global timer_frame_entry
.hidden timer_frame_entry
.type timer_frame_entry,@function
timer_frame_entry:
    cld
    push r12
    push r13
    push r14
    push r15
    push rbp
    mov rbp, rsp
    mov r13, rsi
    mov r14, rdx
    mov r15, qword ptr fs:[timer_frame_record@TPOFF]
    mov qword ptr [r15 + 8], 1
    sub rsp, 16
    gate_control 0x2401
    test rax, rax
    js timer_frame_fatal
    mov rsp, [r15 + 16]
    mov rdi, r15
    mov rsi, r13
    mov rdx, r14
    call {body}
    mov rsp, rbp
    sub rsp, 16
    mov qword ptr [r15 + 8], 0
    gate_control 0x2400
    test rax, rax
    lea rdx, [rip + timer_frame_fatal]
    lea rcx, [rip + .Lframe_return]
    cmovns rdx, rcx
    jmp rdx
.Lframe_return:
    add rsp, 16
    pop rbp
    pop r15
    pop r14
    pop r13
    pop r12
    ret
.size timer_frame_entry, .-timer_frame_entry

.global timer_frame_guest
.hidden timer_frame_guest
.type timer_frame_guest,@function
timer_frame_guest:
    push rbx
    push r12
    push r13
    push r14
    push r15
    sub rsp, 16
    mov r15, rdi
    mov r12, rsi
    gate_control 0x2400
    test rax, rax
    lea rdx, [rip + timer_frame_fatal]
    lea rcx, [rip + .Lguest_begin]
    cmovns rdx, rcx
    jmp rdx
.Lguest_begin:
    mov rbx, {sentinel}
    movq xmm0, rbx
    mov [rsp - 64], rbx
    std
    stc
.global timer_frame_loop_begin
.hidden timer_frame_loop_begin
timer_frame_loop_begin:
    dec r12
    jnz timer_frame_loop_begin
.global timer_frame_loop_end
.hidden timer_frame_loop_end
timer_frame_loop_end:
    mov [r15 + {saved_rbx}], rbx
    movq rdx, xmm0
    mov [r15 + {saved_xmm}], rdx
    mov rdx, [rsp - 64]
    mov [r15 + {saved_redzone}], rdx
    pushfq
    pop rdx
    mov [r15 + {saved_flags}], rdx
    cld
    gate_control 0x2401
    test rax, rax
    js timer_frame_fatal
    add rsp, 16
    pop r15
    pop r14
    pop r13
    pop r12
    pop rbx
    ret
.size timer_frame_guest, .-timer_frame_guest

timer_frame_fatal:
    mov edi, 231
    mov esi, 80
    xor edx, edx
    xor ecx, ecx
    xor r8d, r8d
    xor r9d, r9d
    mov qword ptr [rsp], 0
    call reverie_preload_trusted_syscall
    ud2
