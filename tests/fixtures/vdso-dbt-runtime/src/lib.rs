/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * This source code is licensed under the BSD-style license in LICENSE.
 */

//! The actual generic vDSO Tool in the selected caller runtime.

#[path = "../../../tool_fixtures/vdso.rs"]
pub mod vdso_tool;

include!(concat!(env!("OUT_DIR"), "/caller_identity.rs"));

reverie_dbt::export_caller_tool!(vdso_tool::LocalState, CALLER_INPUT_IDENTITY);
