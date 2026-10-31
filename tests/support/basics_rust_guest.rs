/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

// The original run_fn_test guest body, shared with the ordinary command.
pub fn run_fn_body() {
    let pid = nix::unistd::getpid();
    println!("Hello world1!  Pid = {:?}", pid);
    unsafe {
        libc::syscall(libc::SYS_write, 1, "Hello world2!\n", 14);
    }
    let _gid = nix::unistd::getgid();
}
