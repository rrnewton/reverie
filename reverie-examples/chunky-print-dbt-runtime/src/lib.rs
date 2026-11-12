/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * This source code is licensed under the BSD-style license in LICENSE.
 */

//! The actual shared ChunkyPrint Tool in the selected DBT caller runtime.

#[path = "../../chunky_print.rs"]
pub mod chunky_print;

include!(concat!(env!("OUT_DIR"), "/caller_identity.rs"));

reverie_dbt::export_caller_tool!(chunky_print::ChunkyPrintLocal, CALLER_INPUT_IDENTITY);
