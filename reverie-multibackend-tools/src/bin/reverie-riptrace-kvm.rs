/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! Syscall tracer (`reverie-tool-riptrace`) over the KVM backend.
//!
//! Runs a single static ELF given on the command line (bounded KVM prototype).

use reverie_tool_riptrace::RipTrace;

fn main() -> anyhow::Result<()> {
    let (_global, code) = reverie_multibackend_tools::run_kvm_static_elf::<RipTrace>(())?;
    std::process::exit(code);
}
