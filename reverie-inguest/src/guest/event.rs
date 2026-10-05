/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! The syscall event that every trap and hook path builds.

/// How a guest syscall reached the runtime.
#[derive(Clone, Copy)]
pub enum SyscallDispatch {
    /// Through the seccomp `SIGSYS` trap.
    Trap,
    /// Through a patched site's installed hook.
    InstalledHook,
    /// Through the in-guest fallback continuation.
    Fallback,
}

/// One guest syscall as the runtime sees it: its number and arguments, where
/// it came from, the protection-key value to run it under, and the result the
/// guest will see.
#[derive(Clone, Copy)]
pub struct SyscallEvent {
    /// The syscall number.
    pub number: i64,
    /// The six argument registers.
    pub args: [u64; 6],
    /// The address of the guest instruction that made the call.
    pub instruction_pointer: u64,
    /// The value returned to the guest (a negative errno on failure).
    pub result: i64,
    /// The dispatch path's register context, as an address.
    pub context: usize,
    /// How the call reached the runtime.
    pub dispatch: SyscallDispatch,
    /// The guest's PKRU value to run the call under, when it is not the runtime's.
    pub guest_pkru: Option<u32>,
}

impl SyscallEvent {
    /// Forward only this guest operation. Runtime-private syscall buffers must
    /// retain caller access and continue to use the ordinary raw gate.
    ///
    /// # Safety
    ///
    /// Issues the guest's syscall with its own arguments through the trusted
    /// gate; the caller must be on a guest syscall path.
    pub unsafe fn forward(&mut self) -> i64 {
        let result = unsafe {
            crate::trap::raw_syscall6_with_result(self.number, self.args, self.guest_pkru)
        };
        // Permission effects survive negative errno and later Tool result
        // transformation. Private injection never calls this operation.
        self.guest_pkru = result.pkru;
        result.result
    }
}
