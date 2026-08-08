/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! Ptrace-side glue for LiteInst instrumentation statistics.
//!
//! The counters themselves now live in the shared `reverie` crate so the in-guest path can
//! populate the same type; see `reverie::liteinst_stats`. What remains here is only the part that
//! is genuinely ptrace-specific: the outcomes a ptrace-hosted patch attempt can produce, and the
//! lock helper used from `task.rs`.

pub use reverie::LiteinstInstrumentationStats;
pub use reverie::LiteinstInstrumentationStatsHandle;
pub(crate) use reverie::LiteinstPatchOutcome;
pub(crate) use reverie::with_liteinst_stats;
