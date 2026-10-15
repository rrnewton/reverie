/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

/// A page that is reserved by Reverie in every guest process.
pub const PRIVATE_PAGE_OFFSET: usize = 0x7100_0000;

/// trampoline data from private pages
pub const TRAMPOLINE_BASE: usize = PRIVATE_PAGE_OFFSET;
pub const TRAMPOLINE_SIZE: usize = 0x1000;

/// total private page size
/// A read-write page after the trampoline, for data Reverie's own
/// injections pass to the kernel (a SIGTRAP repair's `struct sigaction`,
/// <https://github.com/rrnewton/hermit/issues/4409>). The trampoline page is
/// made read-only once populated; this one stays writable.
pub const SCRATCH_BASE: usize = TRAMPOLINE_BASE + TRAMPOLINE_SIZE;
pub const SCRATCH_SIZE: usize = 0x1000;

pub const PRIVATE_PAGE_SIZE: usize = TRAMPOLINE_SIZE + SCRATCH_SIZE;

/// The size of the `ud2` instruction on x86_64.
#[cfg(target_arch = "x86_64")]
pub const UD_INSTR_SIZE: usize = 1;

/// The size of the `udf` instruction on aarch64.
#[cfg(target_arch = "aarch64")]
pub const UD_INSTR_SIZE: usize = 4;

/// The size of the syscall instruction. On x86_64, this is 2 bytes.
#[cfg(target_arch = "x86_64")]
pub const SYSCALL_INSTR_SIZE: usize = 2;

/// The size of the syscall instruction. On aarch64, this is 4 bytes.
#[cfg(target_arch = "aarch64")]
pub const SYSCALL_INSTR_SIZE: usize = 4;
