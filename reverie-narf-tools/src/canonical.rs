/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! The canonical-trace Tool: one line per syscall event, in a fixed format
//! that every backend running the same guest must reproduce byte for byte.
//!
//! Format `narf-hermit-canonical-v1`. Each record is
//! `INFO narf-hermit-canonical-v1 seq=<n> phase=<phase> ...`, where `seq`
//! counts the syscalls of the recording thread from 0:
//!
//! * `write`: `phase=enter nr=<write> a0=<fd> a1=0x<buf:016x> a2=<len>`, then, after
//!   the Tool has run the write with a non-tail inject,
//!   `phase=return result=<ret>` (a negative `ret` is `-errno`).
//! * `exit`: `phase=enter nr=<exit> a0=<status>`; the Tool then tail-injects it.
//! * Anything else: `phase=enter nr=<nr> unsupported`, then a tail inject.
//!   Syscall numbers are the target's (`write` = 1 and `exit` = 60 on
//!   x86_64). The v1 format admits only `write` and `exit`, so a validator rejects any
//!   trace containing such a record; the Tool still lets the guest proceed so
//!   the whole trace is available for diagnosis.

use alloc::boxed::Box;
use alloc::format;
use core::marker::PhantomData;

use reverie::Error;
use reverie::Guest;
use reverie::Tool;
use reverie::syscalls::Syscall;
use reverie::syscalls::SyscallInfo;
use reverie::syscalls::Sysno;

use crate::LineSink;

/// The prefix every canonical record starts with.
pub const PREFIX: &str = "INFO narf-hermit-canonical-v1 ";

/// Emits the canonical-v1 trace of every syscall through `S`.
pub struct CanonicalTrace<S> {
    sink: PhantomData<fn() -> S>,
}

impl<S> Default for CanonicalTrace<S> {
    fn default() -> Self {
        Self { sink: PhantomData }
    }
}

impl<S> core::fmt::Debug for CanonicalTrace<S> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("CanonicalTrace").finish_non_exhaustive()
    }
}

#[reverie::tool]
impl<S: LineSink> Tool for CanonicalTrace<S> {
    type GlobalState = ();
    type ThreadState = u64;

    async fn handle_syscall_event<T: Guest<Self>>(
        &self,
        guest: &mut T,
        syscall: Syscall,
    ) -> Result<i64, Error> {
        let seq = *guest.thread_state();
        *guest.thread_state_mut() += 1;
        let (sysno, args) = syscall.into_parts();
        match sysno {
            Sysno::write => {
                S::emit(&format!(
                    "{PREFIX}seq={seq} phase=enter nr={} a0={} a1=0x{:016x} a2={}",
                    sysno.id(),
                    args.arg0,
                    args.arg1,
                    args.arg2
                ));
                let result = match guest.inject(syscall).await {
                    Ok(value) => value,
                    Err(errno) => -i64::from(errno.into_raw()),
                };
                S::emit(&format!("{PREFIX}seq={seq} phase=return result={result}"));
                Ok(result)
            }
            Sysno::exit => {
                S::emit(&format!(
                    "{PREFIX}seq={seq} phase=enter nr={} a0={}",
                    sysno.id(),
                    args.arg0
                ));
                guest.tail_inject(syscall).await
            }
            other => {
                S::emit(&format!(
                    "{PREFIX}seq={seq} phase=enter nr={} unsupported",
                    other.id()
                ));
                guest.tail_inject(syscall).await
            }
        }
    }
}
