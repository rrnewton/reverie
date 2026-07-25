/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! Syscall counter (`reverie-tool-sysctr`) over the KVM backend.
//!
//! Runs a single static ELF given on the command line (bounded KVM prototype).

use reverie_tool_sysctr::SysCtr;

fn main() -> anyhow::Result<()> {
    let (global, code) = reverie_multibackend_tools::run_kvm_static_elf::<SysCtr>(())?;
    reverie_tool_sysctr::report(&global);
    std::process::exit(code);
}
