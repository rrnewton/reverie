/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * This source code is licensed under the BSD-style license in LICENSE.
 */

//! The actual generic injected-exit Tool in the selected caller runtime.

#[path = "../../../tool_fixtures/exit.rs"]
pub mod exit_tool;

include!(concat!(env!("OUT_DIR"), "/caller_identity.rs"));

reverie_dbt::export_caller_tool!(exit_tool::InjectExitTool, CALLER_INPUT_IDENTITY);
