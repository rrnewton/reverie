/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * This source code is licensed under the BSD-style license in LICENSE.
 */

//! The actual shared Chaos Tool in the selected DBT caller runtime.

#[path = "../../chaos.rs"]
pub mod chaos;

include!(concat!(env!("OUT_DIR"), "/caller_identity.rs"));

reverie_dbt::export_caller_tool!(chaos::ChaosTool, CALLER_INPUT_IDENTITY);
