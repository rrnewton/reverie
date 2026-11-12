/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * This source code is licensed under the BSD-style license in LICENSE.
 */

//! The actual shared minimal Strace Tool in the selected DBT caller runtime.

#[path = "../../strace_minimal.rs"]
pub mod strace_minimal;

include!(concat!(env!("OUT_DIR"), "/caller_identity.rs"));

reverie_dbt::export_caller_tool!(strace_minimal::StraceTool, CALLER_INPUT_IDENTITY);
