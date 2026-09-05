.section .tbss,"awT",@nobits
.p2align 3
.hidden reverie_liteinst_clock_state
.type reverie_liteinst_clock_state,@tls_object
.size reverie_liteinst_clock_state,64
reverie_liteinst_clock_state:
    .zero 64
.text

.macro clock_address target
    mov \target, [rip + reverie_liteinst_clock_state@GOTTPOFF]
    add \target, qword ptr fs:[0]
.endm

.global reverie_liteinst_clock_control
.hidden reverie_liteinst_clock_control
.type reverie_liteinst_clock_control,@function
reverie_liteinst_clock_control:
    clock_address rax
    ret
.size reverie_liteinst_clock_control, .-reverie_liteinst_clock_control

.global reverie_liteinst_clock_enter
.hidden reverie_liteinst_clock_enter
.type reverie_liteinst_clock_enter,@function
reverie_liteinst_clock_enter:
    push rbx
    push r12
    push r13
    mov r12, rdi
    xor r13d, r13d
    clock_address rbx
    lea rax, [rip + .Lclock_enter_legacy]
    lea rdx, [rip + .Lclock_stop]
    cmp qword ptr [rbx + 8], 0
    cmovne rax, rdx
    jmp rax
.Lclock_stop:
    mov eax, 186
    call qword ptr [rbx + 24]
    lea rdx, [rip + .Lclock_stop_owned]
    lea rcx, [rip + .Lclock_fail]
    cmp rax, [rbx + 40]
    cmovne rdx, rcx
    jmp rdx
.Lclock_stop_owned:
    mov eax, 1
    mov edx, 2
    lock cmpxchg [rbx + 32], rdx
    mov ecx, 1
    cmp rax, 1
    cmove r13, rcx
    lea rcx, [rip + .Lclock_enter_legacy]
    lea rdx, [rip + reverie_liteinst_clock_disable_published]
    test rax, rax
    cmovne rcx, rdx
    jmp rcx
.global reverie_liteinst_clock_disable_published
.hidden reverie_liteinst_clock_disable_published
reverie_liteinst_clock_disable_published:
    mov eax, 16
    mov rdi, [rbx + 16]
    mov esi, 0x2401
    xor edx, edx
    call qword ptr [rbx + 24]
    test rax, rax
    jnz .Lclock_fail
    mov qword ptr [rbx + 32], 0
.Lclock_enter_legacy:
    call reverie_liteinst_domain_enter
    cmp qword ptr [rbx + 48], 0
    je .Lclock_enter_done
    call reverie_liteinst_domain_depth
    cmp rax, 2
    jne .Lclock_enter_done
    cmp r12, [rbx + 48]
    jne .Lclock_enter_done
    test r13, r13
    jnz .Lclock_fail
    mov r13, [rbx + 56]
    mov qword ptr [rbx + 48], 0
    mov qword ptr [rbx + 56], 0
    call reverie_liteinst_domain_leave
.Lclock_enter_done:
    mov rax, r13
    pop r13
    pop r12
    pop rbx
    ret
.size reverie_liteinst_clock_enter, .-reverie_liteinst_clock_enter

.global reverie_liteinst_clock_leave
.hidden reverie_liteinst_clock_leave
.type reverie_liteinst_clock_leave,@function
reverie_liteinst_clock_leave:
    push rbx
    push r12
    push r13
    mov r12, rdi
    mov r13, rdx
    clock_address rbx
    cmp r12, 1
    ja .Lclock_fail
    cmp rsi, 2
    ja .Lclock_fail
    cmp rsi, 2
    jne .Lclock_leave_normal
    test r12, r12
    jz .Lclock_leave_normal
    test r13, r13
    jz .Lclock_fail
    cmp qword ptr [rbx + 48], 0
    jne .Lclock_fail
    mov [rbx + 56], r12
    mov [rbx + 48], r13
.global reverie_liteinst_clock_handoff_published
.hidden reverie_liteinst_clock_handoff_published
reverie_liteinst_clock_handoff_published:
    jmp .Lclock_leave_done
.Lclock_leave_normal:
    call reverie_liteinst_domain_leave
    test r12, r12
    jz .Lclock_leave_done
    call reverie_liteinst_domain_depth
    test rax, rax
    jnz .Lclock_fail
    cmp qword ptr [rbx + 8], 1
    jne .Lclock_fail
.global reverie_liteinst_clock_enable_begin
.hidden reverie_liteinst_clock_enable_begin
reverie_liteinst_clock_enable_begin:
    mov qword ptr [rbx + 32], 1
.global reverie_liteinst_clock_enable_published
.hidden reverie_liteinst_clock_enable_published
reverie_liteinst_clock_enable_published:
    mov eax, 16
    mov rdi, [rbx + 16]
    mov esi, 0x2400
    xor edx, edx
    call qword ptr [rbx + 24]
    lea rdx, [rip + .Lclock_leave_done]
    lea rcx, [rip + .Lclock_fail]
    test rax, rax
    cmovne rdx, rcx
    jmp rdx
.Lclock_leave_done:
    pop r13
    pop r12
    pop rbx
    ret
.global reverie_liteinst_clock_enable_end
.hidden reverie_liteinst_clock_enable_end
reverie_liteinst_clock_enable_end:
.size reverie_liteinst_clock_leave, .-reverie_liteinst_clock_leave

.Lclock_fail:
    and rsp, -16
    call {fail}
    ud2

.global reverie_liteinst_clock_constructor_begin
.hidden reverie_liteinst_clock_constructor_begin
.type reverie_liteinst_clock_constructor_begin,@function
reverie_liteinst_clock_constructor_begin:
    push rbx
    clock_address rbx
    cmp qword ptr [rbx], 0
    jne .Lclock_fail
    mov qword ptr [rbx], 1
    call reverie_liteinst_domain_phase
    mov rbx, rax
    call reverie_liteinst_domain_enter
    mov rax, rbx
    pop rbx
    ret
.size reverie_liteinst_clock_constructor_begin, .-reverie_liteinst_clock_constructor_begin

.global reverie_liteinst_clock_constructor_finish
.hidden reverie_liteinst_clock_constructor_finish
.type reverie_liteinst_clock_constructor_finish,@function
reverie_liteinst_clock_constructor_finish:
    clock_address rax
    mov qword ptr [rax], 0
    test edi, edi
    jz .Lconstructor_inactive
    cmp edi, 1
    jne .Lclock_fail
    cmp qword ptr [rax + 8], 1
    jne .Lclock_fail
    mov edi, 1
    xor esi, esi
    xor edx, edx
    jmp reverie_liteinst_clock_leave
.Lconstructor_inactive:
    cmp qword ptr [rax + 8], 0
    jne .Lclock_fail
    push rbx
    mov rbx, rsi
    call reverie_liteinst_domain_leave
    mov rdi, rbx
    call reverie_liteinst_domain_restore_phase
    pop rbx
    ret
.size reverie_liteinst_clock_constructor_finish, .-reverie_liteinst_clock_constructor_finish

.global reverie_liteinst_clock_invoke_hook
.hidden reverie_liteinst_clock_invoke_hook
.type reverie_liteinst_clock_invoke_hook,@function
reverie_liteinst_clock_invoke_hook:
    push r12
    push r13
    sub rsp, 24
    mov [rsp], rdi
    mov [rsp + 8], rax
    mov rdi, [rsp + 40]
    call reverie_liteinst_clock_enter
    mov r12, rax
    mov rdi, [rsp]
    call qword ptr [rsp + 8]
    mov r13, rax
    mov rdi, r12
    xor esi, esi
    xor edx, edx
    call reverie_liteinst_clock_leave
    mov rax, r13
    add rsp, 24
    pop r13
    pop r12
    ret
.size reverie_liteinst_clock_invoke_hook, .-reverie_liteinst_clock_invoke_hook
