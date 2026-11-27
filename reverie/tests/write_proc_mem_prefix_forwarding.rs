/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! `Guest::write_proc_mem_prefix` through the Guest wrappers (`IntoGuest`
//! and `NativeOperationGuest`) reaches the wrapped guest's own store, not
//! the trait's `ENOSYS` default: the wrapped guest records each call and
//! returns a short prefix, so a wrapper that dropped its forward fails here.

use reverie::Error;
use reverie::GlobalRPC;
use reverie::Guest;
use reverie::IntoGuest;
use reverie::NativeOperationGuest;
use reverie::Never;
use reverie::Pid;
use reverie::Stack;
use reverie::TimerSchedule;
use reverie::Tool;
use reverie::syscalls::Addr;
use reverie::syscalls::AddrMut;
use reverie::syscalls::Errno;
use reverie::syscalls::LocalMemory;
use reverie::syscalls::SyscallInfo;

#[derive(Default)]
struct TestStack;

#[derive(Default)]
struct TestStackGuard;

impl Drop for TestStackGuard {
    fn drop(&mut self) {}
}

impl Stack for TestStack {
    type StackGuard = TestStackGuard;

    fn size(&self) -> usize {
        0
    }

    fn capacity(&self) -> usize {
        0
    }

    fn push<'stack, T>(&mut self, _value: T) -> Addr<'stack, T> {
        panic!("test stack is never used")
    }

    fn reserve<'stack, T>(&mut self) -> AddrMut<'stack, T> {
        panic!("test stack is never used")
    }

    fn commit(self) -> Result<Self::StackGuard, Errno> {
        Ok(TestStackGuard)
    }
}

/// A thread state that is its own view (`AsRef` and `AsMut` of itself), as
/// `IntoGuest` asks of the state of the Tool it adapts.
type State = Vec<u8>;

/// A Tool that is its own view, as `IntoGuest` asks of the Tool it adapts.
#[derive(Default)]
struct StoreTool;

impl AsMut<StoreTool> for StoreTool {
    fn as_mut(&mut self) -> &mut StoreTool {
        self
    }
}

#[reverie::tool]
impl Tool for StoreTool {
    type GlobalState = ();
    type ThreadState = State;
}

/// A guest whose forced store records its input and stores only three bytes.
#[derive(Default)]
struct StoringGuest {
    thread_state: State,
    stored: Vec<(u64, Vec<u8>)>,
}

#[reverie::tool]
impl GlobalRPC<()> for StoringGuest {
    async fn send_rpc(&self, _message: ()) {}

    fn config(&self) -> &() {
        &()
    }
}

#[reverie::tool]
impl Guest<StoreTool> for StoringGuest {
    type Memory = LocalMemory;
    type Stack = TestStack;

    fn tid(&self) -> Pid {
        Pid::from_raw(2)
    }

    fn pid(&self) -> Pid {
        Pid::from_raw(2)
    }

    fn ppid(&self) -> Option<Pid> {
        None
    }

    // A test fake with no backend runtime.
    fn is_backend_runtime_bootstrap(&self) -> bool {
        false
    }

    fn memory(&self) -> Self::Memory {
        LocalMemory::new()
    }

    fn write_proc_mem_prefix(&mut self, address: u64, bytes: &[u8]) -> Result<usize, Errno> {
        self.stored.push((address, bytes.to_vec()));
        Ok(bytes.len().min(3))
    }

    fn thread_state_mut(&mut self) -> &mut State {
        &mut self.thread_state
    }

    fn thread_state(&self) -> &State {
        &self.thread_state
    }

    async fn regs(&mut self) -> libc::user_regs_struct {
        // SAFETY: all-zero general registers are sufficient for this
        // callback-only test.
        unsafe { std::mem::zeroed() }
    }

    async fn stack(&mut self) -> Self::Stack {
        TestStack
    }

    async fn daemonize(&mut self) {}

    async fn inject<S: SyscallInfo>(&mut self, _syscall: S) -> Result<i64, Errno> {
        Err(Errno::ENOSYS)
    }

    async fn tail_inject<S: SyscallInfo>(&mut self, _syscall: S) -> Never {
        panic!("test guest cannot tail-inject")
    }

    fn set_timer(&mut self, _sched: TimerSchedule) -> Result<(), Error> {
        Ok(())
    }

    fn set_timer_precise(&mut self, _sched: TimerSchedule) -> Result<(), Error> {
        Ok(())
    }

    fn read_clock(&mut self) -> Result<u64, Error> {
        Ok(0)
    }
}

/// The fake itself stores a three-byte prefix and records the whole input,
/// which the trait default (`ENOSYS`) cannot be confused with.
#[test]
fn the_fake_tells_its_store_from_the_default() {
    let mut guest = StoringGuest::default();
    assert_eq!(
        <StoringGuest as Guest<StoreTool>>::write_proc_mem_prefix(&mut guest, 0x1000, b"abcdef",),
        Ok(3)
    );
    assert_eq!(guest.stored, [(0x1000, b"abcdef".to_vec())]);
}

/// `IntoGuest` forwards the store to the guest it adapts.
#[test]
fn into_guest_forwards_the_forced_store() {
    let mut guest = StoringGuest::default();
    {
        let mut wrapper = IntoGuest::<StoringGuest, StoreTool>::new(&mut guest);
        assert_eq!(
            <IntoGuest<'_, StoringGuest, StoreTool> as Guest<StoreTool>>::write_proc_mem_prefix(
                &mut wrapper,
                0x2000,
                b"abcdef",
            ),
            Ok(3)
        );
    }
    assert_eq!(guest.stored, [(0x2000, b"abcdef".to_vec())]);
}

/// `NativeOperationGuest`, with no operation bound, forwards the store to
/// the guest it wraps.
#[test]
fn native_operation_guest_forwards_the_forced_store() {
    let mut guest = StoringGuest::default();
    {
        let mut wrapper =
            NativeOperationGuest::<StoringGuest, StoreTool>::optional(&mut guest, None)
                .unwrap_or_else(|_| panic!("a wrapper with no operation needs no support"));
        assert_eq!(
            <NativeOperationGuest<'_, StoringGuest, StoreTool> as Guest<StoreTool>>::write_proc_mem_prefix(
                &mut wrapper,
                0x3000,
                b"abcdef",
            ),
            Ok(3)
        );
    }
    assert_eq!(guest.stored, [(0x3000, b"abcdef".to_vec())]);
}
