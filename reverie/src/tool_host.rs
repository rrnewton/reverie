/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! Shared ownership and callback drivers for in-process Tool hosts.
//!
//! This module has no trap, native loader, allocator or concrete RPC dependency.
//! Adapters supply physical Guest operations and a borrowed GlobalRPC port.

mod driver;
mod owner;
pub mod sync;

pub use driver::*;
pub use owner::*;
