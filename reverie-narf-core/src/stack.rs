/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! Guest-stack scratch space built from the task's memory and registers.
//!
//! This mirrors the ptrace backend's `GuestStack`: values are staged locally
//! and written below the System V red zone (`rsp - 128`) on commit, and one
//! checkout token prevents two live stacks for the same callback. Where the
//! ptrace backend panics (a second checkout, an overflow), this stack cannot
//! panic in the kernel; it records the fault and fails the commit instead.

use alloc::sync::Arc;
use alloc::vec::Vec;
use core::mem::ManuallyDrop;
use core::sync::atomic::AtomicBool;
use core::sync::atomic::Ordering;

use reverie::Stack;
use reverie::syscalls::Addr;
use reverie::syscalls::AddrMut;
use reverie::syscalls::Errno;
use reverie::syscalls::MemoryAccess;

/// Bytes below the stack pointer that the System V ABI lets leaf code use.
pub const REDZONE_SIZE: usize = 128;

/// Scratch capacity, matching the ptrace backend.
pub const STACK_CAPACITY: usize = 1024 - REDZONE_SIZE;

struct StackToken {
    flag: Arc<AtomicBool>,
}

impl StackToken {
    fn acquire(flag: &Arc<AtomicBool>) -> Option<Self> {
        if flag.swap(true, Ordering::AcqRel) {
            None
        } else {
            Some(Self { flag: flag.clone() })
        }
    }
}

impl Drop for StackToken {
    fn drop(&mut self) {
        self.flag.store(false, Ordering::Release);
    }
}

/// Guest stack for one Narf callback.
pub struct NarfStack<M> {
    memory: M,
    top: usize,
    sp: usize,
    buf: Vec<u64>,
    fault: Option<Errno>,
    token: Option<StackToken>,
}

impl<M: MemoryAccess + Send> NarfStack<M> {
    pub(crate) fn new(memory: M, rsp: u64, flag: &Arc<AtomicBool>) -> Self {
        let token = StackToken::acquire(flag);
        let mut fault = if token.is_none() {
            Some(Errno::EBUSY)
        } else {
            None
        };
        let top = match (rsp as usize).checked_sub(REDZONE_SIZE + STACK_CAPACITY) {
            Some(_) => rsp as usize - REDZONE_SIZE,
            None => {
                fault.get_or_insert(Errno::EFAULT);
                0
            }
        };
        Self {
            memory,
            top,
            sp: top,
            buf: Vec::new(),
            fault,
            token,
        }
    }

    fn allocate<'stack, T>(&mut self, mut words: Vec<u64>) -> AddrMut<'stack, T> {
        let bytes = words.len() * core::mem::size_of::<u64>();
        if self.fault.is_some() || self.size() + bytes > STACK_CAPACITY {
            self.fault.get_or_insert(Errno::ENOMEM);
            // The address is never written: commit fails, so a Tool that
            // follows the Stack protocol never passes it to the guest.
            return AddrMut::from_raw(self.sp.max(1)).expect("nonzero address");
        }
        self.sp -= bytes;
        words.reverse();
        self.buf.extend_from_slice(&words);
        AddrMut::from_raw(self.sp).expect("guest stack address is nonzero")
    }
}

/// Keeps the stack checked out until the Tool drops it.
pub struct NarfStackGuard {
    _token: StackToken,
}

impl Drop for NarfStackGuard {
    fn drop(&mut self) {}
}

impl<M: MemoryAccess + Send> Stack for NarfStack<M> {
    type StackGuard = NarfStackGuard;

    fn size(&self) -> usize {
        self.top - self.sp
    }

    fn capacity(&self) -> usize {
        STACK_CAPACITY
    }

    fn push<'stack, T>(&mut self, value: T) -> Addr<'stack, T> {
        self.allocate(to_words(value)).into()
    }

    fn reserve<'stack, T>(&mut self) -> AddrMut<'stack, T> {
        // Zeroed guest bytes; no `T` is ever materialized in this address space.
        self.allocate(alloc::vec![0u64; core::mem::size_of::<T>().div_ceil(8)])
    }

    fn commit(mut self) -> Result<Self::StackGuard, Errno> {
        if let Some(errno) = self.fault {
            return Err(errno);
        }
        let token = self.token.take().ok_or(Errno::EBUSY)?;
        if self.buf.is_empty() {
            return Ok(NarfStackGuard { _token: token });
        }
        let remote: AddrMut<u8> = AddrMut::from_raw(self.sp).ok_or(Errno::EFAULT)?;
        self.buf.reverse();
        let mut bytes = Vec::with_capacity(self.size());
        for word in &self.buf {
            bytes.extend_from_slice(&word.to_ne_bytes());
        }
        self.memory.write_exact(remote, &bytes)?;
        Ok(NarfStackGuard { _token: token })
    }
}

/// Copies `value`'s bytes into whole words, without running its destructor.
fn to_words<T>(value: T) -> Vec<u64> {
    let value = ManuallyDrop::new(value);
    let size = core::mem::size_of::<T>();
    let mut bytes = alloc::vec![0u8; size.div_ceil(8) * 8];
    // SAFETY: `value` is a live `T` of `size` bytes; the destination holds at
    // least `size` bytes and does not overlap it.
    unsafe {
        core::ptr::copy_nonoverlapping(
            (&*value as *const T).cast::<u8>(),
            bytes.as_mut_ptr(),
            size,
        );
    }
    let (words, _) = bytes.as_chunks::<8>();
    words.iter().map(|word| u64::from_ne_bytes(*word)).collect()
}
