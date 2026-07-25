/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! Backend-agnostic strace-like syscall tracer Reverie tool.
//!
//! Like [`reverie_tool_sysctr`](../reverie_tool_sysctr/index.html), this crate
//! defines the tool *once* against only the `reverie` trait crate so the same
//! [`RipTrace`] implementation can be linked into a binary against any backend.
//! Each intercepted syscall is printed to stderr with its decoded arguments and
//! return value, using the portable [`reverie::Guest`] API (`inject` /
//! `tail_inject` / `memory`) that every backend implements.
//!
//! It traces every syscall (no filtering), keeping the tool dependency-light;
//! richer filtering belongs in a binary's argument parsing, not this shared
//! tool.

use reverie::Error;
use reverie::GlobalTool;
use reverie::Guest;
use reverie::Pid;
use reverie::Tool;
use reverie::syscalls::Displayable;
use reverie::syscalls::Syscall;
use reverie::syscalls::SyscallInfo;

/// Trivial global state: the tracer prints directly and needs no aggregation.
#[derive(Debug, Default)]
pub struct RipTraceGlobal;

#[reverie::global_tool]
impl GlobalTool for RipTraceGlobal {
    type Request = ();
    type Response = ();
    type Config = ();

    async fn receive_rpc(&self, _from: Pid, _req: Self::Request) -> Self::Response {}
}

/// The per-guest tracer tool.
#[derive(Debug, Default, Clone)]
pub struct RipTrace;

#[reverie::tool]
impl Tool for RipTrace {
    type GlobalState = RipTraceGlobal;
    type ThreadState = ();

    async fn handle_syscall_event<T: Guest<Self>>(
        &self,
        guest: &mut T,
        syscall: Syscall,
    ) -> Result<i64, Error> {
        match syscall {
            Syscall::Exit(_) | Syscall::ExitGroup(_) => {
                // The process is about to disappear; its return can't be
                // observed, so print before injecting.
                eprintln!(
                    "[pid {}] {} = ?",
                    guest.tid().colored(),
                    syscall.display_with_outputs(&guest.memory()),
                );
                guest.tail_inject(syscall).await
            }
            Syscall::Execve(_) | Syscall::Execveat(_) => {
                let tid = guest.tid();
                // Must be pre-formatted: on a successful execve the original
                // program image (and its memory references) is wiped out.
                eprintln!(
                    "[pid {}] {}",
                    tid.colored(),
                    syscall.display_with_outputs(&guest.memory()),
                );
                let errno = guest.inject(syscall).await.unwrap_err();
                eprintln!(
                    "[pid {}] ({}) = {:?}",
                    tid.colored(),
                    syscall.number(),
                    errno
                );
                Err(errno.into())
            }
            _otherwise => {
                let ret = guest.inject(syscall).await;
                eprintln!(
                    "[pid {}] {} = {}",
                    guest.tid().colored(),
                    syscall.display_with_outputs(&guest.memory()),
                    ret.unwrap_or_else(|errno| -errno.into_raw() as i64),
                );
                Ok(ret?)
            }
        }
    }
}
