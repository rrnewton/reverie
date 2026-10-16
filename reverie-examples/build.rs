/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

fn main() {
    // The cross-backend check's direct-syscall guest (e2e/direct_guest.rs)
    // is e9patch's measured program, and e9patch's direct path runs only an
    // executable that is not position-independent.
    println!("cargo:rustc-link-arg-bin=reverie-e2e-direct=-no-pie");
}
