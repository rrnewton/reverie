/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! A Tool that intercepts every syscall and changes nothing.

// The `#[reverie::tool]` expansion boxes each handler future by the bare name
// `Box`, which is only in the prelude with `std`.
use alloc::boxed::Box;

use reverie::Error;
use reverie::Guest;
use reverie::Tool;
use reverie::syscalls::Syscall;

/// Tail-injects every syscall it is handed, unmodified.
///
/// Under a correct backend a guest run under `PassThrough` is observably
/// identical to the same guest run with no Tool at all.
#[derive(Debug, Default, Clone, Copy)]
pub struct PassThrough;

#[reverie::tool]
impl Tool for PassThrough {
    type GlobalState = ();
    type ThreadState = ();

    async fn handle_syscall_event<T: Guest<Self>>(
        &self,
        guest: &mut T,
        syscall: Syscall,
    ) -> Result<i64, Error> {
        guest.tail_inject(syscall).await
    }
}
