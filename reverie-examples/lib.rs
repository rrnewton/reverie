/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! Reusable implementations of Reverie's example tools.

// TODO-HUMAN-REVIEW(PR-128): Review exposing the example tools to other backends.
#[path = "chaos.rs"]
pub mod chaos;
#[path = "chunky_print.rs"]
pub mod chunky_print;
#[path = "noop.rs"]
pub mod noop;
