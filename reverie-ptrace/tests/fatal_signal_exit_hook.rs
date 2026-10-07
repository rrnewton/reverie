/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! `GlobalTool::on_fatal_signal_exit` runs once for a thread a core-dumping
//! signal kills, while the thread is held at its exit stop with its memory
//! still mapped, and never for a thread that exits any other way.

use std::fs::File;
use std::os::unix::fs::FileExt;
use std::sync::Mutex;

use reverie::ExitStatus;
use reverie::FatalSignalExit;
use reverie::GlobalTool;
use reverie::Pid;
use reverie::Signal;
use reverie::Subscription;
use reverie::Tool;
use reverie_ptrace::testing::test_fn_with_config;

const BEFORE: &[u8; 16] = b"written-pre-fork";
const DYING: &[u8; 16] = b"written-by-guest";

/// A page the test process allocates before the guest is forked from it, so
/// the guest has it at the same address.
fn shared_page() -> usize {
    let page = Box::leak(Box::new([0u8; 4096]));
    page[..BEFORE.len()].copy_from_slice(BEFORE);
    page.as_ptr() as usize
}

#[derive(Debug)]
struct Seen {
    tid: Pid,
    signal: Signal,
    status: ExitStatus,
    rip: u64,
    /// The guest's bytes at the page, read through `/proc/<tid>/mem` from
    /// inside the hook.
    page: Vec<u8>,
}

#[derive(Default)]
struct FatalLog {
    page: Mutex<usize>,
    seen: Mutex<Vec<Seen>>,
}

#[reverie::global_tool]
impl GlobalTool for FatalLog {
    type Request = ();
    type Response = ();
    type Config = usize;

    async fn init_global_state(page: &usize) -> Self {
        Self {
            page: Mutex::new(*page),
            seen: Mutex::new(Vec::new()),
        }
    }

    async fn receive_rpc(&self, _from: Pid, _request: ()) {}

    fn on_fatal_signal_exit(&self, exit: &FatalSignalExit) {
        let page = *self.page.lock().unwrap();
        let mut bytes = vec![0; DYING.len()];
        let mem = File::open(format!("/proc/{}/mem", exit.tid.as_raw()))
            .expect("the dying thread's memory should still be open");
        mem.read_exact_at(&mut bytes, page as u64)
            .expect("the dying thread's memory should still be mapped");
        self.seen.lock().unwrap().push(Seen {
            tid: exit.tid,
            signal: exit.signal,
            status: exit.status,
            rip: exit.regs.rip,
            page: bytes,
        });
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct FatalLogTool;

#[reverie::tool]
impl Tool for FatalLogTool {
    type GlobalState = FatalLog;
    type ThreadState = ();

    fn subscriptions(_config: &usize) -> Subscription {
        Subscription::none()
    }
}

fn run(guest: impl FnOnce(usize)) -> (ExitStatus, Vec<Seen>) {
    let page = shared_page();
    let (output, log) = test_fn_with_config::<FatalLogTool, _>(|| guest(page), page, true)
        .expect("the guest should run");
    (output.status, log.seen.into_inner().unwrap())
}

#[test]
fn a_segfault_is_offered_once_with_its_memory_still_mapped() {
    let (status, seen) = run(|page| unsafe {
        std::ptr::copy_nonoverlapping(DYING.as_ptr(), page as *mut u8, DYING.len());
        // The dangling pointer is address 8, which is never mapped.
        std::ptr::dangling_mut::<u64>().write_volatile(1);
    });
    assert_eq!(status, ExitStatus::Signaled(Signal::SIGSEGV, true));
    assert_eq!(seen.len(), 1, "{seen:?}");
    let seen = &seen[0];
    assert_eq!(seen.signal, Signal::SIGSEGV);
    assert_eq!(seen.status, status);
    assert!(seen.tid.as_raw() > 0);
    assert_ne!(seen.rip, 0);
    assert_eq!(
        seen.page,
        DYING,
        "the hook read {:?}, not what the guest wrote before it died",
        String::from_utf8_lossy(&seen.page)
    );
}

#[test]
fn an_abort_is_offered() {
    let (status, seen) = run(|_| std::process::abort());
    assert_eq!(status, ExitStatus::Signaled(Signal::SIGABRT, true));
    assert_eq!(seen.len(), 1, "{seen:?}");
    assert_eq!(seen[0].signal, Signal::SIGABRT);
}

#[test]
fn signals_that_dump_no_core_and_ordinary_exits_are_not_offered() {
    let (status, seen) = run(|_| unsafe {
        libc::kill(libc::getpid(), libc::SIGTERM);
    });
    assert_eq!(status, ExitStatus::Signaled(Signal::SIGTERM, false));
    assert!(seen.is_empty(), "{seen:?}");

    let (status, seen) = run(|_| unsafe { libc::_exit(3) });
    assert_eq!(status, ExitStatus::Exited(3));
    assert!(seen.is_empty(), "{seen:?}");
}
