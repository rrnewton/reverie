/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! Whether this process is a host-fork copy of a run (PROTOTYPE).
//!
//! `fork()` copies only the calling thread. A copy of a run therefore lacks
//! every helper thread the run started, and must never wait for one: the
//! wait would never end. Code that joins such a thread asks
//! [`is_forked_child`] first.

use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;

static FORKED_CHILD: AtomicBool = AtomicBool::new(false);

/// Records that this process is a forked copy of a run, before any repair.
pub fn mark_forked_child() {
    FORKED_CHILD.store(true, Ordering::Relaxed);
}

/// Whether [`mark_forked_child`] ran in this process.
pub fn is_forked_child() -> bool {
    FORKED_CHILD.load(Ordering::Relaxed)
}
