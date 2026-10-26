/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! Shared basic command tests using the DBT command helper.

#[path = "tool_fixtures/basics.rs"]
mod tools;

use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;

use reverie::ExitStatus;
use reverie_test_support::dbt::test_cmd;
use tools::CounterLocal;
use tools::NoopTool;

include!("tool_fixtures/basics_commands.rs");

impl reverie_test_support::dbt::DbtTestTool for NoopTool {
    const CLIENT_ENV: &'static str = "REVERIE_DBT_NOOP_TEST_CLIENT";
    const ZERO_APPLICATION_RPC: bool = true;
}
impl reverie_test_support::dbt::DbtTestTool for CounterLocal {
    const CLIENT_ENV: &'static str = "REVERIE_DBT_COUNTER_TEST_CLIENT";
}
