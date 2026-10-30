/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! [`BackendCapabilities::tool_reads_every_guest_writable_page`] holds under
//! the tracer: an aligned eight-byte `read_value` through `Guest::memory`
//! reads a traced guest's writable page, its write-only page and its
//! `PROT_NONE` page, and fails only at an address with no mapping, which the
//! guest cannot write either.
//!
//! The guest asks at each page with `tuxcall`, which Linux does not
//! implement; the tool answers it, and records what it read then.

#![cfg(target_arch = "x86_64")]

use std::collections::BTreeMap;
use std::sync::Mutex;

use reverie::BackendCapabilities;
use reverie::Error;
use reverie::GlobalTool;
use reverie::Guest;
use reverie::Pid;
use reverie::Subscription;
use reverie::Tool;
use reverie::syscalls::Addr;
use reverie::syscalls::MemoryAccess;
use reverie::syscalls::Syscall;
use reverie::syscalls::SyscallInfo;
use reverie::syscalls::Sysno;

/// The first argument of a guest's `tuxcall` that asks for a read.
const ASK: u64 = 0x7265_6164_7061_6765;

/// What the guest stores at offset 8 of page `index`: `VALUE + index`.
const VALUE: u64 = 0x5eed_0000_0000_0000;

/// The tool's reads, by page index: the value, or the raw errno.
#[derive(Default)]
struct Reads(Mutex<BTreeMap<u64, Result<u64, i32>>>);

#[reverie::global_tool]
impl GlobalTool for Reads {
    type Request = (u64, Result<u64, i32>);
    type Response = ();
    type Config = ();

    async fn receive_rpc(&self, _from: Pid, request: Self::Request) {
        let (index, read) = request;
        let previous = self.0.lock().unwrap().insert(index, read);
        assert!(previous.is_none(), "page {index} asked twice");
    }
}

#[derive(Default)]
struct ReadTool;

#[reverie::tool]
impl Tool for ReadTool {
    type GlobalState = Reads;
    type ThreadState = ();

    fn subscriptions(_config: &()) -> Subscription {
        [Sysno::tuxcall].into_iter().collect()
    }

    async fn handle_syscall_event<G: Guest<Self>>(
        &self,
        guest: &mut G,
        syscall: Syscall,
    ) -> Result<i64, Error> {
        let (_, args) = syscall.into_parts();
        if args.arg0 as u64 != ASK {
            return Ok(guest.inject(syscall).await?);
        }
        let (index, address) = (args.arg1 as u64, args.arg2);
        let read = guest
            .memory()
            .read_value(Addr::<u64>::from_raw(address).unwrap())
            .map_err(|errno| errno.into_raw());
        guest.send_rpc((index, read)).await;
        Ok(0)
    }
}

#[test]
fn the_tool_reads_every_page_a_traced_guest_can_write() {
    const {
        assert!(BackendCapabilities::PTRACE.tool_reads_every_guest_writable_page);
        assert!(BackendCapabilities::E9PATCH.tool_reads_every_guest_writable_page);
    };
    let reads = reverie_ptrace::testing::check_fn::<ReadTool, _>(|| {
        let page = 4096;
        // SAFETY: a fresh private anonymous mapping, used only below.
        let base = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                4 * page,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
                -1,
                0,
            )
        };
        assert_ne!(base, libc::MAP_FAILED);
        let base = base as usize;
        let word = |index: usize| (base + index * page + 8) as *mut u64;
        // SAFETY: each page is mapped readable and writable, and the word
        // is aligned.
        for index in 0..4 {
            unsafe { word(index).write(VALUE + index as u64) };
        }
        // Page 1 becomes write-only, and the guest still writes it; page 2
        // becomes PROT_NONE; page 3 is unmapped.
        // SAFETY: the pages belong to the mapping above.
        unsafe {
            assert_eq!(
                libc::mprotect((base + page) as *mut _, page, libc::PROT_WRITE),
                0
            );
            word(1).write(VALUE + 1);
            assert_eq!(
                libc::mprotect((base + 2 * page) as *mut _, page, libc::PROT_NONE),
                0
            );
            assert_eq!(libc::munmap((base + 3 * page) as *mut _, page), 0);
        }
        for index in 0..4 {
            // SAFETY: the traced tool answers this tuxcall; it changes no
            // guest memory.
            let answered =
                unsafe { libc::syscall(libc::SYS_tuxcall, ASK, index as u64, word(index)) };
            assert_eq!(answered, 0);
        }
    });
    let reads = reads.0.into_inner().unwrap();
    assert_eq!(reads.len(), 4, "{reads:?}");
    for index in 0..3 {
        assert_eq!(reads[&index], Ok(VALUE + index), "page {index}");
    }
    // PTRACE_PEEKDATA fails with EIO where nothing is mapped.
    assert_eq!(reads[&3], Err(libc::EIO), "unmapped page");
}
