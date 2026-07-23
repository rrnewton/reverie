/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! A minimal `strace`-like Reverie tool running over the KVM Guest interface.
//!
//! This mirrors `reverie-examples/strace_minimal.rs` (the ptrace-backed tool)
//! but drives the tool through `KvmBackend::run_with_tool`. It demonstrates that
//! the shared `Tool`/`Guest` contract is sufficient to observe every guest
//! syscall, decode its arguments, execute it, and report the result -- with no
//! backend-specific tool code.

#![cfg(target_arch = "x86_64")]

use std::sync::Mutex;

use kvm_ioctls::Kvm;
use reverie::Error;
use reverie::GlobalTool;
use reverie::Guest;
use reverie::Pid;
use reverie::Tool;
use reverie::syscalls::Displayable;
use reverie::syscalls::Syscall;
use reverie_kvm::GuestMemory;
use reverie_kvm::KvmBackend;
use reverie_kvm::SyscallRequest;

// The message buffer is placed well above the guest program page (ENTRY_POINT)
// and the consecutive per-syscall transport frames (FRAME_ADDRESS + n*4096) so
// the three regions never overlap.
const MEMORY_SIZE: usize = 0x2_0000;
const ENTRY_POINT: u64 = 0x1000;
const FRAME_ADDRESS: u64 = 0x2000;
const MESSAGE_ADDRESS: u64 = 0x1_0000;

fn kvm_is_unavailable(error: &kvm_ioctls::Error) -> bool {
    matches!(error.errno(), libc::ENOENT | libc::EACCES | libc::EPERM)
}

/// Accumulates one formatted `strace` line per intercepted syscall.
///
/// Collecting through a `GlobalTool` keeps the trace on the same Reverie RPC
/// contract that `run_with_tool` hands back to the caller, exactly as the
/// `RecordingGlobal` tool in `tests/vmcall.rs` does for its event bitmask.
#[derive(Default)]
struct StraceLog {
    lines: Mutex<Vec<String>>,
}

#[reverie::global_tool]
impl GlobalTool for StraceLog {
    type Request = String;
    type Response = ();
    type Config = ();

    async fn receive_rpc(&self, _from: Pid, line: String) {
        self.lines
            .lock()
            .expect("strace log mutex poisoned")
            .push(line);
    }
}

/// The strace tool itself. It subscribes to every syscall by default
/// (`Tool::subscriptions` is `Subscription::all_syscalls()`), so the KVM runtime
/// routes each guest vmcall into `handle_syscall_event`.
#[derive(Default)]
struct StraceTool;

#[reverie::tool]
impl Tool for StraceTool {
    type GlobalState = StraceLog;
    type ThreadState = ();

    async fn handle_syscall_event<G: Guest<Self>>(
        &self,
        guest: &mut G,
        syscall: Syscall,
    ) -> Result<i64, Error> {
        // Decode the call against guest memory, matching the ptrace-backed
        // `strace_minimal` example. Syscall names and scalar arguments are
        // decoded (e.g. `brk(NULL)`); raw byte-buffer pointers render as guest
        // addresses.
        let rendered = format!("{}", syscall.display_with_outputs(&guest.memory()));
        // Execute the syscall through the Guest contract (this routes to the
        // backend executor) and report the real result, like `strace`.
        let ret = match guest.inject(syscall).await {
            Ok(value) => value,
            Err(errno) => -(errno.into_raw() as i64),
        };
        let line = format!("[pid {}] {} = {}", guest.tid(), rendered, ret);
        eprintln!("{line}");
        guest.send_rpc(line).await;
        Ok(ret)
    }
}

/// A stand-in for the guest kernel's syscall execution. `write` reports the
/// number of bytes requested; every other call succeeds with 0. This keeps the
/// trace hermetic and deterministic while still exercising real return values.
fn demo_executor(request: &SyscallRequest, _memory: &GuestMemory) -> i64 {
    if request.number() == libc::SYS_write as u64 {
        request.args()[2] as i64
    } else {
        0
    }
}

#[test]
fn strace_tool_traces_a_simple_guest_program() {
    match Kvm::new() {
        Ok(_) => {}
        Err(error) if kvm_is_unavailable(&error) => {
            eprintln!("skipping KVM strace test: cannot open /dev/kvm: {error}");
            return;
        }
        Err(error) => panic!("failed to probe /dev/kvm: {error}"),
    }

    let mut backend = KvmBackend::new(MEMORY_SIZE).unwrap();
    backend
        .memory_mut()
        .write(MESSAGE_ADDRESS, b"hello")
        .unwrap();

    // A tiny "program": write(1, "hello", 5); close(7); brk(0).
    let requests = [
        SyscallRequest::new(libc::SYS_write as u64, [1, MESSAGE_ADDRESS, 5, 0, 0, 0]),
        SyscallRequest::new(libc::SYS_close as u64, [7, 0, 0, 0, 0, 0]),
        SyscallRequest::new(libc::SYS_brk as u64, [0, 0, 0, 0, 0, 0]),
    ];
    backend
        .install_syscalls(ENTRY_POINT, FRAME_ADDRESS, &requests)
        .unwrap();

    let log =
        futures::executor::block_on(backend.run_with_tool::<StraceTool, _>((), demo_executor))
            .unwrap();

    let lines = log.lines.lock().unwrap();
    assert_eq!(
        lines.len(),
        3,
        "expected one trace line per syscall: {lines:?}"
    );
    assert!(
        lines[0].contains("write") && lines[0].contains("= 5"),
        "first line should decode write(1, ..., 5) = 5 (executor returned the byte count): {}",
        lines[0]
    );
    assert!(
        lines[1].contains("close") && lines[1].contains("= 0"),
        "second line should be close(...) = 0: {}",
        lines[1]
    );
    assert!(
        lines[2].contains("brk") && lines[2].contains("= 0"),
        "third line should be brk(...) = 0: {}",
        lines[2]
    );
}
