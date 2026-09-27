/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! The `reverie::Tool` contract with `std` off, driven by a minimal backend.
//!
//! `NarfGuest` implements `reverie::Guest` the way an in-kernel backend can:
//! syscalls go to a kernel dispatch function, RPCs to the global state call
//! it directly, and the auxiliary vector is the one the backend built.
//! `handle_syscall` runs a tool's `handle_syscall_event` to completion or to
//! its tail injection.
//!
//! The tools here are reverie-examples' existing counter1 and counter2 tools,
//! compiled from their own source files, not copies. The tests below drive
//! counter1; `narf_core` drives both through the Narf execution core.

use alloc::boxed::Box;
use core::future::Future;
use core::pin::pin;
use core::task::Context;
use core::task::Poll;
use core::task::Waker;

use reverie::Auxv;
use reverie::Error;
use reverie::GlobalRPC;
use reverie::GlobalTool;
use reverie::Guest;
use reverie::Never;
use reverie::Pid;
use reverie::Stack;
use reverie::TimerSchedule;
use reverie::Tool;
use reverie::syscalls::Addr;
use reverie::syscalls::AddrMut;
use reverie::syscalls::Errno;
use reverie::syscalls::Syscall;
use reverie::syscalls::SyscallInfo;
use reverie::syscalls::libc;
use syscalls::SyscallArgs;
use syscalls::Sysno;

use crate::SameAddressSpace;

#[path = "../../reverie-examples/counter1_tool.rs"]
pub mod counter1_tool;
#[path = "../../reverie-examples/counter2_tool.rs"]
pub mod counter2_tool;

/// Compiles only while reverie is built without `std`: with it, `Error` has
/// an `Io` variant and this match is not exhaustive.
pub fn error_errno(error: Error) -> Option<Errno> {
    match error {
        Error::Errno(errno) => Some(errno),
        Error::Tool(_) => None,
    }
}

/// The kernel's syscall entry, as the backend sees it.
pub type KernelDispatch = fn(Sysno, SyscallArgs) -> Result<i64, Errno>;

/// A guest thread of a backend that shares the tool's address space.
pub struct NarfGuest<'a, T: Tool> {
    tid: Pid,
    thread_state: T::ThreadState,
    global: &'a T::GlobalState,
    config: &'a <T::GlobalState as GlobalTool>::Config,
    kernel: KernelDispatch,
    tail_result: Option<Result<i64, Errno>>,
}

impl<'a, T: Tool> NarfGuest<'a, T> {
    /// Creates the guest for thread `tid`.
    pub fn new(
        tid: Pid,
        global: &'a T::GlobalState,
        config: &'a <T::GlobalState as GlobalTool>::Config,
        kernel: KernelDispatch,
    ) -> Self {
        Self {
            tid,
            thread_state: Default::default(),
            global,
            config,
            kernel,
            tail_result: None,
        }
    }
}

/// A guest stack with no capacity: this backend offers tools no scratch
/// space on the guest stack.
pub struct NarfStack;

/// Guard for [`NarfStack`].
pub struct NarfStackGuard;

impl Drop for NarfStackGuard {
    fn drop(&mut self) {}
}

impl Stack for NarfStack {
    type StackGuard = NarfStackGuard;

    fn size(&self) -> usize {
        0
    }

    fn capacity(&self) -> usize {
        0
    }

    fn push<'stack, V>(&mut self, _value: V) -> Addr<'stack, V> {
        panic!("NarfStack has no capacity")
    }

    fn reserve<'stack, V>(&mut self) -> AddrMut<'stack, V> {
        panic!("NarfStack has no capacity")
    }

    fn commit(self) -> Result<Self::StackGuard, Errno> {
        Ok(NarfStackGuard)
    }
}

// `reverie::tool` is reverie's name for `async_trait`.
#[reverie::tool]
impl<T: Tool> GlobalRPC<T::GlobalState> for NarfGuest<'_, T> {
    async fn send_rpc(
        &self,
        message: <T::GlobalState as GlobalTool>::Request,
    ) -> <T::GlobalState as GlobalTool>::Response {
        // The global state lives in the same address space: call it.
        self.global.receive_rpc(self.tid, message).await
    }

    fn config(&self) -> &<T::GlobalState as GlobalTool>::Config {
        self.config
    }
}

#[reverie::tool]
impl<T: Tool> Guest<T> for NarfGuest<'_, T> {
    type Memory = SameAddressSpace;
    type Stack = NarfStack;

    fn tid(&self) -> Pid {
        self.tid
    }

    fn pid(&self) -> Pid {
        self.tid
    }

    fn ppid(&self) -> Option<Pid> {
        None
    }

    fn auxv(&self) -> Auxv {
        Auxv::from_entries([(libc::AT_UID, 0), (libc::AT_GID, 0)])
    }

    fn memory(&self) -> Self::Memory {
        SameAddressSpace
    }

    fn thread_state_mut(&mut self) -> &mut T::ThreadState {
        &mut self.thread_state
    }

    fn thread_state(&self) -> &T::ThreadState {
        &self.thread_state
    }

    async fn regs(&mut self) -> libc::user_regs_struct {
        // SAFETY: every field of `user_regs_struct` is an integer.
        unsafe { core::mem::zeroed() }
    }

    async fn stack(&mut self) -> Self::Stack {
        NarfStack
    }

    async fn daemonize(&mut self) {}

    async fn inject<S: SyscallInfo>(&mut self, syscall: S) -> Result<i64, Errno> {
        let (sysno, args) = syscall.into_parts();
        (self.kernel)(sysno, args)
    }

    async fn tail_inject<S: SyscallInfo>(&mut self, syscall: S) -> Never {
        let (sysno, args) = syscall.into_parts();
        self.tail_result = Some((self.kernel)(sysno, args));
        // The handler ends here. `handle_syscall` sees the result and drops
        // the handler's future.
        core::future::pending().await
    }

    fn set_timer(&mut self, _sched: TimerSchedule) -> Result<(), Error> {
        Err(Errno::ENOSYS.into())
    }

    fn set_timer_precise(&mut self, _sched: TimerSchedule) -> Result<(), Error> {
        Err(Errno::ENOSYS.into())
    }

    fn read_clock(&mut self) -> Result<u64, Error> {
        Err(Errno::ENOSYS.into())
    }
}

/// Polls `future` once. This backend never suspends a handler except at a
/// tail injection, so one poll either finishes it or reaches that point.
pub fn poll_once<F: Future>(future: F) -> Option<F::Output> {
    let mut future = pin!(future);
    match future
        .as_mut()
        .poll(&mut Context::from_waker(Waker::noop()))
    {
        Poll::Ready(output) => Some(output),
        Poll::Pending => None,
    }
}

/// Runs `tool`'s syscall handler for one guest syscall and returns the value
/// the guest's syscall returns.
pub fn handle_syscall<T: Tool>(
    tool: &T,
    guest: &mut NarfGuest<'_, T>,
    sysno: Sysno,
    args: SyscallArgs,
) -> Result<i64, Error> {
    let syscall = Syscall::from_raw(sysno, args);
    if let Some(result) = poll_once(tool.handle_syscall_event(guest, syscall)) {
        return result;
    }
    match guest.tail_result.take() {
        Some(result) => result.map_err(Error::from),
        None => panic!("handler for {sysno} suspended without a tail injection"),
    }
}

#[cfg(test)]
mod tests {
    use super::counter1_tool::CounterGlobal;
    use super::counter1_tool::CounterLocal;
    use super::*;

    fn kernel(sysno: Sysno, args: SyscallArgs) -> Result<i64, Errno> {
        match sysno {
            Sysno::getpid => Ok(42),
            Sysno::write => Ok(args.arg2 as i64),
            _ => Err(Errno::ENOSYS),
        }
    }

    /// counter1 counts every syscall through its global RPC, and each
    /// syscall's result is the kernel's, delivered by `tail_inject`.
    #[test]
    fn counter1_counts_and_tail_injects() {
        let config = ();
        let global = poll_once(CounterGlobal::init_global_state(&config)).unwrap();
        let tool = <CounterLocal as Tool>::new(Pid::from_raw(7), &config);
        let mut guest = NarfGuest::<CounterLocal>::new(Pid::from_raw(7), &global, &config, kernel);

        let write = SyscallArgs::new(1, 0x1000, 5, 0, 0, 0);
        let none = SyscallArgs::new(0, 0, 0, 0, 0, 0);
        assert_eq!(
            handle_syscall(&tool, &mut guest, Sysno::getpid, none).ok(),
            Some(42)
        );
        assert_eq!(
            handle_syscall(&tool, &mut guest, Sysno::write, write).ok(),
            Some(5)
        );
        let err = handle_syscall(&tool, &mut guest, Sysno::uname, none).unwrap_err();
        assert_eq!(error_errno(err), Some(Errno::ENOSYS));
        assert_eq!(global.total(), 3);
    }
}
