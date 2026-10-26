// Copyright (c) Meta Platforms, Inc. and affiliates.
// Licensed under the BSD-style license in the repository LICENSE file.

#[path = "basics.rs"]
#[allow(dead_code)] // The companion client selects the other unchanged shared Tool.
mod tools;
reverie_dbt::export_caller_tool!(crate::tools::NoopTool);
