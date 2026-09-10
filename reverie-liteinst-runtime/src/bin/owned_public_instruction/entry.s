.text
.global public_entry
.hidden public_entry
.type public_entry,@function
public_entry:
    sub rsp, 24
    mov [rsp], rdi
    call {clock_begin}
    mov [rsp + 8], rax
    mov rdi, [rsp]
    call {initialize}
    mov edi, eax
    mov rsi, [rsp + 8]
    call {clock_finish}
    mov eax, 39
.global public_first
.hidden public_first
public_first:
    syscall
    mov [rip + {results}], rax
    mov ecx, {branches}
.Lwork_a:
    dec ecx
    jnz .Lwork_a
    mov eax, 0xfeed
    mov ecx, 0xbaad
    cpuid
    mov [rip + {results} + 8], rax
    mov [rip + {results} + 16], rbx
    mov [rip + {results} + 24], rcx
    mov [rip + {results} + 32], rdx
    mov ecx, {branches}
.Lwork_b:
    dec ecx
    jnz .Lwork_b
    mov ecx, {rdtsc_ecx_seed}
    rdtsc
    mov [rip + {results} + 40], rax
    mov [rip + {results} + 48], rbx
    mov [rip + {results} + 56], rcx
    mov [rip + {results} + 64], rdx
    mov ecx, {branches}
.Lwork_c:
    dec ecx
    jnz .Lwork_c
    rdtscp
    mov [rip + {results} + 72], rax
    mov [rip + {results} + 80], rbx
    mov [rip + {results} + 88], rcx
    mov [rip + {results} + 96], rdx
    mov ecx, {branches}
.Lwork_d:
    dec ecx
    jnz .Lwork_d
    mov rdi, [rip + {pipe_fd}]
    lea rsi, [rip + {buffer}]
    mov edx, {read_bytes}
    xor eax, eax
    syscall
    mov [rip + {results} + 104], rax
    mov ecx, {branches}
.Lwork_e:
    dec ecx
    jnz .Lwork_e
    mov eax, 39
    syscall
    mov [rip + {results} + 112], rax
    xor edi, edi
    call reverie_liteinst_clock_enter
    mov rdi, [rsp]
    call {verify}
    ud2
.size public_entry, .-public_entry
