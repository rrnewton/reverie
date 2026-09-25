/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! Reverie Tool hosting over Narf's first-class kernel interception hooks.
//!
//! This crate owns the mechanism-neutral half of the backend. A Narf adapter
//! implements [`NarfKernel`] with direct references to the current task and the
//! kernel-owned native syscall transition. [`drive_syscall`] then hosts an
//! arbitrary [`reverie::Tool`] in that same address space. Global RPC is a
//! direct method call on the run's singleton global state; this backend does not
//! introduce a socket, ring, pipe, or other IPC path merely to share Tool state.
//!
//! The crate currently builds against Reverie's Linux/`std` public API. The
//! kernel adapter and `no_std` API split required to link this driver into a
//! Narf image are tracked as an explicit implementation boundary rather than a
//! claim of current runtime support.

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

use async_trait::async_trait;
use reverie::Auxv;
use reverie::DetlogMemoryRegion;
use reverie::Error;
use reverie::GlobalRPC;
use reverie::GlobalTool;
use reverie::Guest;
use reverie::Never;
use reverie::Pid;
use reverie::Stack;
use reverie::TimerSchedule;
use reverie::Tool;
use reverie::syscalls::Errno;
use reverie::syscalls::Syscall;
use reverie::syscalls::SyscallInfo;
use reverie_memory::MemoryAccess;
pub use reverie_narf_core::NarfSyscallOutcome;
pub use reverie_narf_core::NarfSyscallRequest;
pub use reverie_narf_core::OriginalAlreadyExecuted;
pub use reverie_narf_core::RawSyscallArgs;

const TAIL_NONE: u8 = 0;
const TAIL_RETURNED: u8 = 1;
const TAIL_CONTEXT_MANAGED: u8 = 2;
const NARF_SYSCALL_NUMBER_MASK: u32 = 0x00ff_ffff;

/// Direct Narf services required by one stopped guest thread.
///
/// Implementations live in the Narf integration layer. All methods execute in
/// the current kernel/task address space; this trait is not an RPC transport.
pub trait NarfKernel: Send + Sync {
    /// Guest-memory accessor tied to the current address space.
    type Memory: MemoryAccess + Send;
    /// Guest-stack allocator tied to the current thread.
    type Stack: Stack + Send;

    /// Current thread ID.
    fn tid(&self) -> i32;
    /// Current process ID.
    fn pid(&self) -> i32;
    /// Parent process ID, absent for the root process.
    fn ppid(&self) -> Option<i32>;
    /// Backend-provided auxiliary-vector entries.
    fn auxv_entries(&self) -> Vec<(libc::c_ulong, libc::c_ulong)>;
    /// Guest-memory accessor.
    fn memory(&self) -> Self::Memory;
    /// Snapshot current guest registers.
    fn regs(&mut self) -> libc::user_regs_struct;
    /// Replace current guest registers.
    fn set_regs(&mut self, regs: libc::user_regs_struct) -> Result<(), Errno>;
    /// Allocate a guest-stack staging object.
    fn stack(&mut self) -> Self::Stack;
    /// Mark the current task as a daemon.
    fn daemonize(&mut self);
    /// Execute the intercepted original syscall at most once.
    fn execute_original(&mut self) -> Result<NarfSyscallOutcome, OriginalAlreadyExecuted>;
    /// Execute one explicit syscall request, bypassing interception.
    fn execute_injected(&mut self, request: NarfSyscallRequest) -> NarfSyscallOutcome;
    /// Program an approximate timer.
    fn set_timer(&mut self, schedule: TimerSchedule) -> Result<(), Error>;
    /// Program a precise timer.
    fn set_timer_precise(&mut self, schedule: TimerSchedule) -> Result<(), Error>;
    /// Read the backend's thread-local monotonic clock.
    fn read_clock(&mut self) -> Result<u64, Error>;
    /// Whether CPUID interception is active for this task.
    fn has_cpuid_interception(&self) -> bool {
        false
    }
    /// Guest ranges used for deterministic memory-map logging.
    fn detlog_memory_regions(&self) -> Option<Vec<DetlogMemoryRegion>> {
        None
    }
}

/// Terminal result of driving one Tool syscall callback.
#[derive(Debug)]
pub enum DrivenSyscall {
    /// Resume the guest with this raw Linux return value.
    Complete(i64),
    /// Narf's handler owns task continuation; no return may be fabricated.
    ContextManaged,
    /// The Tool failed with a non-errno error.
    Fatal(Error),
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

/// A direct in-address-space [`Guest`] implementation for one Narf callback.
pub struct NarfGuest<'a, T, K>
where
    T: Tool,
    K: NarfKernel,
{
    kernel: &'a mut K,
    global: &'a T::GlobalState,
    config: &'a <T::GlobalState as GlobalTool>::Config,
    thread_state: &'a mut T::ThreadState,
    original: NarfSyscallRequest,
    tail: &'a TailCell,
}

impl<T, K> NarfGuest<'_, T, K>
where
    T: Tool,
    K: NarfKernel,
{
    fn request<S: SyscallInfo>(syscall: S) -> NarfSyscallRequest {
        let (number, args) = syscall.into_parts();
        NarfSyscallRequest {
            number: number.id() as u32,
            args: [
                args.arg0 as u64,
                args.arg1 as u64,
                args.arg2 as u64,
                args.arg3 as u64,
                args.arg4 as u64,
                args.arg5 as u64,
            ],
        }
    }

    fn execute(&mut self, request: NarfSyscallRequest) -> NarfSyscallOutcome {
        // Reverie's typed Syscall carries the architecture number and
        // arguments, but not Narf's version byte. Treat an unchanged typed
        // forward as the intercepted original and let the kernel retain its
        // exact wire version. A genuinely different typed request remains a
        // version-zero injected syscall.
        let forwards_original = request.args == self.original.args
            && request.number == self.original.number & NARF_SYSCALL_NUMBER_MASK;
        if forwards_original && let Ok(outcome) = self.kernel.execute_original() {
            return outcome;
        }
        self.kernel.execute_injected(request)
    }
}

#[async_trait]
impl<T, K> GlobalRPC<T::GlobalState> for NarfGuest<'_, T, K>
where
    T: Tool,
    K: NarfKernel,
{
    async fn send_rpc(
        &self,
        message: <T::GlobalState as GlobalTool>::Request,
    ) -> <T::GlobalState as GlobalTool>::Response {
        self.global.receive_rpc(self.tid(), message).await
    }

    fn config(&self) -> &<T::GlobalState as GlobalTool>::Config {
        self.config
    }
}

#[async_trait]
impl<T, K> Guest<T> for NarfGuest<'_, T, K>
where
    T: Tool,
    K: NarfKernel,
{
    type Memory = K::Memory;
    type Stack = K::Stack;

    fn tid(&self) -> Pid {
        Pid::from_raw(self.kernel.tid())
    }

    fn pid(&self) -> Pid {
        Pid::from_raw(self.kernel.pid())
    }

    fn ppid(&self) -> Option<Pid> {
        self.kernel.ppid().map(Pid::from_raw)
    }

    fn auxv(&self) -> Auxv {
        Auxv::from_entries(self.kernel.auxv_entries())
    }

    fn memory(&self) -> Self::Memory {
        self.kernel.memory()
    }

    fn thread_state_mut(&mut self) -> &mut T::ThreadState {
        self.thread_state
    }

    fn thread_state(&self) -> &T::ThreadState {
        self.thread_state
    }

    async fn regs(&mut self) -> libc::user_regs_struct {
        self.kernel.regs()
    }

    async fn set_regs(&mut self, regs: libc::user_regs_struct) -> Result<(), Error> {
        self.kernel.set_regs(regs).map_err(Error::from)
    }

    async fn stack(&mut self) -> Self::Stack {
        self.kernel.stack()
    }

    async fn daemonize(&mut self) {
        self.kernel.daemonize();
    }

    async fn inject<S: SyscallInfo>(&mut self, syscall: S) -> Result<i64, Errno> {
        match self.execute(Self::request(syscall)) {
            NarfSyscallOutcome::Returned(value) => {
                Errno::from_ret(value as usize).map(|result| result as i64)
            }
            outcome @ NarfSyscallOutcome::ContextManaged => {
                self.tail.publish(outcome);
                core::future::pending().await
            }
        }
    }

    async fn tail_inject<S: SyscallInfo>(&mut self, syscall: S) -> Never {
        let outcome = self.execute(Self::request(syscall));
        self.tail.publish(outcome);
        core::future::pending().await
    }

    fn set_timer(&mut self, schedule: TimerSchedule) -> Result<(), Error> {
        self.kernel.set_timer(schedule)
    }

    fn set_timer_precise(&mut self, schedule: TimerSchedule) -> Result<(), Error> {
        self.kernel.set_timer_precise(schedule)
    }

    fn read_clock(&mut self) -> Result<u64, Error> {
        self.kernel.read_clock()
    }

    fn has_cpuid_interception(&self) -> bool {
        self.kernel.has_cpuid_interception()
    }

    fn detlog_memory_regions(&self) -> Option<Vec<DetlogMemoryRegion>> {
        self.kernel.detlog_memory_regions()
    }
}

fn classify_tool_result(result: Result<i64, Error>) -> DrivenSyscall {
    match result {
        Ok(value) => DrivenSyscall::Complete(value),
        Err(error) => match error.into_errno() {
            Ok(errno) => DrivenSyscall::Complete(-(errno.into_raw() as i64)),
            Err(error) => DrivenSyscall::Fatal(error),
        },
    }
}

/// Drive one subscribed syscall through an arbitrary Reverie Tool.
///
/// `global` is the run-owned singleton, passed by direct shared reference.
/// `kernel` is borrowed for exactly this callback, so neither the Tool nor a
/// future can retain the kernel's native transition after dispatch ends.
pub async fn drive_syscall<T, K>(
    tool: &T,
    global: &T::GlobalState,
    config: &<T::GlobalState as GlobalTool>::Config,
    thread_state: &mut T::ThreadState,
    kernel: &mut K,
    original: NarfSyscallRequest,
    syscall: Syscall,
) -> DrivenSyscall
where
    T: Tool,
    K: NarfKernel,
{
    let tail = TailCell::default();
    let mut guest = NarfGuest::<T, K> {
        kernel,
        global,
        config,
        thread_state,
        original,
        tail: &tail,
    };
    let mut future = tool.handle_syscall_event(&mut guest, syscall);
    poll_fn(|context| poll_tool_future(future.as_mut(), &tail, context)).await
}

fn poll_tool_future<F>(
    mut future: Pin<&mut F>,
    tail: &TailCell,
    context: &mut Context<'_>,
) -> Poll<DrivenSyscall>
where
    F: Future<Output = Result<i64, Error>> + ?Sized,
{
    match future.as_mut().poll(context) {
        Poll::Ready(result) => Poll::Ready(classify_tool_result(result)),
        Poll::Pending => match tail.take() {
            Some(outcome) => Poll::Ready(outcome),
            None => Poll::Pending,
        },
    }
}

#[cfg(test)]
mod tests {
    use core::sync::atomic::AtomicU64;
    use core::sync::atomic::AtomicUsize;

    use reverie::GlobalTool;
    use reverie::Subscription;
    use reverie::syscalls::Getpid;
    use reverie::syscalls::Getppid;
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

    struct EmptyGuard;
    impl Drop for EmptyGuard {
        fn drop(&mut self) {}
    }

    #[derive(Default)]
    struct EmptyStack;
    impl Stack for EmptyStack {
        type StackGuard = EmptyGuard;

        fn size(&self) -> usize {
            0
        }

        fn capacity(&self) -> usize {
            0
        }

        fn push<'stack, V>(&mut self, _value: V) -> reverie::syscalls::Addr<'stack, V> {
            panic!("stack access is outside this test")
        }

        fn reserve<'stack, V>(&mut self) -> reverie::syscalls::AddrMut<'stack, V> {
            panic!("stack access is outside this test")
        }

        fn commit(self) -> Result<Self::StackGuard, Errno> {
            Ok(EmptyGuard)
        }
    }

    struct FakeKernel {
        original_calls: AtomicUsize,
        injected_calls: AtomicUsize,
    }

    impl FakeKernel {
        fn new() -> Self {
            Self {
                original_calls: AtomicUsize::new(0),
                injected_calls: AtomicUsize::new(0),
            }
        }
    }

    impl NarfKernel for FakeKernel {
        type Memory = LocalMemory;
        type Stack = EmptyStack;

        fn tid(&self) -> i32 {
            101
        }

        fn pid(&self) -> i32 {
            101
        }

        fn ppid(&self) -> Option<i32> {
            None
        }

        fn auxv_entries(&self) -> Vec<(libc::c_ulong, libc::c_ulong)> {
            Vec::new()
        }

        fn memory(&self) -> Self::Memory {
            LocalMemory::new()
        }

        fn regs(&mut self) -> libc::user_regs_struct {
            // SAFETY: Linux's user_regs_struct accepts the all-zero bit pattern.
            unsafe { core::mem::zeroed() }
        }

        fn set_regs(&mut self, _regs: libc::user_regs_struct) -> Result<(), Errno> {
            Ok(())
        }

        fn stack(&mut self) -> Self::Stack {
            EmptyStack
        }

        fn daemonize(&mut self) {}

        fn execute_original(&mut self) -> Result<NarfSyscallOutcome, OriginalAlreadyExecuted> {
            if self.original_calls.fetch_add(1, Ordering::Relaxed) == 0 {
                Ok(NarfSyscallOutcome::Returned(37))
            } else {
                Err(OriginalAlreadyExecuted)
            }
        }

        fn execute_injected(&mut self, _request: NarfSyscallRequest) -> NarfSyscallOutcome {
            self.injected_calls.fetch_add(1, Ordering::Relaxed);
            NarfSyscallOutcome::Returned(99)
        }

        fn set_timer(&mut self, _schedule: TimerSchedule) -> Result<(), Error> {
            Err(Errno::ENOSYS.into())
        }

        fn set_timer_precise(&mut self, _schedule: TimerSchedule) -> Result<(), Error> {
            Err(Errno::ENOSYS.into())
        }

        fn read_clock(&mut self) -> Result<u64, Error> {
            Ok(0)
        }
    }

    #[test]
    fn arbitrary_tool_preserves_versioned_original_and_uses_direct_global_state() {
        let tool = ProbeTool;
        let global = SharedGlobal::default();
        let mut thread_state = 0;
        let mut kernel = FakeKernel::new();
        let syscall = Syscall::Getpid(Getpid::new());
        let original = NarfSyscallRequest {
            number: (7 << 24) | libc::SYS_getpid as u32,
            args: [0; 6],
        };

        let outcome = futures::executor::block_on(drive_syscall(
            &tool,
            &global,
            &(),
            &mut thread_state,
            &mut kernel,
            original,
            syscall,
        ));

        assert!(matches!(outcome, DrivenSyscall::Complete(42)));
        assert_eq!(thread_state, 1);
        assert_eq!(global.sum.load(Ordering::Relaxed), 5);
        assert_eq!(kernel.original_calls.load(Ordering::Relaxed), 1);
        assert_eq!(kernel.injected_calls.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn distinct_typed_request_uses_injected_transition() {
        let mut thread_state = 0;
        let mut kernel = FakeKernel::new();
        let outcome = futures::executor::block_on(drive_syscall(
            &InjectOtherTool,
            &SharedGlobal::default(),
            &(),
            &mut thread_state,
            &mut kernel,
            NarfSyscallRequest {
                number: (7 << 24) | libc::SYS_getpid as u32,
                args: [0; 6],
            },
            Syscall::Getpid(Getpid::new()),
        ));

        assert!(matches!(outcome, DrivenSyscall::Complete(99)));
        assert_eq!(kernel.original_calls.load(Ordering::Relaxed), 0);
        assert_eq!(kernel.injected_calls.load(Ordering::Relaxed), 1);
    }
}
