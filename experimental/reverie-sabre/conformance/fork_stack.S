/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

  .text
  .globl framed_fork
  .type framed_fork, @function

framed_fork:
  pushq %rbp
  movq %rsp, %rbp
  pushq %rbx
  subq $24, %rsp

  movq $57, %rax
  syscall

  addq $24, %rsp
  popq %rbx
  popq %rbp
  ret

  .size framed_fork, .-framed_fork
  .section .note.GNU-stack,"",@progbits
