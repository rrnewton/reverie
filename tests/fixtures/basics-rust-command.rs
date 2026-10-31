/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

#[cfg(target_arch = "x86_64")]
#[path = "../support/basics_rust_guest.rs"]
mod guest;

#[cfg(target_arch = "x86_64")]
fn main() {
    guest::run_fn_body();
    // This witnesses return from the shared body, without changing that body.
    const MARKER: &[u8] = b"basics-rust-body-complete\n";
    let written = unsafe { libc::syscall(libc::SYS_write, 1, MARKER.as_ptr(), MARKER.len()) };
    if written != MARKER.len() as libc::c_long {
        unsafe {
            libc::syscall(libc::SYS_exit_group, 97);
            core::arch::asm!("ud2", options(noreturn));
        }
    }
}

#[cfg(not(target_arch = "x86_64"))]
fn main() {
    panic!("the basics Rust command fixture requires x86_64");
}
