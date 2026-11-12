/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * This source code is licensed under the BSD-style license in LICENSE.
 */

//! The actual generic basics counter Tool in the selected caller runtime.

#[path = "../../../tool_fixtures/basics.rs"]
pub mod tools;

include!(concat!(env!("OUT_DIR"), "/caller_identity.rs"));

reverie_dbt::export_caller_tool!(tools::CounterLocal, CALLER_INPUT_IDENTITY);
