/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! Syscall counter (`reverie-tool-sysctr`) over the DBI (DynamoRIO) backend.
//!
//! Links the shared tool against the DBI backend and launches the guest under
//! the DynamoRIO client. See [`reverie_multibackend_tools::run_dbi`] for the
//! tool-embedding caveat.

use reverie_tool_sysctr::SysCtr;

fn main() -> anyhow::Result<()> {
    let status = reverie_multibackend_tools::run_dbi::<SysCtr>()?;
    std::process::exit(status.code().unwrap_or(1));
}
