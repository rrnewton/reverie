// Copyright (c) Meta Platforms, Inc. and affiliates.
// Licensed under the BSD-style license in the repository LICENSE file.

#[path = "native_controls.rs"]
mod tools;
reverie_dbt::export_caller_tool!(crate::tools::ControlTool<false>);
