/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! Standalone `LD_PRELOAD` leaf for the shared in-guest runtime.
//!
//! The shared runtime in `reverie-inguest` is an rlib so backends can embed it
//! and install it after their own bootstrap. This cdylib-only leaf is the
//! standalone preload: its only job is to register the core's constructor with
//! the dynamic loader. Keeping the constructor here, instead of behind a core
//! Cargo feature, means every build graph that links this artifact produces a
//! constructor-bearing `libreverie_inguest.so`; no dependent package's
//! `default-features = false` selection can leave an inert variant behind at
//! the unhashed artifact path.

#![forbid(unsafe_op_in_unsafe_fn)]

#[cfg(not(all(target_os = "linux", target_arch = "x86_64")))]
compile_error!("reverie-inguest-preload requires Linux x86-64");

#[used]
#[unsafe(link_section = ".init_array")]
static REVERIE_INGUEST_INIT: unsafe extern "C" fn() =
    reverie_inguest_core::reverie_inguest_initialize;
