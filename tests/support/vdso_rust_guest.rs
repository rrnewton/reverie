/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

pub fn clock_getres() {
    let mut res = libc::timespec {
        tv_sec: 1,
        tv_nsec: 0,
    };

    let ret = unsafe { libc::clock_getres(libc::CLOCK_MONOTONIC, &mut res as *mut _) };

    assert_eq!(ret, 0);

    assert_eq!(res.tv_sec, 0);
    assert_eq!(res.tv_nsec, 42);
}
