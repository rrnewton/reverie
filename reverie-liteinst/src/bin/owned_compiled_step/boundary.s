.text
.global compiled_entry
.hidden compiled_entry
.type compiled_entry,@function
compiled_entry:
    sub rsp, 24
    mov [rsp], rdi
    call {clock_begin}
    mov [rsp + 8], rax
    mov rdi, [rsp]
    call {initialize}
    mov edi, eax
    mov rsi, [rsp + 8]
    call {clock_finish}
    mov qword ptr [rip + {init_phase}], 7
    mov qword ptr [rsp + 16], 0x7979
    mov ebx, 0x7171
    mov ebp, 0x7272
    mov r12d, 0x7373
    mov r13d, 0x7474
    mov r14d, 0x7575
    mov r15d, 0x7676
    mov [rip + {results} + 128], rsp
    .irp register,16,17,18,19,20,21,22,23,24,25,26,27,28,29,30,31
    vpternlogd zmm\register, zmm\register, zmm\register, 0xff
    .endr
    fninit
    fld1
    fldpi
    vpcmpeqd ymm0, ymm0, ymm0
    vpcmpeqd ymm1, ymm1, ymm1
    lea rsi, [rip + {before}]
    mov eax, 0x2e7
    xor edx, edx
    xsave64 [rsi]
    mov eax, 39
.global compiled_first
.hidden compiled_first
compiled_first:
    syscall
    mov [rip + {results}], rax
    lea rdi, [rip + {data}]
    mov esi, {words}
    mov rdx, {seed}
    call {workload}
    mov [rip + {results} + 144], rbx
    mov [rip + {results} + 152], rbp
    mov [rip + {results} + 160], r12
    mov [rip + {results} + 168], r13
    mov [rip + {results} + 176], r14
    mov [rip + {results} + 184], r15
    mov [rip + {results} + 8], rax
    mov eax, 0xfeed
    mov ecx, 0xbaad
    cpuid
    mov [rip + {results} + 16], rax
    mov [rip + {results} + 24], rbx
    mov [rip + {results} + 32], rcx
    mov [rip + {results} + 40], rdx
    rdtsc
    mov [rip + {results} + 48], rax
    mov [rip + {results} + 56], rbx
    mov [rip + {results} + 64], rcx
    mov [rip + {results} + 72], rdx
    rdtscp
    mov [rip + {results} + 80], rax
    mov [rip + {results} + 88], rbx
    mov [rip + {results} + 96], rcx
    mov [rip + {results} + 104], rdx
    mov rdi, [rip + {pipe_fd}]
    lea rsi, [rip + {buffer} + 8]
    mov edx, 39
    xor eax, eax
    syscall
    mov [rip + {results} + 112], rax
    mov eax, 39
    syscall
    mov [rip + {results} + 120], rax
    mov [rip + {results} + 136], rsp
    lea rsi, [rip + {after}]
    mov eax, 0x2e7
    xor edx, edx
    xsave64 [rsi]
    mov rax, [rsp + 16]
    mov [rip + {results} + 192], rax
    xor edi, edi
    call reverie_liteinst_clock_enter
    mov rdi, [rsp]
    call {verify}
    ud2
.size compiled_entry, .-compiled_entry
