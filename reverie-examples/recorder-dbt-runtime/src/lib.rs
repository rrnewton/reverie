/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * This source code is licensed under the BSD-style license in LICENSE.
 */

//! The actual shared Recorder Tool in the selected DBT caller runtime.

#[path = "../../recorder_tool.rs"]
pub mod recorder_tool;

include!(concat!(env!("OUT_DIR"), "/caller_identity.rs"));

reverie_dbt::export_caller_tool!(recorder_tool::Recorder, CALLER_INPUT_IDENTITY);
