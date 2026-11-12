/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! The actual standalone injection Tool in the selected caller runtime.

#[path = "../../../tool_fixtures/inject_then_tail.rs"]
pub mod inject_then_tail_tool;

include!(concat!(env!("OUT_DIR"), "/caller_identity.rs"));

reverie_dbt::export_caller_tool!(inject_then_tail_tool::TestTool, CALLER_INPUT_IDENTITY);
