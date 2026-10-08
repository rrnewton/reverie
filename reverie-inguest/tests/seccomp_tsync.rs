/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! A real `SECCOMP_FILTER_FLAG_TSYNC` failure. Another thread loads a filter of
//! its own, which the runtime's filter cannot descend from, so the kernel
//! installs the runtime's filter on no thread and returns that thread's id: a
//! positive value, not an error number. `SeccompFilter::install` must refuse
//! it, naming the thread, and must not record the filter as installed. Had it
//! been installed, this thread's next syscall outside the (fictitious) gate
//! would trap and kill the process, which fails the test as well.
//!
//! A test binary without the libtest harness, so that no harness thread
//! exists besides the two threads it starts.

use std::sync::mpsc;

use reverie_inguest::seccomp::SeccompFilter;
use reverie_inguest::seccomp::TrustedGate;
use reverie_inguest::seccomp::runtime_filter_installed;

fn main() {
    // Set before the other thread starts, so that it inherits it and can load
    // a filter without privilege.
    assert_eq!(
        unsafe { libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) },
        0
    );
    let (ready, ready_receiver) = mpsc::channel();
    let (finish, finish_receiver) = mpsc::channel::<()>();
    let other = std::thread::spawn(move || {
        // BPF_RET | BPF_K, SECCOMP_RET_ALLOW: allow everything.
        let mut allow = [libc::sock_filter {
            code: 0x06,
            jt: 0,
            jf: 0,
            k: 0x7fff_0000,
        }];
        let program = libc::sock_fprog {
            len: 1,
            filter: allow.as_mut_ptr(),
        };
        // No TSYNC: this thread only.
        let loaded = unsafe {
            libc::syscall(
                libc::SYS_seccomp,
                libc::SECCOMP_SET_MODE_FILTER,
                0,
                &raw const program,
            )
        };
        assert_eq!(loaded, 0, "{}", std::io::Error::last_os_error());
        ready
            .send(unsafe { libc::syscall(libc::SYS_gettid) })
            .unwrap();
        finish_receiver.recv().unwrap();
    });
    let tid = ready_receiver.recv().unwrap();
    let mut filter = SeccompFilter::for_trusted_gate(TrustedGate {
        syscall_ip: 0x1000,
        return_ip: 0x1002,
    })
    .unwrap();
    let error = unsafe { filter.install() }
        .expect_err("TSYNC against a thread with its own filter must not install");
    assert!(
        error
            .to_string()
            .contains(&format!("could not synchronize thread {tid}")),
        "{error}"
    );
    assert!(!runtime_filter_installed());
    finish.send(()).unwrap();
    other.join().unwrap();
    println!("seccomp_tsync: refused, naming thread {tid}");
}
