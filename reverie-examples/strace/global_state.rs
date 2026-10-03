/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

// This module names only `core` and `alloc`, so it also builds without
// `std` (reverie-narf-tools compiles it for the Narf kernel).
extern crate alloc;

// The `#[reverie::global_tool]` expansion boxes each handler future by the
// bare name `Box`, which is only in the prelude with `std`.
use alloc::boxed::Box;

use reverie::GlobalTool;
use reverie::Pid;

use crate::config::Config;

#[derive(Debug, Default)]
pub struct GlobalState;

#[reverie::global_tool]
impl GlobalTool for GlobalState {
    type Request = ();
    type Response = ();
    type Config = Config;

    async fn receive_rpc(&self, _pid: Pid, _req: Self::Request) {}
}
