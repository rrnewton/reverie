.section .tbss,"awT",@nobits
.p2align 3
.hidden reverie_liteinst_domain_state
.type reverie_liteinst_domain_state,@tls_object
.size reverie_liteinst_domain_state,32
reverie_liteinst_domain_state:
    .zero 32
.text

.macro domain_address
    mov rax, qword ptr [rip + reverie_liteinst_domain_state@GOTTPOFF]
    add rax, qword ptr fs:[0]
.endm

.global reverie_liteinst_domain_enter
.hidden reverie_liteinst_domain_enter
.type reverie_liteinst_domain_enter,@function
reverie_liteinst_domain_enter:
    domain_address
    mov qword ptr [rax], 1
.global reverie_liteinst_domain_enter_depth
.hidden reverie_liteinst_domain_enter_depth
reverie_liteinst_domain_enter_depth:
    add qword ptr [rax + 8], 1
    mov qword ptr [rax], 1
    ret
.size reverie_liteinst_domain_enter, .-reverie_liteinst_domain_enter

.global reverie_liteinst_domain_leave
.hidden reverie_liteinst_domain_leave
.type reverie_liteinst_domain_leave,@function
reverie_liteinst_domain_leave:
    domain_address
    sub qword ptr [rax + 8], 1
    mov rdx, 1
    mov rcx, 2
    cmovnz rcx, rdx
    mov qword ptr [rax], rcx
    ret
.size reverie_liteinst_domain_leave, .-reverie_liteinst_domain_leave

.macro domain_read name, offset
    .global \name
    .hidden \name
    .type \name,@function
\name:
    domain_address
    mov rax, [rax + \offset]
    ret
    .size \name, .-\name
.endm

.macro domain_adjust name, offset, amount
    .global \name
    .hidden \name
    .type \name,@function
\name:
    domain_address
    add qword ptr [rax + \offset], \amount
    ret
    .size \name, .-\name
.endm

domain_read reverie_liteinst_domain_phase, 0
domain_read reverie_liteinst_domain_depth, 8
.global reverie_liteinst_domain_restore_phase
.hidden reverie_liteinst_domain_restore_phase
.type reverie_liteinst_domain_restore_phase,@function
reverie_liteinst_domain_restore_phase:
    domain_address
    mov [rax], rdi
    ret
.size reverie_liteinst_domain_restore_phase, .-reverie_liteinst_domain_restore_phase
domain_read reverie_liteinst_installation_depth, 16
domain_read reverie_liteinst_allocation_depth, 24
domain_adjust reverie_liteinst_installation_enter, 16, 1
domain_adjust reverie_liteinst_installation_leave, 16, -1
domain_adjust reverie_liteinst_allocation_enter, 24, 1
domain_adjust reverie_liteinst_allocation_leave, 24, -1
