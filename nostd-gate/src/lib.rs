/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! Positive gate: the contract crates build and are usable with only `core`
//! and `alloc`.

#![no_std]

extern crate alloc;

use reverie_memory::Addr;
use reverie_memory::IoSlice;
use reverie_memory::IoSliceMut;
use reverie_memory::MemoryAccess;
use syscalls::Errno;

/// A same-address-space memory accessor, the shape a Narf backend provides.
pub struct SameAddressSpace;

impl MemoryAccess for SameAddressSpace {
    fn read_vectored(&self, from: &[IoSlice], to: &mut [IoSliceMut]) -> Result<usize, Errno> {
        copy(from, to)
    }

    fn write_vectored(&mut self, from: &[IoSlice], to: &mut [IoSliceMut]) -> Result<usize, Errno> {
        copy(from, to)
    }
}

fn copy(from: &[IoSlice], to: &mut [IoSliceMut]) -> Result<usize, Errno> {
    let mut total = 0;
    for (src, dst) in from.iter().zip(to.iter_mut()) {
        let n = src.len().min(dst.len());
        dst[..n].copy_from_slice(&src[..n]);
        total += n;
    }
    Ok(total)
}

/// Exercises an `alloc`-returning default method of the shared trait.
pub fn read_string(memory: &SameAddressSpace, addr: Addr<u8>) -> alloc::ffi::CString {
    memory.read_cstring(addr).unwrap_or_default()
}

/// Exercises the process-identity types the `Tool` contract names.
pub fn describe_exit(pid: reverie_process::Pid, raw: i32) -> Option<reverie_process::Signal> {
    use reverie_process::ExitStatus;
    use reverie_process::Signal;

    let _raw_pid: i32 = pid.as_raw();
    match ExitStatus::from_raw(raw) {
        ExitStatus::Exited(_) => None,
        ExitStatus::Signaled(Signal::SIGSEGV, _) => Some(Signal::SIGSEGV),
        ExitStatus::Signaled(sig, _) => Signal::try_from(sig as i32).ok(),
    }
}

/// Exercises the syscall types the `Tool` contract hands to tools: decoding,
/// `Display` against guest memory, reading a path argument, and the libc
/// layouts guest memory is read as.
pub fn describe_syscall(
    memory: &SameAddressSpace,
    sysno: syscalls::Sysno,
    args: syscalls::SyscallArgs,
) -> alloc::string::String {
    use core::fmt::Write;

    use reverie_syscalls::Displayable;
    use reverie_syscalls::ReadAddr;
    use reverie_syscalls::Syscall;

    let syscall = Syscall::from_raw(sysno, args);
    let mut out = alloc::string::String::new();
    let _ = write!(out, "{}", syscall.display(memory));
    if let Syscall::Openat(openat) = syscall {
        // Without `std` a path is read as the `CString` of its bytes.
        let path: Option<alloc::ffi::CString> = openat.path().and_then(|p| p.read(memory).ok());
        let _ = write!(out, " {path:?} {:?}", openat.flags());
    }
    let _ = write!(
        out,
        " stat={}",
        core::mem::size_of::<reverie_syscalls::libc::stat>()
    );
    out
}
