/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

// Intended integration: a cfg(test) child module of tool_host::driver.
// These are unit controls of the real common driver, not native execution.

use std::cell::Cell;
use std::future::pending;
use std::future::poll_fn;
use std::sync::Arc;
use std::sync::atomic::AtomicUsize;
use std::task::Wake;

use super::*;
use crate::GlobalRPC;
use crate::Never;
use crate::Stack;
use crate::TimerSchedule;
use crate::syscalls::Addr;
use crate::syscalls::AddrMut;
use crate::syscalls::Getpid;
use crate::syscalls::LocalMemory;

#[derive(Default)]
struct WakeCount(AtomicUsize);

impl Wake for WakeCount {
    fn wake(self: Arc<Self>) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

struct ObservedDriver {
    waker: Waker,
    wake_count: Arc<WakeCount>,
    pending_calls: Cell<usize>,
}

impl PendingDriver for ObservedDriver {
    fn waker(&self) -> &Waker {
        &self.waker
    }

    fn on_pending(&self) {
        assert_eq!(self.wake_count.0.load(Ordering::SeqCst), 1);
        assert_eq!(self.pending_calls.replace(1), 0);
    }
}

#[test]
fn pending_driver_receives_the_actual_future_wake() {
    let wake_count = Arc::new(WakeCount::default());
    let driver = ObservedDriver {
        waker: Waker::from(Arc::clone(&wake_count)),
        wake_count,
        pending_calls: Cell::new(0),
    };
    let mut polls = 0;
    let value = drive_ready_with(
        poll_fn(|cx| {
            assert!(cx.waker().will_wake(driver.waker()));
            polls += 1;
            if polls == 1 {
                cx.waker().wake_by_ref();
                Poll::Pending
            } else {
                assert_eq!(driver.pending_calls.get(), 1);
                Poll::Ready(42)
            }
        }),
        &driver,
    );
    assert_eq!(value, 42);
    assert_eq!(polls, 2);
    assert_eq!(driver.pending_calls.get(), 1);
    assert_eq!(driver.wake_count.0.load(Ordering::SeqCst), 1);
}

#[derive(Clone, Copy, Default)]
enum Scenario {
    #[default]
    RestartTwice,
    ExecuteOriginal,
    IgnoreRefusal,
    IgnoreExit,
}

#[derive(Default)]
struct ScriptedTool {
    scenario: Scenario,
    calls: AtomicUsize,
    dropped_callbacks: AtomicUsize,
}

struct CallbackDrop<'a>(&'a AtomicUsize);

impl Drop for CallbackDrop<'_> {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

#[crate::tool]
impl Tool for ScriptedTool {
    type GlobalState = ();
    type ThreadState = u64;

    async fn handle_syscall_event<G: Guest<Self>>(
        &self,
        guest: &mut G,
        syscall: Syscall,
    ) -> Result<i64, Error> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        // The previous callback must have released its borrowed state before
        // a restart re-enters the same Tool with the same Guest.
        assert_eq!(self.dropped_callbacks.load(Ordering::SeqCst), call);
        let _drop = CallbackDrop(&self.dropped_callbacks);
        *guest.thread_state_mut() += 1;
        match self.scenario {
            Scenario::RestartTwice if call < 2 => Err(Errno::ERESTARTSYS.into()),
            Scenario::RestartTwice => Ok(42),
            Scenario::ExecuteOriginal => guest.tail_inject(syscall).await,
            Scenario::IgnoreRefusal | Scenario::IgnoreExit => {
                // Deliberately swallow the injection result. A staged terminal
                // action must still defeat this later ordinary Ready result.
                let _ = guest.inject(syscall).await;
                Ok(999)
            }
        }
    }
}

struct TestGuest<'a> {
    tail: &'a TailResult,
    scenario: Scenario,
    state: u64,
    injected: usize,
}

#[crate::global_tool]
impl GlobalRPC<()> for TestGuest<'_> {
    async fn send_rpc(&self, _: ()) {
        panic!("driver fixture does not issue RPCs");
    }

    fn config(&self) -> &() {
        &()
    }
}

#[crate::tool]
impl Guest<ScriptedTool> for TestGuest<'_> {
    type Memory = LocalMemory;
    type Stack = UnusedStack;

    fn tid(&self) -> Pid {
        Pid::from_raw(17)
    }
    fn pid(&self) -> Pid {
        self.tid()
    }
    fn ppid(&self) -> Option<Pid> {
        None
    }
    fn memory(&self) -> LocalMemory {
        panic!("no memory access in driver control")
    }
    fn thread_state_mut(&mut self) -> &mut u64 {
        &mut self.state
    }
    fn thread_state(&self) -> &u64 {
        &self.state
    }
    async fn regs(&mut self) -> libc::user_regs_struct {
        panic!("no registers")
    }
    async fn stack(&mut self) -> UnusedStack {
        panic!("no stack access")
    }
    async fn daemonize(&mut self) {
        panic!("no daemonization")
    }

    async fn inject<S: SyscallInfo>(&mut self, _: S) -> Result<i64, Errno> {
        self.injected += 1;
        match self.scenario {
            Scenario::IgnoreRefusal => self.tail.refuse_initial_root_operation(libc::SYS_clone),
            Scenario::IgnoreExit => self
                .tail
                .set_exit(libc::SYS_exit_group, [42, 0, 0, 0, 0, 0]),
            _ => panic!("unexpected returning injection"),
        }
        Err(Errno::ENOSYS)
    }

    async fn tail_inject<S: SyscallInfo>(&mut self, syscall: S) -> Never {
        self.injected += 1;
        let (number, args) = syscall.into_parts();
        self.tail
            .set_execute_original(Syscall::from_raw(number, args));
        pending().await
    }

    fn set_timer(&mut self, _: TimerSchedule) -> Result<(), Error> {
        panic!("no timer")
    }
    fn set_timer_precise(&mut self, _: TimerSchedule) -> Result<(), Error> {
        panic!("no timer")
    }
    fn read_clock(&mut self) -> Result<u64, Error> {
        panic!("no clock")
    }
}

struct UnusedStack;
struct UnusedStackGuard;
impl Drop for UnusedStackGuard {
    fn drop(&mut self) {}
}
impl Stack for UnusedStack {
    type StackGuard = UnusedStackGuard;
    fn size(&self) -> usize {
        panic!("no stack access")
    }
    fn capacity(&self) -> usize {
        panic!("no stack access")
    }
    fn push<'stack, T>(&mut self, _: T) -> Addr<'stack, T> {
        panic!("no stack access")
    }
    fn reserve<'stack, T>(&mut self) -> AddrMut<'stack, T> {
        panic!("no stack access")
    }
    fn commit(self) -> Result<UnusedStackGuard, Errno> {
        panic!("no stack access")
    }
}

fn fixture(scenario: Scenario, tail: &TailResult) -> (ScriptedTool, TestGuest<'_>) {
    (
        ScriptedTool {
            scenario,
            ..ScriptedTool::default()
        },
        TestGuest {
            tail,
            scenario,
            state: 0,
            injected: 0,
        },
    )
}

#[test]
fn real_tool_callback_restarts_twice_without_exposing_private_errno() {
    let tail = TailResult::default();
    let (tool, mut guest) = fixture(Scenario::RestartTwice, &tail);
    let outcome = drive_tool_syscall_with(
        &tool,
        &mut guest,
        Getpid::default().into(),
        &tail,
        &SpinDriver,
    );
    assert!(matches!(
        outcome,
        NativeSyscallOutcome::Complete(DrivenSyscall::Result(42))
    ));
    assert_eq!(tool.calls.load(Ordering::SeqCst), 3);
    assert_eq!(tool.dropped_callbacks.load(Ordering::SeqCst), 3);
    assert_eq!(guest.state, 3);
    assert_eq!(guest.injected, 0);
}

#[test]
fn execute_original_is_returned_only_after_the_tool_future_drops() {
    let tail = TailResult::default();
    let (tool, mut guest) = fixture(Scenario::ExecuteOriginal, &tail);
    let outcome = drive_tool_syscall_with(
        &tool,
        &mut guest,
        Getpid::default().into(),
        &tail,
        &SpinDriver,
    );
    let NativeSyscallOutcome::ExecuteOriginal(syscall) = outcome else {
        panic!("expected the deferred physical operation");
    };
    assert_eq!(syscall.number(), Sysno::getpid);
    assert_eq!(tool.calls.load(Ordering::SeqCst), 1);
    assert_eq!(tool.dropped_callbacks.load(Ordering::SeqCst), 1);
    assert_eq!(guest.injected, 1);
    // Reborrow is legal here, after the Tool future is gone. This unit control
    // does not execute Linux or claim the caller has also dropped its lease.
    *guest.thread_state_mut() += 10;
    assert_eq!(guest.state, 11);
}

#[test]
fn staged_refusal_wins_over_an_actual_tool_ready_result() {
    let tail = TailResult::default();
    let (tool, mut guest) = fixture(Scenario::IgnoreRefusal, &tail);
    let outcome = drive_tool_syscall_with(
        &tool,
        &mut guest,
        Getpid::default().into(),
        &tail,
        &SpinDriver,
    );
    let NativeSyscallOutcome::Complete(DrivenSyscall::Fatal(error)) = outcome else {
        panic!("a swallowed refusal must not become success");
    };
    assert_eq!(error.to_string(), initial_root_refusal(libc::SYS_clone));
    assert!(tail.ending());
    assert_eq!(guest.injected, 1);
    assert_eq!(tool.calls.load(Ordering::SeqCst), 1);
    assert_eq!(tool.dropped_callbacks.load(Ordering::SeqCst), 1);
}

#[test]
fn staged_exit_wins_over_an_actual_tool_ready_result() {
    let tail = TailResult::default();
    let (tool, mut guest) = fixture(Scenario::IgnoreExit, &tail);
    let outcome = drive_tool_syscall_with(
        &tool,
        &mut guest,
        Getpid::default().into(),
        &tail,
        &SpinDriver,
    );
    assert!(matches!(
        outcome,
        NativeSyscallOutcome::Complete(DrivenSyscall::Exit {
            number: libc::SYS_exit_group,
            args: [42, 0, 0, 0, 0, 0],
        })
    ));
    assert!(tail.ending());
    assert_eq!(guest.injected, 1);
    assert_eq!(tool.calls.load(Ordering::SeqCst), 1);
    assert_eq!(tool.dropped_callbacks.load(Ordering::SeqCst), 1);
}

#[test]
fn legacy_trap_driver_refuses_a_native_deferred_operation() {
    let tail = TailResult::default();
    let (tool, mut guest) = fixture(Scenario::ExecuteOriginal, &tail);
    let outcome = drive_tool_syscall(&tool, &mut guest, Getpid::default().into(), &tail);
    let DrivenSyscall::Fatal(error) = outcome else {
        panic!("legacy driver must not fabricate a physical syscall result");
    };
    assert_eq!(
        error.to_string(),
        "deferred physical syscall requires a native Tool driver"
    );
    assert_eq!(guest.injected, 1);
    assert_eq!(tool.dropped_callbacks.load(Ordering::SeqCst), 1);
}

#[test]
fn first_terminal_action_cannot_be_replaced_after_abandoning_a_future() {
    let tail = TailResult::default();
    tail.refuse_initial_root_operation(libc::SYS_clone);
    tail.set_exit(libc::SYS_exit_group, [0; 6]);
    tail.set_result(999);
    tail.set_execute_original(Getpid::default().into());
    tail.set_fork_child(
        Pid::from_raw(1),
        Pid::from_raw(1),
        Pid::from_raw(2),
        Pid::from_raw(2),
    );
    assert!(matches!(
        tail.take(),
        Some(TailAction::Refused {
            number: libc::SYS_clone
        })
    ));
    assert!(tail.ending());
}
