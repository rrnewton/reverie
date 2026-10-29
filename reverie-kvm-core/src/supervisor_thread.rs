/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! The host threads that drive guest processes and guest threads, and their
//! stack size.
//!
//! A supervisor thread runs one guest process's or one guest thread's whole
//! loop: its vCPU entries, LinuxEmu's syscall emulation and the Tool's
//! callbacks, under one `futures::executor::block_on`. Its stack holds that
//! loop's future and every frame a Tool callback needs. The supervisor threads
//! are the forked-process threads (`reverie-kvm-process-<pid>`) and the guest
//! thread workers (`reverie-kvm-guest-<tid>`), Tool-owned or host-owned.
//!
//! A thread spawned without an explicit size gets `RUST_MIN_STACK`, or Rust's
//! 2 MiB default, and a debug build overflows 2 MiB on these threads. So every
//! supervisor thread is spawned with [`stack_bytes`] of stack.
//!
//! Two more threads get the same size because they can destroy Tool state,
//! whose destructors can be as deep as a Tool's callbacks:
//! - `reverie-kvm-reaper` retires a run whose future was dropped. It drops
//!   that run's executors, and with them any Tool future the run had not yet
//!   polled (a failed spawn's cleanup holds the child's Tool and thread
//!   state), plus the panic payloads it collects while joining.
//! - `reverie-kvm-join` joins guest workers and receives each one's result
//!   and panic payload.
//!
//! `kvm-task-file-exit` keeps the default. It only hands a task's host socket
//! owners to the exit broker and catches its own panics, so no Tool or
//! executor state ever reaches it. It used at most 14 KiB in a debug build.
//!
//! The run's first process is not on a supervisor thread. It runs on whichever
//! embedder thread polls the run, and needs about as much stack there: 2,092
//! KiB below the run's entry frame in a debug build, 247 KiB in a release
//! build, in the measurements below. An embedder should poll a run from a
//! thread with at least [`KVM_SUPERVISOR_THREAD_STACK_BYTES`] of stack.

use std::ffi::OsStr;
use std::sync::OnceLock;

/// The stack every supervisor thread is spawned with, unless `RUST_MIN_STACK`
/// asks for more (see [`stack_bytes`]).
///
/// Measured high-water marks (the lowest stack page a thread touched, below
/// its entry frame) over the KVM integration tests of a large production Tool
/// (fork trees, nested signal handlers, POSIX and interval timers, exec and
/// waits) and over Reverie's own KVM suites:
///
/// | build   | forked-process thread | guest thread | Reverie's own Tools |
/// |---------|-----------------------|--------------|---------------------|
/// | debug   | 2,054 KiB             | 2,029 KiB    | 974 KiB             |
/// | release | 235 KiB               | 230 KiB      | 159 KiB             |
///
/// Spawned with exactly 2,176 KiB, 12 of the 25 debug runs that use these
/// threads overflowed; with 2,240 KiB none did. In release, 256 KiB overflowed
/// all 25 and 320 KiB none. 16 MiB is about seven times the debug minimum,
/// enough for a Tool to grow without changing this. It is reserved address
/// space: physical memory is allocated only for the pages a thread touches
/// (about 2.2 MiB in a debug build and 300 KiB in a release build). Two host
/// settings change that. With transparent huge pages set to `always`, the
/// touched top of the stack can be backed by 2 MiB pages. With strict
/// overcommit (`vm.overcommit_memory=2`), the whole reservation counts against
/// the commit limit.
pub const KVM_SUPERVISOR_THREAD_STACK_BYTES: usize = 16 << 20;

/// The stack size to spawn a supervisor thread with
/// (`std::thread::Builder::stack_size`).
///
/// It is the larger of [`KVM_SUPERVISOR_THREAD_STACK_BYTES`] and
/// `RUST_MIN_STACK`, read once, as Rust's standard library reads it. An
/// explicit size replaces `RUST_MIN_STACK` for the thread it is given to, so
/// taking the larger keeps that variable able to raise the stack for a Tool
/// that needs more, and never lets it shrink the stack below what the backend
/// needs.
pub fn stack_bytes() -> usize {
    static BYTES: OnceLock<usize> = OnceLock::new();
    *BYTES.get_or_init(|| stack_bytes_for(std::env::var_os("RUST_MIN_STACK").as_deref()))
}

/// [`stack_bytes`] for a given `RUST_MIN_STACK` value. As in the standard
/// library, a value that is not a decimal byte count is ignored.
fn stack_bytes_for(rust_min_stack: Option<&OsStr>) -> usize {
    rust_min_stack
        .and_then(|value| value.to_str()?.parse::<usize>().ok())
        .map_or(KVM_SUPERVISOR_THREAD_STACK_BYTES, |bytes| {
            bytes.max(KVM_SUPERVISOR_THREAD_STACK_BYTES)
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rust_min_stack_can_raise_the_supervisor_stack_but_not_lower_it() {
        let size = |value: Option<&str>| stack_bytes_for(value.map(OsStr::new));
        assert_eq!(size(None), KVM_SUPERVISOR_THREAD_STACK_BYTES);
        assert_eq!(size(Some("33554432")), 32 << 20);
        assert_eq!(size(Some("1048576")), KVM_SUPERVISOR_THREAD_STACK_BYTES);
        assert_eq!(size(Some("")), KVM_SUPERVISOR_THREAD_STACK_BYTES);
        assert_eq!(size(Some("16M")), KVM_SUPERVISOR_THREAD_STACK_BYTES);
    }

    #[test]
    fn a_supervisor_thread_gets_at_least_the_supervisor_stack() {
        let handle = std::thread::Builder::new()
            .name("supervisor-stack-test".to_owned())
            .stack_size(stack_bytes())
            .spawn(|| {
                // SAFETY: pthread_getattr_np initializes `attr`, which is
                // destroyed before this returns.
                unsafe {
                    let mut attr = std::mem::zeroed::<libc::pthread_attr_t>();
                    assert_eq!(libc::pthread_getattr_np(libc::pthread_self(), &mut attr), 0);
                    let mut size = 0;
                    assert_eq!(libc::pthread_attr_getstacksize(&attr, &mut size), 0);
                    libc::pthread_attr_destroy(&mut attr);
                    (std::thread::current().name().map(str::to_owned), size)
                }
            })
            .unwrap();
        let (name, size) = handle.join().unwrap();
        assert_eq!(name.as_deref(), Some("supervisor-stack-test"));
        assert!(
            size >= KVM_SUPERVISOR_THREAD_STACK_BYTES,
            "a supervisor thread got {size} bytes of stack"
        );
    }
}
