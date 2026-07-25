/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! Syscall tracer (`reverie-tool-riptrace`) over the ptrace backend.

use reverie_tool_riptrace::RipTrace;

#[tokio::main]
async fn main() -> Result<(), reverie::Error> {
    let (status, _global) = reverie_multibackend_tools::run_ptrace::<RipTrace>(()).await?;
    status.raise_or_exit()
}
