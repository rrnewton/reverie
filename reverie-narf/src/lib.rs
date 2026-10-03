/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! Host (`std`) face of the Narf Reverie backend.
//!
//! The execution core is [`reverie_narf_core`], which builds with `core` and
//! `alloc` only and is the code the Narf kernel links. This crate re-exports
//! it and adds what a host build needs to run the same core outside the
//! kernel: [`StdTaskLock`], a [`TaskLock`] over [`std::sync::Mutex`], and the
//! [`StdToolHost`] alias. There is no second Tool trait and no second driver;
//! host tests of this crate drive the same [`NarfToolHost`] the kernel uses.

#![deny(missing_docs)]
#![deny(rustdoc::broken_intra_doc_links)]

use std::sync::Mutex;

pub use reverie_narf_core::*;

/// A [`TaskLock`] over [`std::sync::Mutex`] for host builds.
///
/// The core never holds the lock while a Tool runs, so a poisoned lock can
/// only come from a panic inside the core's own table update; the lock is
/// recovered rather than propagating the panic to every later callback.
pub struct StdTaskLock<V>(Mutex<V>);

impl<V: Send> TaskLock<V> for StdTaskLock<V> {
    fn new(value: V) -> Self {
        Self(Mutex::new(value))
    }

    fn with<R>(&self, f: impl FnOnce(&mut V) -> R) -> R {
        let mut guard = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        f(&mut guard)
    }
}

/// A [`NarfToolHost`] whose task table is guarded by a [`StdTaskLock`].
pub type StdToolHost<T> = NarfToolHost<T, StdTaskLock<TaskTable<T>>>;

#[cfg(test)]
mod tests {
    use core::sync::atomic::AtomicU64;
    use core::sync::atomic::AtomicUsize;
    use core::sync::atomic::Ordering;

    use async_trait::async_trait;
    use reverie::Auxv;
    use reverie::Error;
    use reverie::GlobalTool;
    use reverie::Guest;
    use reverie::Pid;
    use reverie::Subscription;
    use reverie::Tool;
    use reverie::syscalls::Errno;
    use reverie::syscalls::Getppid;
    use reverie::syscalls::Syscall;
    use reverie_memory::LocalMemory;

    use super::*;

    #[derive(Default)]
    struct SharedGlobal {
        sum: AtomicU64,
    }

    #[async_trait]
    impl GlobalTool for SharedGlobal {
        type Request = u64;
        type Response = u64;
        type Config = ();

        async fn receive_rpc(&self, _from: reverie::Tid, message: u64) -> u64 {
            self.sum.fetch_add(message, Ordering::Relaxed) + message
        }
    }

    #[derive(Default)]
    struct ProbeTool;

    #[async_trait]
    impl Tool for ProbeTool {
        type GlobalState = SharedGlobal;
        type ThreadState = u64;

        fn subscriptions(_config: &()) -> Subscription {
            Subscription::all_syscalls()
        }

        async fn handle_syscall_event<G: Guest<Self>>(
            &self,
            guest: &mut G,
            syscall: Syscall,
        ) -> Result<i64, Error> {
            *guest.thread_state_mut() += 1;
            let shared = guest.send_rpc(5).await;
            Ok(guest.inject(syscall).await? + shared as i64)
        }
    }

    #[derive(Default)]
    struct InjectOtherTool;

    #[async_trait]
    impl Tool for InjectOtherTool {
        type GlobalState = SharedGlobal;
        type ThreadState = u64;

        fn subscriptions(_config: &()) -> Subscription {
            Subscription::all_syscalls()
        }

        async fn handle_syscall_event<G: Guest<Self>>(
            &self,
            guest: &mut G,
            _syscall: Syscall,
        ) -> Result<i64, Error> {
            guest
                .inject(Syscall::Getppid(Getppid::new()))
                .await
                .map_err(Error::from)
        }
    }

    const TID: i32 = 101;

    struct FakeKernel {
        original_calls: AtomicUsize,
        injected_calls: AtomicUsize,
        original_context_managed: bool,
    }

    impl FakeKernel {
        fn new() -> Self {
            Self {
                original_calls: AtomicUsize::new(0),
                injected_calls: AtomicUsize::new(0),
                original_context_managed: false,
            }
        }
    }

    impl KernelServices for FakeKernel {
        type Memory = LocalMemory;

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
            Auxv::from_entries(Vec::new())
        }

        fn memory(&self) -> Self::Memory {
            LocalMemory::new()
        }

        fn regs(&self) -> libc::user_regs_struct {
            // SAFETY: Linux's user_regs_struct accepts the all-zero bit pattern.
            unsafe { core::mem::zeroed() }
        }

        fn execute_original(&mut self) -> Result<NarfSyscallOutcome, OriginalSyscallError> {
            if self.original_context_managed {
                return Err(OriginalSyscallError::ContextManaged);
            }
            if self.original_calls.fetch_add(1, Ordering::Relaxed) == 0 {
                Ok(NarfSyscallOutcome::Returned(37))
            } else {
                Err(OriginalSyscallError::AlreadyExecuted)
            }
        }

        fn execute_injected(&mut self, _request: NarfSyscallRequest) -> NarfSyscallOutcome {
            self.injected_calls.fetch_add(1, Ordering::Relaxed);
            NarfSyscallOutcome::Returned(99)
        }

        fn take_created_task(&mut self) -> Option<CreatedTask> {
            None
        }

        fn daemonize(&mut self) -> Result<(), Errno> {
            Ok(())
        }
    }

    fn host<T: Tool<GlobalState = SharedGlobal>>() -> StdToolHost<T> {
        let host = StdToolHost::<T>::new(()).expect("host");
        host.register_root(Pid::from_raw(TID), Pid::from_raw(TID))
            .expect("root task");
        host
    }

    fn versioned_getpid() -> SyscallEntry {
        SyscallEntry::new(NarfSyscallRequest {
            number: (7 << 24) | libc::SYS_getpid as u32,
            args: [0; 6],
        })
    }

    #[test]
    fn arbitrary_tool_preserves_versioned_original_and_uses_direct_global_state() {
        let host = host::<ProbeTool>();
        let mut kernel = FakeKernel::new();

        let outcome = host.handle_syscall(&mut kernel, versioned_getpid());

        assert!(matches!(outcome, Ok(Disposition::Complete(42))));
        let thread_state = host
            .with_thread_state(Pid::from_raw(TID), |state| *state)
            .expect("thread state");
        assert_eq!(thread_state, 1);
        assert_eq!(host.global().sum.load(Ordering::Relaxed), 5);
        assert_eq!(kernel.original_calls.load(Ordering::Relaxed), 1);
        assert_eq!(kernel.injected_calls.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn distinct_typed_request_uses_injected_transition() {
        let host = host::<InjectOtherTool>();
        let mut kernel = FakeKernel::new();

        let outcome = host.handle_syscall(&mut kernel, versioned_getpid());

        assert!(matches!(outcome, Ok(Disposition::Complete(99))));
        assert_eq!(kernel.original_calls.load(Ordering::Relaxed), 0);
        assert_eq!(kernel.injected_calls.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn terminal_context_does_not_fall_through_to_injected_dispatch() {
        let host = host::<ProbeTool>();
        let mut kernel = FakeKernel::new();
        kernel.original_context_managed = true;

        let outcome = host.handle_syscall(&mut kernel, versioned_getpid());

        assert!(matches!(outcome, Ok(Disposition::ContextManaged)));
        assert_eq!(kernel.original_calls.load(Ordering::Relaxed), 0);
        assert_eq!(kernel.injected_calls.load(Ordering::Relaxed), 0);
    }
}
