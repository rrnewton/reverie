/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! `no_std` kernel-facing execution core for `reverie-narf`.
//!
//! This crate contains no Linux process API, socket, serialization, or IPC
//! dependency. Narf implements [`KernelTransition`] with its first-class kernel
//! hook. A kernel-compatible Tool implements [`Tool`], and [`drive_syscall`]
//! polls that Tool while lending direct references to its global and per-thread
//! state. The standard `reverie-narf` crate reuses the exact request and outcome
//! types here while adapting the full Linux/`std` Reverie traits.

#![no_std]
#![deny(missing_docs)]
#![deny(rustdoc::broken_intra_doc_links)]

use core::future::Future;
use core::future::poll_fn;
use core::pin::Pin;
use core::sync::atomic::AtomicI64;
use core::sync::atomic::AtomicU8;
use core::sync::atomic::Ordering;
use core::task::Context;
use core::task::Poll;

const TAIL_NONE: u8 = 0;
const TAIL_RETURNED: u8 = 1;
const TAIL_CONTEXT_MANAGED: u8 = 2;

/// Six raw Linux syscall arguments in architecture register order.
pub type RawSyscallArgs = [u64; 6];

/// One explicit native syscall request.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct NarfSyscallRequest {
    /// Exact unsigned syscall wire number for the current architecture.
    pub number: u32,
    /// Six raw register arguments.
    pub args: RawSyscallArgs,
}

/// Immutable metadata for one intercepted guest syscall.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SyscallEvent {
    /// Original syscall request.
    pub request: NarfSyscallRequest,
    /// Narf task identifier.
    pub task_id: u64,
    /// User instruction pointer immediately after the syscall instruction.
    pub instruction_pointer: u64,
    /// User stack pointer at entry.
    pub stack_pointer: u64,
}

/// Outcome reported by Narf's kernel-owned native transition.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NarfSyscallOutcome {
    /// The handler returned a raw Linux result (`>= 0` or `-errno`).
    Returned(i64),
    /// The handler parked, execed, exited, or redirected the task.
    ContextManaged,
}

/// The original transition had already been consumed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct OriginalAlreadyExecuted;

/// Narf's kernel-owned native transition for the current callback.
pub trait KernelTransition {
    /// Execute the intercepted original syscall at most once.
    fn execute_original(&mut self) -> Result<NarfSyscallOutcome, OriginalAlreadyExecuted>;

    /// Execute one explicit request, bypassing interception.
    fn execute_injected(&mut self, request: NarfSyscallRequest) -> NarfSyscallOutcome;
}

/// A Tool failure which cannot be represented as an ordinary raw return.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ToolError {
    /// Linux errno number, stored as a positive value.
    Errno(i32),
    /// Backend/tool-specific fatal class. The integration must stop the run.
    Fatal(u32),
}

/// Terminal disposition after driving one Tool syscall callback.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DrivenSyscall {
    /// Resume the guest with this raw Linux return value.
    Complete(i64),
    /// Narf owns continuation; no return may be fabricated.
    ContextManaged,
    /// Stop the run with this fatal class.
    Fatal(u32),
}

/// Kernel-compatible Tool policy hosted by this core.
///
/// The full `reverie::Tool` adapter remains in the standard `reverie-narf`
/// crate. This trait is the allocation-free ABI that can compile into Narf
/// today; the ongoing API split will make full Reverie Tools target this same
/// core without duplicating policy.
pub trait Tool: Sync {
    /// Process-tree singleton shared directly by every callback.
    type GlobalState: Sync;
    /// State owned by one guest thread.
    type ThreadState;

    /// Handle one subscribed syscall.
    fn handle_syscall<'a, K>(
        &'a self,
        guest: &'a mut Guest<'_, Self, K>,
        event: SyscallEvent,
    ) -> impl Future<Output = Result<i64, ToolError>> + 'a
    where
        Self: Sized,
        K: KernelTransition + 'a;
}

#[derive(Default)]
struct TailCell {
    kind: AtomicU8,
    value: AtomicI64,
}

impl TailCell {
    fn publish(&self, outcome: NarfSyscallOutcome) {
        match outcome {
            NarfSyscallOutcome::Returned(value) => {
                self.value.store(value, Ordering::Relaxed);
                self.kind.store(TAIL_RETURNED, Ordering::Release);
            }
            NarfSyscallOutcome::ContextManaged => {
                self.kind.store(TAIL_CONTEXT_MANAGED, Ordering::Release);
            }
        }
    }

    fn take(&self) -> Option<DrivenSyscall> {
        match self.kind.swap(TAIL_NONE, Ordering::AcqRel) {
            TAIL_RETURNED => Some(DrivenSyscall::Complete(self.value.load(Ordering::Relaxed))),
            TAIL_CONTEXT_MANAGED => Some(DrivenSyscall::ContextManaged),
            _ => None,
        }
    }
}

/// Direct state and kernel operations available to one Tool callback.
pub struct Guest<'a, T, K>
where
    T: Tool + ?Sized,
    K: KernelTransition,
{
    kernel: &'a mut K,
    global: &'a T::GlobalState,
    thread_state: &'a mut T::ThreadState,
    original: NarfSyscallRequest,
    tail: &'a TailCell,
}

impl<T, K> Guest<'_, T, K>
where
    T: Tool + ?Sized,
    K: KernelTransition,
{
    /// Borrow the process-tree singleton directly.
    pub fn global(&self) -> &T::GlobalState {
        self.global
    }

    /// Borrow this guest thread's Tool state.
    pub fn thread_state(&self) -> &T::ThreadState {
        self.thread_state
    }

    /// Mutably borrow this guest thread's Tool state.
    pub fn thread_state_mut(&mut self) -> &mut T::ThreadState {
        self.thread_state
    }

    fn execute(&mut self, request: NarfSyscallRequest) -> NarfSyscallOutcome {
        if request == self.original
            && let Ok(outcome) = self.kernel.execute_original()
        {
            return outcome;
        }
        self.kernel.execute_injected(request)
    }

    /// Execute a syscall and return its raw Linux result.
    ///
    /// A context-managed transition deliberately never resolves this future;
    /// the outer driver observes the terminal cell in the same poll and returns
    /// [`DrivenSyscall::ContextManaged`].
    pub async fn inject(&mut self, request: NarfSyscallRequest) -> i64 {
        match self.execute(request) {
            NarfSyscallOutcome::Returned(value) => value,
            outcome @ NarfSyscallOutcome::ContextManaged => {
                self.tail.publish(outcome);
                core::future::pending().await
            }
        }
    }

    /// Execute a syscall as the terminal action of this callback.
    pub async fn tail_inject(&mut self, request: NarfSyscallRequest) -> Never {
        let outcome = self.execute(request);
        self.tail.publish(outcome);
        core::future::pending().await
    }
}

/// Uninhabited return type for terminal Tool actions.
pub enum Never {}

/// Drive one syscall through a kernel-compatible Tool.
pub async fn drive_syscall<T, K>(
    tool: &T,
    global: &T::GlobalState,
    thread_state: &mut T::ThreadState,
    kernel: &mut K,
    event: SyscallEvent,
) -> DrivenSyscall
where
    T: Tool,
    K: KernelTransition,
{
    let tail = TailCell::default();
    let mut guest = Guest::<T, K> {
        kernel,
        global,
        thread_state,
        original: event.request,
        tail: &tail,
    };
    let mut future = core::pin::pin!(tool.handle_syscall(&mut guest, event));
    poll_fn(|context| poll_tool_future(future.as_mut(), &tail, context)).await
}

fn poll_tool_future<F>(
    mut future: Pin<&mut F>,
    tail: &TailCell,
    context: &mut Context<'_>,
) -> Poll<DrivenSyscall>
where
    F: Future<Output = Result<i64, ToolError>> + ?Sized,
{
    match future.as_mut().poll(context) {
        Poll::Ready(Ok(value)) => Poll::Ready(DrivenSyscall::Complete(value)),
        Poll::Ready(Err(ToolError::Errno(errno))) => {
            Poll::Ready(DrivenSyscall::Complete(-(errno as i64)))
        }
        Poll::Ready(Err(ToolError::Fatal(class))) => Poll::Ready(DrivenSyscall::Fatal(class)),
        Poll::Pending => match tail.take() {
            Some(outcome) => Poll::Ready(outcome),
            None => Poll::Pending,
        },
    }
}

#[cfg(test)]
extern crate std;

#[cfg(test)]
mod tests {
    use core::sync::atomic::AtomicU64;
    use core::task::RawWaker;
    use core::task::RawWakerVTable;
    use core::task::Waker;

    use super::*;

    #[derive(Default)]
    struct Global {
        total: AtomicU64,
    }

    struct Probe;
    impl Tool for Probe {
        type GlobalState = Global;
        type ThreadState = u64;

        async fn handle_syscall<'a, K>(
            &'a self,
            guest: &'a mut Guest<'_, Self, K>,
            event: SyscallEvent,
        ) -> Result<i64, ToolError>
        where
            K: KernelTransition + 'a,
        {
            *guest.thread_state_mut() += 1;
            guest.global().total.fetch_add(5, Ordering::Relaxed);
            Ok(guest.inject(event.request).await + 5)
        }
    }

    struct Kernel {
        calls: u64,
    }

    impl KernelTransition for Kernel {
        fn execute_original(&mut self) -> Result<NarfSyscallOutcome, OriginalAlreadyExecuted> {
            if self.calls != 0 {
                return Err(OriginalAlreadyExecuted);
            }
            self.calls += 1;
            Ok(NarfSyscallOutcome::Returned(37))
        }

        fn execute_injected(&mut self, _request: NarfSyscallRequest) -> NarfSyscallOutcome {
            self.calls += 1;
            NarfSyscallOutcome::Returned(99)
        }
    }

    fn block_on_ready<F: Future>(future: F) -> F::Output {
        unsafe fn clone(_: *const ()) -> RawWaker {
            raw_waker()
        }
        unsafe fn no_op(_: *const ()) {}
        fn raw_waker() -> RawWaker {
            RawWaker::new(
                core::ptr::null(),
                &RawWakerVTable::new(clone, no_op, no_op, no_op),
            )
        }

        let waker = unsafe { Waker::from_raw(raw_waker()) };
        let mut context = Context::from_waker(&waker);
        let mut future = core::pin::pin!(future);
        loop {
            if let Poll::Ready(value) = future.as_mut().poll(&mut context) {
                return value;
            }
        }
    }

    #[test]
    fn drives_direct_global_thread_and_original_state() {
        let global = Global::default();
        let mut thread = 0;
        let mut kernel = Kernel { calls: 0 };
        let event = SyscallEvent {
            request: NarfSyscallRequest {
                number: 39,
                args: [0; 6],
            },
            task_id: 7,
            instruction_pointer: 0x1000,
            stack_pointer: 0x2000,
        };

        let outcome = block_on_ready(drive_syscall(
            &Probe,
            &global,
            &mut thread,
            &mut kernel,
            event,
        ));

        assert_eq!(outcome, DrivenSyscall::Complete(42));
        assert_eq!(thread, 1);
        assert_eq!(global.total.load(Ordering::Relaxed), 5);
        assert_eq!(kernel.calls, 1);
    }
}
