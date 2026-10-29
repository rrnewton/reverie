/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

use reverie::Errno;
use reverie::Guest;
use reverie::Tool;
use reverie::syscalls::MemoryAccess;

#[derive(Debug, Default, Clone)]
pub struct TestTool {}

pub const PRNG_SEED: [u8; 16] = [
    0x12, 0x34, 0x56, 0x78, 0xde, 0xad, 0xbe, 0xef, 0xca, 0xfe, 0xca, 0xfe, 0x87, 0x65, 0x43, 0x21,
];

#[reverie::tool]
impl Tool for TestTool {
    type GlobalState = ();
    type ThreadState = ();

    async fn handle_post_exec<T: Guest<Self>>(&self, guest: &mut T) -> Result<(), Errno> {
        if let Some(ptr) = guest.auxv().at_random() {
            // It is safe to mutate this address since libc has not yet had a
            // chance to modify or copy the auxv table.
            let ptr = unsafe { ptr.into_mut() };
            guest.memory().write_value(ptr, &PRNG_SEED)?;
        }

        Ok(())
    }
}
