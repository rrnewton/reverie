/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! `no_std` execution core that hosts an unmodified [`reverie::Tool`] inside
//! the Narf kernel.
//!
//! The Narf kernel calls its syscall interceptor on the trapping task's own
//! kernel stack, in the one address space it shares with every other task and
//! with the Tool. This crate turns that call into a Reverie callback without
//! any IPC, ptrace emulation, signal or binary rewriting:
//!
//! * the kernel implements [`KernelServices`], a narrow view of the current
//!   task and of the kernel-owned native transition for this one syscall;
//! * [`NarfToolHost`] owns the run's [`reverie::GlobalTool`] singleton, the
//!   Tool configuration, one Tool value per process and one
//!   `ThreadState` per thread;
//! * [`NarfToolHost::handle_syscall`] builds a [`NarfGuest`], which implements
//!   [`reverie::Guest`] on top of [`KernelServices`], and polls the Tool's
//!   `handle_syscall_event` future with a waker that does nothing. A future
//!   pending in a non-tail `inject` whose syscall parked the task is kept
//!   and polled again, with the syscall's value, when the kernel
//!   re-executes it. Any other future that is still pending without having
//!   made a terminal transition is waiting for another task: the kernel
//!   lets other tasks run ([`KernelServices::wait_for_repoll`]) and the
//!   core polls it again, within the same interceptor entry, until it
//!   finishes or the task is killed. Where the kernel cannot wait, such a
//!   future fails closed with [`NarfFatal::ToolSuspended`] and is never
//!   polled again, and so does one pending in thread start, post-exec, an
//!   exit hook or an interrupted inject, which are polled once;
//! * global RPC is a direct call of [`reverie::GlobalTool::receive_rpc`] on the
//!   singleton;
//! * [`NarfToolHost::task_exited`] runs `on_exit_thread` exactly once per
//!   thread and `on_exit_process` exactly once per process.
//!
//! The crate builds with `core` and `alloc` only, against Reverie with its
//! `std` feature off, and also against the host's `std` build of Reverie, so
//! the same code is exercised by host tests and linked into the kernel.

#![no_std]
#![deny(missing_docs)]
#![deny(rustdoc::broken_intra_doc_links)]

extern crate alloc;

mod guest;
mod host;
mod services;
mod stack;

pub use guest::NarfGuest;
pub use host::Disposition;
pub use host::LifecycleOutcome;
pub use host::NarfFatal;
pub use host::NarfToolHost;
pub use host::TaskExit;
pub use host::TaskLock;
pub use host::TaskTable;
pub use services::CreatedTask;
pub use services::CreatedTaskKind;
pub use services::KernelServices;
pub use services::RepollWait;
pub use stack::NarfStack;
pub use stack::NarfStackGuard;

/// Largest errno value reserved by Linux's raw syscall return convention.
pub const LINUX_MAX_ERRNO: i32 = 4095;

/// Bits of a Narf syscall wire number that carry the Linux syscall number.
///
/// The top byte is Narf's ABI version. Reverie's typed syscalls know only the
/// architecture number, so the core compares requests under this mask and
/// re-issues the intercepted original with its exact wire number.
pub const NARF_SYSCALL_NUMBER_MASK: u32 = 0x00ff_ffff;

/// Six raw Linux syscall arguments in architecture register order.
pub type RawSyscallArgs = [u64; 6];

/// One explicit native syscall request.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct NarfSyscallRequest {
    /// Exact unsigned syscall wire number, including Narf's version byte.
    pub number: u32,
    /// Six raw register arguments.
    pub args: RawSyscallArgs,
}

impl NarfSyscallRequest {
    /// The Linux syscall number with Narf's version byte removed.
    pub const fn linux_number(&self) -> u32 {
        self.number & NARF_SYSCALL_NUMBER_MASK
    }

    /// Whether `other` names the same Linux syscall with the same arguments,
    /// ignoring the version byte.
    pub const fn same_call(&self, other: &Self) -> bool {
        let mut i = 0;
        while i < 6 {
            if self.args[i] != other.args[i] {
                return false;
            }
            i += 1;
        }
        self.linux_number() == other.linux_number()
    }
}

/// One entry into the kernel's syscall interceptor.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SyscallEntry {
    /// The intercepted request exactly as the guest issued it.
    pub request: NarfSyscallRequest,
    /// True when the kernel is re-executing a syscall it previously parked
    /// (by rewinding the user instruction pointer), rather than delivering a
    /// new guest syscall. The Tool already observed the syscall at its first
    /// entry, so the core re-issues the parked transition without calling the
    /// Tool again.
    pub park_reexecution: bool,
}

impl SyscallEntry {
    /// A new guest syscall.
    pub const fn new(request: NarfSyscallRequest) -> Self {
        Self {
            request,
            park_reexecution: false,
        }
    }

    /// A re-execution of a previously parked syscall.
    pub const fn reexecution(request: NarfSyscallRequest) -> Self {
        Self {
            request,
            park_reexecution: true,
        }
    }
}

/// Outcome reported by Narf's kernel-owned native transition.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NarfSyscallOutcome {
    /// The handler returned the Linux-ABI value the guest observes after
    /// Narf's architecture return path has folded its native status.
    Returned(i64),
    /// The handler parked, execed, exited, or redirected the task.
    ContextManaged,
}

/// Why the intercepted original syscall can no longer execute.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OriginalSyscallError {
    /// The original syscall already executed once.
    AlreadyExecuted,
    /// Another transition parked, exited, execed, or redirected the live task.
    ContextManaged,
}

#[cfg(test)]
extern crate std;

#[cfg(test)]
mod tests {
    use alloc::boxed::Box;
    use core::sync::atomic::AtomicU64;
    use core::sync::atomic::Ordering;
    use std::sync::Mutex;

    use async_trait::async_trait;
    use reverie::Auxv;
    use reverie::Error;
    use reverie::GlobalTool;
    use reverie::Guest;
    use reverie::Pid;
    use reverie::Tool;
    use reverie::syscalls::Errno;
    use reverie::syscalls::IoSlice;
    use reverie::syscalls::IoSliceMut;
    use reverie::syscalls::MemoryAccess;
    use reverie::syscalls::Syscall;
    use reverie::syscalls::SyscallInfo;
    use reverie::syscalls::libc;

    use super::*;

    struct StdLock<V>(Mutex<V>);

    impl<V: Send> TaskLock<V> for StdLock<V> {
        fn new(value: V) -> Self {
            Self(Mutex::new(value))
        }

        fn with<R>(&self, f: impl FnOnce(&mut V) -> R) -> R {
            f(&mut self.0.lock().unwrap())
        }
    }

    #[derive(Default)]
    struct Global {
        total: AtomicU64,
    }

    #[async_trait]
    impl GlobalTool for Global {
        type Request = u64;
        type Response = ();
        type Config = ();

        async fn receive_rpc(&self, _from: Pid, message: u64) {
            self.total.fetch_add(message, Ordering::Relaxed);
        }
    }

    #[derive(Default)]
    struct Probe;

    #[async_trait]
    impl Tool for Probe {
        type GlobalState = Global;
        type ThreadState = u64;

        async fn handle_syscall_event<G: Guest<Self>>(
            &self,
            guest: &mut G,
            syscall: Syscall,
        ) -> Result<i64, Error> {
            *guest.thread_state_mut() += 1;
            guest.send_rpc(5).await;
            Ok(guest.inject(syscall).await? + 5)
        }
    }

    /// Fails every syscall with the errno in its first argument.
    #[derive(Default)]
    struct ErrnoTool;

    #[async_trait]
    impl Tool for ErrnoTool {
        type GlobalState = Global;
        type ThreadState = ();

        async fn handle_syscall_event<G: Guest<Self>>(
            &self,
            _guest: &mut G,
            syscall: Syscall,
        ) -> Result<i64, Error> {
            let (_, args) = syscall.into_parts();
            Err(Errno::new(args.arg0 as i32).into())
        }
    }

    struct NoMemory;

    impl MemoryAccess for NoMemory {
        fn read_vectored(&self, _: &[IoSlice], _: &mut [IoSliceMut]) -> Result<usize, Errno> {
            Err(Errno::EFAULT)
        }

        fn write_vectored(&mut self, _: &[IoSlice], _: &mut [IoSliceMut]) -> Result<usize, Errno> {
            Err(Errno::EFAULT)
        }
    }

    const TID: i32 = 7;

    struct Kernel {
        calls: u64,
    }

    impl KernelServices for Kernel {
        type Memory = NoMemory;

        fn tid(&self) -> Pid {
            Pid::from_raw(TID)
        }

        fn pid(&self) -> Pid {
            Pid::from_raw(TID)
        }

        fn ppid(&self) -> Option<Pid> {
            None
        }

        fn auxv(&self) -> Auxv {
            Auxv::from_entries([])
        }

        fn memory(&self) -> NoMemory {
            NoMemory
        }

        fn regs(&self) -> libc::user_regs_struct {
            // SAFETY: user_regs_struct is plain integers; all-zero is valid.
            unsafe { core::mem::zeroed() }
        }

        fn execute_original(&mut self) -> Result<NarfSyscallOutcome, OriginalSyscallError> {
            if self.calls != 0 {
                return Err(OriginalSyscallError::AlreadyExecuted);
            }
            self.calls += 1;
            Ok(NarfSyscallOutcome::Returned(37))
        }

        fn execute_injected(&mut self, _request: NarfSyscallRequest) -> NarfSyscallOutcome {
            self.calls += 1;
            NarfSyscallOutcome::Returned(99)
        }

        fn take_created_task(&mut self) -> Option<CreatedTask> {
            None
        }

        fn daemonize(&mut self) -> Result<(), Errno> {
            Ok(())
        }
    }

    fn host<T: Tool<GlobalState = Global> + 'static>() -> NarfToolHost<T, StdLock<TaskTable<T>>> {
        let host = NarfToolHost::new(()).expect("host");
        host.register_root(Pid::from_raw(TID), Pid::from_raw(TID))
            .expect("root");
        host
    }

    fn getpid(arg0: u64) -> SyscallEntry {
        SyscallEntry::new(NarfSyscallRequest {
            number: 39,
            args: [arg0, 0, 0, 0, 0, 0],
        })
    }

    #[test]
    fn errno_range_is_checked_and_maps_to_negative_linux_results() {
        let host = host::<ErrnoTool>();
        let mut kernel = Kernel { calls: 0 };
        let mut errno = |raw: i32| host.handle_syscall(&mut kernel, getpid(raw as u64));

        assert!(matches!(errno(-1), Err(NarfFatal::InvalidErrno(-1))));
        assert!(matches!(errno(0), Err(NarfFatal::InvalidErrno(0))));
        assert!(matches!(
            errno(LINUX_MAX_ERRNO + 1),
            Err(NarfFatal::InvalidErrno(4096))
        ));

        assert_eq!(errno(1).ok(), Some(Disposition::Complete(-1)));
        assert_eq!(
            errno(LINUX_MAX_ERRNO).ok(),
            Some(Disposition::Complete(-i64::from(LINUX_MAX_ERRNO)))
        );
    }

    #[test]
    fn drives_direct_global_thread_and_original_state() {
        let host = host::<Probe>();
        let mut kernel = Kernel { calls: 0 };

        let outcome = host.handle_syscall(&mut kernel, getpid(0));

        assert_eq!(outcome.ok(), Some(Disposition::Complete(42)));
        let thread = host.with_thread_state(Pid::from_raw(TID), |state| *state);
        assert_eq!(thread, Some(1));
        assert_eq!(host.global().total.load(Ordering::Relaxed), 5);
        assert_eq!(kernel.calls, 1);
    }
}
