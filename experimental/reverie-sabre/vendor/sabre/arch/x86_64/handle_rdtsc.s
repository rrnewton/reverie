/*  Copyright © 2019 Software Reliability Group, Imperial College London
 *
 *  This file is part of SaBRe.
 *
 *  SPDX-License-Identifier: GPL-3.0-or-later
 */

  .file "handle_rdtsc.s"
  .text
  .globl rdtsc_entrypoint
  .internal rdtsc_entrypoint
  .type rdtsc_entrypoint, @function

rdtsc_entrypoint:
  .cfi_startproc
  .cfi_def_cfa rsp, 0x88
  .cfi_offset rip, -0x88

  # RDTSC leaves RFLAGS unchanged. Save them before stack alignment or the
  # plugin call can alter them, and restore them immediately before returning.
  pushfq
  .cfi_adjust_cfa_offset 8
  .cfi_remember_state
  cld # C and Rust callbacks require a clear direction flag under SysV.

  # Prologue
  push %rbp
  .cfi_adjust_cfa_offset 8
  mov %rsp, %rbp
  .cfi_def_cfa_register rbp
  .cfi_remember_state

  # Save the registers
  pushq %rbx
  pushq %rcx
  pushq %rdx
  pushq %rsi
  pushq %rdi
  pushq %r8
  pushq %r9
  pushq %r10
  pushq %r11
  pushq %r12
  pushq %r13
  pushq %r14
  pushq %r15

  # Align the stack on a 16-byte boundary before the call
  push %rbp
  mov %rsp, %rbp
  .cfi_adjust_cfa_offset 0x70
  and $0xfffffffffffffff0, %rsp

  # Call the actual handler
  call *plugin_rdtsc_handler(%rip)

  # Move high part of rax to rdx
  mov %rax, %rdx
  mov $0x00000000FFFFFFFF, %r15
  and %r15, %rax
  shr $32, %rdx
  and %r15, %rdx
  mov %rdx, 88(%rbp) # Save rdx in its stable slot above the aligned stack

  # Restore the stack
  mov %rbp, %rsp
  pop %rbp
  .cfi_restore_state

  # Reload registers
  popq %r15
  popq %r14
  popq %r13
  popq %r12
  popq %r11
  popq %r10
  popq %r9
  popq %r8
  popq %rdi
  popq %rsi
  popq %rdx
  popq %rcx
  popq %rbx

  # Epilogue
  pop %rbp
  .cfi_restore_state
  popfq
  .cfi_adjust_cfa_offset -8
  leaq 8(%rsp), %rsp # drop fake return address without changing RFLAGS
  .cfi_undefined rip
  ret
  .cfi_endproc
  .size rdtsc_entrypoint, .-rdtsc_entrypoint

  .globl rdtscp_entrypoint
  .internal rdtscp_entrypoint
  .type rdtscp_entrypoint, @function

rdtscp_entrypoint:
  .cfi_startproc
  .cfi_def_cfa rsp, 0x88
  .cfi_offset rip, -0x88

  # RDTSCP leaves RFLAGS unchanged. Save them before stack alignment or the
  # plugin call can alter them, and restore them immediately before returning.
  pushfq
  .cfi_adjust_cfa_offset 8
  .cfi_remember_state
  cld # C and Rust callbacks require a clear direction flag under SysV.

  # Prologue
  push %rbp
  .cfi_adjust_cfa_offset 8
  mov %rsp, %rbp
  .cfi_def_cfa_register rbp
  .cfi_remember_state

  # Save the registers
  pushq %rbx
  pushq %rcx
  pushq %rdx
  pushq %rsi
  pushq %rdi
  pushq %r8
  pushq %r9
  pushq %r10
  pushq %r11
  pushq %r12
  pushq %r13
  pushq %r14
  pushq %r15

  # Align the stack on a 16-byte boundary before the call
  push %rbp
  mov %rsp, %rbp
  .cfi_adjust_cfa_offset 0x70
  and $0xfffffffffffffff0, %rsp

  # Call the actual handler
  call *plugin_rdtsc_handler(%rip)

  # Move high part of rax to rdx
  mov %rax, %rdx
  mov $0x00000000FFFFFFFF, %r15
  and %r15, %rax
  shr $32, %rdx
  and %r15, %rdx
  mov %rdx, 88(%rbp) # Save rdx in its stable slot above the aligned stack

  # Restore the stack
  mov %rbp, %rsp
  pop %rbp
  .cfi_restore_state

  # Reload registers. RDTSCP writes TSC_AUX to ecx; expose deterministic CPU 0.
  popq %r15
  popq %r14
  popq %r13
  popq %r12
  popq %r11
  popq %r10
  popq %r9
  popq %r8
  popq %rdi
  popq %rsi
  popq %rdx
  popq %rcx
  mov $0, %ecx
  popq %rbx

  # Epilogue
  pop %rbp
  .cfi_restore_state
  popfq
  .cfi_adjust_cfa_offset -8
  leaq 8(%rsp), %rsp # drop fake return address without changing RFLAGS
  .cfi_undefined rip
  ret
  .cfi_endproc
  .size rdtscp_entrypoint, .-rdtscp_entrypoint

  .globl cpuid_entrypoint
  .internal cpuid_entrypoint
  .type cpuid_entrypoint, @function

cpuid_entrypoint:
  .cfi_startproc
  .cfi_def_cfa rsp, 0x88
  .cfi_offset rip, -0x88

  # CPUID leaves RFLAGS unchanged. Preserve every non-output register while
  # the plugin computes EAX/EBX/ECX/EDX from the guest input EAX/ECX pair.
  pushfq
  .cfi_adjust_cfa_offset 8
  .cfi_remember_state
  cld

  push %rbp
  .cfi_adjust_cfa_offset 8
  mov %rsp, %rbp
  .cfi_def_cfa_register rbp
  .cfi_remember_state

  pushq %rax
  pushq %rbx
  pushq %rcx
  pushq %rdx
  pushq %rsi
  pushq %rdi
  pushq %r8
  pushq %r9
  pushq %r10
  pushq %r11
  pushq %r12
  pushq %r13
  pushq %r14
  pushq %r15

  push %rbp
  mov %rsp, %rbp
  .cfi_adjust_cfa_offset 0x78
  and $0xfffffffffffffff0, %rsp

  mov 112(%rbp), %edi
  mov 96(%rbp), %esi
  call *plugin_cpuid_handler(%rip)

  # A 16-byte integer struct returns in RAX:RDX as four packed u32 fields.
  # Store zero-extended architectural outputs into the saved guest slots.
  mov %rax, %r14
  mov %rdx, %r15
  mov %r14d, %r13d
  mov %r13, 112(%rbp)
  shr $32, %r14
  mov %r14d, %r13d
  mov %r13, 104(%rbp)
  mov %r15d, %r13d
  mov %r13, 96(%rbp)
  shr $32, %r15
  mov %r15d, %r13d
  mov %r13, 88(%rbp)

  mov %rbp, %rsp
  pop %rbp
  .cfi_restore_state

  popq %r15
  popq %r14
  popq %r13
  popq %r12
  popq %r11
  popq %r10
  popq %r9
  popq %r8
  popq %rdi
  popq %rsi
  popq %rdx
  popq %rcx
  popq %rbx
  popq %rax

  pop %rbp
  .cfi_restore_state
  popfq
  .cfi_adjust_cfa_offset -8
  leaq 8(%rsp), %rsp
  .cfi_undefined rip
  ret
  .cfi_endproc
  .size cpuid_entrypoint, .-cpuid_entrypoint
  .section .note.GNU-stack,"",@progbits
