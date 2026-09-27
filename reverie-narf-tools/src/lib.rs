/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! Backend-neutral `reverie::Tool`s shared by the Narf kernel backend and the
//! Linux hosts it is compared against.
//!
//! Every Tool here names only `core` and `alloc`, so the same source builds
//! into the Narf kernel (through `reverie-narf-core`) and into a `std` host
//! (through `reverie-ptrace`). A Tool that has to print takes a [`LineSink`]
//! type parameter: the Tool's code, and therefore the bytes it formats, is the
//! same in every host; only where the finished line goes differs.

#![no_std]

extern crate alloc;

pub mod canonical;
pub mod passthrough;
pub mod probe;

/// counter1, compiled unmodified from `reverie-examples`.
#[allow(dead_code)]
#[path = "../../reverie-examples/counter1_tool.rs"]
pub mod counter1;

/// counter2, compiled unmodified from `reverie-examples`. Without `std` it
/// prints its thread-exit line only through a reporter the backend sets.
#[allow(dead_code)]
#[path = "../../reverie-examples/counter2_tool.rs"]
pub mod counter2;

/// Where a printing Tool delivers each finished line.
///
/// `emit` receives one complete line without its terminating newline. The
/// sink appends the newline and must deliver the line atomically with respect
/// to other lines, so concurrent threads cannot interleave within a line.
pub trait LineSink: 'static {
    /// Delivers one line.
    fn emit(line: &str);
}
