/*  Copyright © 2026 Software Reliability Group, Imperial College London
 *
 *  This file is part of SaBRe.
 *
 *  SPDX-License-Identifier: GPL-3.0-or-later
 */

/*
 * REQUIRES: rdtsc
 * RUN: %{cc} -nostdlib -nostartfiles -Wl,--no-as-needed -lc %s -o %t
 * RUN: %t
 * RUN: %{sbr} %{sbr-id} -- %t
 */

.text
.global _start
.type _start, @function
_start:
  mov $1, %rax
  cmp $2, %rax
  pushfq
  pop %r14

  mov $0, %eax
  mov $0, %ecx
  mov $0x22, %rsi
  mov $0x33, %rdi
  mov $0x44, %r8
  mov $0x55, %r9
  mov $0x66, %r10
  mov $0x77, %r11
  mov $0x88, %r12
  mov $0x99, %r13
  cpuid

  pushfq
  pop %r15
  cmp %r14, %r15
  jne fail_flags
  cmp $0x22, %rsi
  jne fail_rsi
  cmp $0x33, %rdi
  jne fail_rdi
  cmp $0x44, %r8
  jne fail_r8
  cmp $0x55, %r9
  jne fail_r9
  cmp $0x66, %r10
  jne fail_r10
  cmp $0x77, %r11
  jne fail_r11
  cmp $0x88, %r12
  jne fail_r12
  cmp $0x99, %r13
  jne fail_r13

  xor %edi, %edi
  jmp exit
fail_flags:
  mov $1, %edi
  jmp exit
fail_rsi:
  mov $2, %edi
  jmp exit
fail_rdi:
  mov $3, %edi
  jmp exit
fail_r8:
  mov $4, %edi
  jmp exit
fail_r9:
  mov $5, %edi
  jmp exit
fail_r10:
  mov $6, %edi
  jmp exit
fail_r11:
  mov $7, %edi
  jmp exit
fail_r12:
  mov $8, %edi
  jmp exit
fail_r13:
  mov $9, %edi
exit:
  mov $60, %eax
  syscall
.size _start, .-_start

.section .note.GNU-stack,"",@progbits
