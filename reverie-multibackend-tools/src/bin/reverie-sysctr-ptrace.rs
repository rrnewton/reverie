/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! Syscall counter (`reverie-tool-sysctr`) over the ptrace backend.

use reverie_tool_sysctr::SysCtr;

#[tokio::main]
async fn main() -> Result<(), reverie::Error> {
    let (status, global) = reverie_multibackend_tools::run_ptrace::<SysCtr>(()).await?;
    reverie_tool_sysctr::report(&global);
    status.raise_or_exit()
}
