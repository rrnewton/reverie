/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! A minimal [`reverie::Guest`] implementation over the KVM prototype.
//!
//! The M1 audit found `reverie-kvm` implemented *none* of the `Guest` trait.
//! This module adds the smallest viable [`KvmGuest`] so a real (async)
//! `reverie::Tool` — e.g. a syscall counter — can be driven over the existing
//! single-vCPU `vmcall` syscall-interception path (see [`crate::KvmBackend`]).
//!
//! # Scope and honesty
//!
//! This is scaffolding on the *bare* KVM prototype, not a Linux backend:
//!
//! - The prototype runs a **real-mode** guest, so a guest address is used
//!   directly as a guest-physical address ([`KvmMemory`] performs identity
//!   translation). A real backend needs guest page-table (VA→GPA) translation.
//! - Registers passed to the tool are **synthesised from the intercepted
//!   syscall frame** (number + six arguments), not read from the vCPU. The vCPU
//!   register ioctl is exposed as [`KvmBackend::vcpu_regs`] as the building
//!   block, but wiring it into `Guest::regs` needs a run-loop refactor because
//!   the KVM exit handle borrows the vCPU (documented in `run_tool`).
//! - [`Guest::inject`] executes the syscall **on the host** (translating in-range
//!   guest-physical pointer arguments to host addresses). This host-execution
//!   model is only valid for the marshalled demo syscalls; a real Linux
//!   personality (a guest kernel or a gVisor-style Sentry, per
//!   `ai_docs/kvm_backend_design.md`) is required for general programs.
//! - `stack`, `tail_inject`, and the timers are stubbed; `pid`/`tid` are
//!   synthetic (single vCPU, no process model yet).

use std::future::Future;
use std::io;
use std::pin::pin;
use std::task::Context;
use std::task::Poll;
use std::task::Waker;

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
use reverie::syscalls::SyscallInfo;
use reverie_memory::MemoryAccess;

use crate::GuestMemory;
use crate::SyscallRequest;

/// [`MemoryAccess`] over a KVM guest-physical memory region.
///
/// A lightweight, `Copy` handle holding the host base of the shared mapping plus
/// the guest-physical range. In the real-mode prototype guest addresses are
/// guest-physical, so translation is `host_base + (addr - guest_base)`.
#[derive(Clone, Copy, Debug)]
pub struct KvmMemory {
    host_base: usize,
    guest_base: u64,
    size: usize,
}

impl KvmMemory {
    /// Creates a memory handle for `memory`. The returned handle is only valid
    /// while the underlying mapping is live.
    pub(crate) fn new(memory: &GuestMemory) -> Self {
        Self {
            host_base: memory.host_address() as usize,
            guest_base: memory.guest_base(),
            size: memory.len(),
        }
    }

    /// Translates a guest(-physical) address + length to a host pointer,
    /// bounds-checked against the mapping.
    fn translate(&self, addr: u64, len: usize) -> std::result::Result<*mut u8, Errno> {
        let offset = addr.checked_sub(self.guest_base).ok_or(Errno::EFAULT)?;
        let end = offset.checked_add(len as u64).ok_or(Errno::EFAULT)?;
        if end > self.size as u64 {
            return Err(Errno::EFAULT);
        }
        Ok((self.host_base + offset as usize) as *mut u8)
    }

    /// Whether `addr` falls inside the guest-physical mapping (used to decide
    /// which injected-syscall arguments are pointers to translate).
    fn contains(&self, addr: u64) -> bool {
        addr >= self.guest_base && addr < self.guest_base + self.size as u64
    }
}

impl MemoryAccess for KvmMemory {
    fn read_vectored(
        &self,
        read_from: &[io::IoSlice],
        write_to: &mut [io::IoSliceMut],
    ) -> std::result::Result<usize, Errno> {
        // `read_from` are guest addresses; `write_to` are host destination
        // buffers. Copy pairwise (the trait's default `read`/`read_exact` use a
        // single slice each).
        let mut total = 0;
        for (src, dst) in read_from.iter().zip(write_to.iter_mut()) {
            let len = src.len().min(dst.len());
            if len == 0 {
                continue;
            }
            let host = self.translate(src.as_ptr() as u64, len)?;
            // SAFETY: `translate` proved [host, host+len) lies in the live
            // mapping; `dst` is a distinct host buffer of at least `len` bytes.
            unsafe {
                std::ptr::copy_nonoverlapping(host, dst.as_mut_ptr(), len);
            }
            total += len;
        }
        Ok(total)
    }

    fn write_vectored(
        &mut self,
        read_from: &[io::IoSlice],
        write_to: &mut [io::IoSliceMut],
    ) -> std::result::Result<usize, Errno> {
        // `read_from` are host source buffers; `write_to` are guest addresses.
        let mut total = 0;
        for (src, dst) in read_from.iter().zip(write_to.iter_mut()) {
            let len = src.len().min(dst.len());
            if len == 0 {
                continue;
            }
            let host = self.translate(dst.as_ptr() as u64, len)?;
            // SAFETY: `translate` proved [host, host+len) lies in the live
            // mapping; `src` is a distinct host buffer of at least `len` bytes.
            unsafe {
                std::ptr::copy_nonoverlapping(src.as_ptr(), host, len);
            }
            total += len;
        }
        Ok(total)
    }
}

/// A guest thread handle presented to a Reverie tool for one intercepted
/// syscall. See the module docs for scope.
pub struct KvmGuest<'a, T>
where
    T: Tool,
{
    memory: KvmMemory,
    regs: libc::user_regs_struct,
    tid: Pid,
    pid: Pid,
    ppid: Option<Pid>,
    thread_state: &'a mut T::ThreadState,
    global_state: &'a T::GlobalState,
    config: &'a <T::GlobalState as GlobalTool>::Config,
}

impl<'a, T> KvmGuest<'a, T>
where
    T: Tool,
{
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        memory: KvmMemory,
        regs: libc::user_regs_struct,
        tid: Pid,
        pid: Pid,
        ppid: Option<Pid>,
        thread_state: &'a mut T::ThreadState,
        global_state: &'a T::GlobalState,
        config: &'a <T::GlobalState as GlobalTool>::Config,
    ) -> Self {
        Self {
            memory,
            regs,
            tid,
            pid,
            ppid,
            thread_state,
            global_state,
            config,
        }
    }
}

#[reverie::tool]
impl<T> GlobalRPC<T::GlobalState> for KvmGuest<'_, T>
where
    T: Tool,
{
    async fn send_rpc(
        &self,
        message: <T::GlobalState as GlobalTool>::Request,
    ) -> <T::GlobalState as GlobalTool>::Response {
        // Single address space today: the global state lives in this process.
        self.global_state.receive_rpc(self.tid, message).await
    }

    fn config(&self) -> &<T::GlobalState as GlobalTool>::Config {
        self.config
    }
}

#[reverie::tool]
impl<T> Guest<T> for KvmGuest<'_, T>
where
    T: Tool,
{
    type Memory = KvmMemory;
    type Stack = UnsupportedStack;

    fn tid(&self) -> Pid {
        self.tid
    }

    fn pid(&self) -> Pid {
        self.pid
    }

    fn ppid(&self) -> Option<Pid> {
        self.ppid
    }

    fn memory(&self) -> Self::Memory {
        self.memory
    }

    fn thread_state_mut(&mut self) -> &mut T::ThreadState {
        self.thread_state
    }

    fn thread_state(&self) -> &T::ThreadState {
        self.thread_state
    }

    async fn regs(&mut self) -> libc::user_regs_struct {
        // Synthesised from the intercepted syscall frame (see module docs).
        self.regs
    }

    async fn stack(&mut self) -> Self::Stack {
        UnsupportedStack
    }

    async fn daemonize(&mut self) {}

    async fn inject<S: SyscallInfo>(&mut self, syscall: S) -> std::result::Result<i64, Errno> {
        let (number, args) = syscall.into_parts();
        // Translate any argument that points inside guest memory to its host
        // address so the host can execute the syscall on the guest's behalf.
        let map = |value: usize| -> usize {
            let value = value as u64;
            if self.memory.contains(value) {
                match self.memory.translate(value, 1) {
                    Ok(ptr) => ptr as usize,
                    Err(_) => value as usize,
                }
            } else {
                value as usize
            }
        };
        let result = unsafe {
            libc::syscall(
                number.id() as libc::c_long,
                map(args.arg0),
                map(args.arg1),
                map(args.arg2),
                map(args.arg3),
                map(args.arg4),
                map(args.arg5),
            )
        };
        Errno::from_ret(result as usize).map(|value| value as i64)
    }

    async fn tail_inject<S: SyscallInfo>(&mut self, _syscall: S) -> Never {
        panic!("tail injection is not implemented by the KVM prototype")
    }

    fn set_timer(&mut self, _sched: TimerSchedule) -> std::result::Result<(), Error> {
        Err(Errno::ENOSYS.into())
    }

    fn set_timer_precise(&mut self, _sched: TimerSchedule) -> std::result::Result<(), Error> {
        Err(Errno::ENOSYS.into())
    }

    fn read_clock(&mut self) -> std::result::Result<u64, Error> {
        // The bare prototype has no branch/instruction counter yet.
        Ok(0)
    }
}

/// Placeholder stack implementation (matches the DBI prototype). A real guest
/// stack requires a Linux ABI/address space.
pub struct UnsupportedStack;

/// Guard returned by [`UnsupportedStack`].
pub struct UnsupportedStackGuard;

impl Drop for UnsupportedStackGuard {
    fn drop(&mut self) {}
}

impl Stack for UnsupportedStack {
    type StackGuard = UnsupportedStackGuard;

    fn size(&self) -> usize {
        0
    }

    fn capacity(&self) -> usize {
        0
    }

    fn push<'stack, V>(&mut self, _value: V) -> Addr<'stack, V> {
        panic!("guest stack allocation is not implemented by the KVM prototype")
    }

    fn reserve<'stack, V>(&mut self) -> AddrMut<'stack, V> {
        panic!("guest stack allocation is not implemented by the KVM prototype")
    }

    fn commit(self) -> std::result::Result<Self::StackGuard, Errno> {
        Err(Errno::ENOSYS)
    }
}

/// Synthesise an x86-64 `user_regs_struct` from an intercepted syscall frame.
pub(crate) fn regs_from_frame(request: &SyscallRequest) -> libc::user_regs_struct {
    // SAFETY: `user_regs_struct` is plain-old-data; zero is a valid bit pattern.
    let mut regs: libc::user_regs_struct = unsafe { std::mem::zeroed() };
    let args = request.args();
    regs.orig_rax = request.number();
    regs.rax = request.number();
    regs.rdi = args[0];
    regs.rsi = args[1];
    regs.rdx = args[2];
    regs.r10 = args[3];
    regs.r8 = args[4];
    regs.r9 = args[5];
    regs
}

/// A minimal, dependency-free, single-threaded executor driving a tool handler
/// (which may suspend) to completion on the current thread.
pub(crate) fn block_on<F: Future>(future: F) -> F::Output {
    use std::sync::Arc;
    use std::task::Wake;
    use std::thread;

    struct ThreadWaker(thread::Thread);

    impl Wake for ThreadWaker {
        fn wake(self: Arc<Self>) {
            self.0.unpark();
        }
        fn wake_by_ref(self: &Arc<Self>) {
            self.0.unpark();
        }
    }

    let mut future = pin!(future);
    let waker = Waker::from(Arc::new(ThreadWaker(thread::current())));
    let mut context = Context::from_waker(&waker);
    loop {
        match future.as_mut().poll(&mut context) {
            Poll::Ready(value) => return value,
            Poll::Pending => thread::park(),
        }
    }
}
