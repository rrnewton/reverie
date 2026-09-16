.intel_syntax noprefix
.text
.global capture_pair
.type capture_pair,@function
capture_pair:
    sub rsp, 16
    stmxcsr [rsp]
    fnstcw [rsp + 4]
    test edx, edx
    jnz .Lnonzero
    .irp register,16,17,18,19,20,21,22,23,24,25,26,27,28,29,30,31
    vmovdqu64 zmm\register, [rsi]
    .endr
    jmp .Lcapture
.Lnonzero:
    .irp register,16,17,18,19,20,21,22,23,24,25,26,27,28,29,30,31
    vpternlogd zmm\register, zmm\register, zmm\register, 0xff
    .endr
.Lcapture:
    fninit
    fld1
    fldpi
    vpcmpeqd ymm0, ymm0, ymm0
    vpcmpeqd ymm1, ymm1, ymm1
    mov eax, 0x2e7
    xor edx, edx
    xsave64 [rdi]
    test ecx, ecx
    jz .Lafter
    xrstor64 [rdi]
.Lafter:
    xsave64 [rsi]
    fninit
    fldcw [rsp + 4]
    ldmxcsr [rsp]
    vzeroupper
    add rsp, 16
    ret
.size capture_pair, .-capture_pair
.section .note.GNU-stack,"",@progbits
