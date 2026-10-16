/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! A corpus program for the cross-backend check (`corpus.tsv`, `direct`)
//! whose own executable makes every syscall it makes after startup, each
//! with a `syscall` instruction, and which ends with `exit_group` itself.
//!
//! The rest of the corpus makes its syscalls through libc, and e9patch's
//! direct path hosts a tool only in a program whose main executable makes
//! syscalls: this program is e9patch's measured cell, and a direct-syscall
//! program on every other backend.
//!
//! `main` is C's, which glibc calls once the program's constructors (and an
//! in-guest backend's preload) have run, so no Rust runtime start-up runs
//! between them and its first call.

#![no_main]

use std::arch::asm;
use std::ffi::c_char;
use std::ffi::c_int;

/// # Safety
///
/// The arguments must be valid for syscall `number`.
unsafe fn syscall(number: libc::c_long, a: usize, b: usize, c: usize) -> isize {
    let result: isize;
    // SAFETY: the caller passes arguments valid for `number`; the kernel
    // clobbers rcx and r11 and nothing else.
    unsafe {
        asm!(
            "syscall",
            inlateout("rax") number as isize => result,
            in("rdi") a,
            in("rsi") b,
            in("rdx") c,
            in("r10") 0usize,
            in("r8") 0usize,
            in("r9") 0usize,
            lateout("rcx") _,
            lateout("r11") _,
            options(nostack),
        );
    }
    result
}

#[unsafe(no_mangle)]
pub extern "C" fn main(_argc: c_int, _argv: *const *const c_char) -> c_int {
    let hello = b"direct\n";
    let mut buffer = [0u8; 256];
    // SAFETY: each call's pointers name live buffers of the lengths passed.
    unsafe {
        syscall(libc::SYS_getpid, 0, 0, 0);
        syscall(libc::SYS_getuid, 0, 0, 0);
        syscall(libc::SYS_write, 1, hello.as_ptr() as usize, hello.len());
        let missing = c"/nonexistent/reverie-e2e-direct";
        syscall(
            libc::SYS_openat,
            libc::AT_FDCWD as usize,
            missing.as_ptr() as usize,
            libc::O_RDONLY as usize,
        );
        let hostname = c"/etc/hostname";
        let fd = syscall(
            libc::SYS_openat,
            libc::AT_FDCWD as usize,
            hostname.as_ptr() as usize,
            (libc::O_RDONLY | libc::O_CLOEXEC) as usize,
        );
        if fd >= 0 {
            syscall(
                libc::SYS_read,
                fd as usize,
                buffer.as_mut_ptr() as usize,
                buffer.len(),
            );
            syscall(libc::SYS_close, fd as usize, 0, 0);
        }
        syscall(libc::SYS_uname, buffer.as_mut_ptr() as usize, 0, 0);
        syscall(libc::SYS_exit_group, 0, 0, 0);
    }
    unreachable!("exit_group returned")
}
