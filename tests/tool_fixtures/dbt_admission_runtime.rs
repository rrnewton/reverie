// Copyright (c) Meta Platforms, Inc. and affiliates.
// Licensed under the BSD-style license in the repository LICENSE file.
#[path = "dbt_admission.rs"]
mod tools;
reverie_dbt::export_caller_tool!(crate::tools::AdmissionTool);
