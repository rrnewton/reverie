/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * This source code is licensed under the BSD-style license in LICENSE.
 */

//! The original ChromeTrace Tool compiled as an explicit external DBT caller.
//!
//! These sibling modules share the Tool, event types and GlobalState directly
//! with the public example. No preload or guest constructor is linked here.

#[path = "../../chrome-trace/event.rs"]
pub mod event;
#[path = "../../chrome-trace/global_state.rs"]
pub mod global_state;
#[path = "../../chrome-trace/tool.rs"]
pub mod tool;

include!(concat!(env!("OUT_DIR"), "/caller_identity.rs"));

reverie_dbt::export_caller_tool!(tool::ChromeTrace, CALLER_INPUT_IDENTITY);
