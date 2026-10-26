/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

// Intended integration: a cfg(test) child module of tool_host::owner. Private
// lock inspection is deliberate: try_borrow alone only tests the first lock.
// The port below is a unit-test port, not a kernel socket or a runtime claim.

use std::future::pending;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::AtomicUsize;
use std::task::Context;
use std::task::Waker;

use super::*;
use crate::Never;
use crate::Stack;
use crate::TimerSchedule;
use crate::syscalls::Addr;
use crate::syscalls::AddrMut;
use crate::syscalls::Errno;
use crate::syscalls::Getpid;
use crate::syscalls::LocalMemory;
use crate::syscalls::Syscall;
use crate::syscalls::SyscallInfo;
use crate::tool_host::SpinDriver;
use crate::tool_host::drive_ready_with;

const CONFIG: u64 = 1000;
const CONTEXT: u64 = 42;

#[derive(Debug, PartialEq, Eq)]
enum Event {
    Init(i32),
    ThreadStart(i32, u64),
    PostExec(i32, u64),
    Rpc(u64, u64),
    Pending,
    FutureDropped,
    ThreadExit(i32, u64, ExitStatus),
    ProcessExit(i32, ExitStatus),
}

type Events = Arc<Mutex<Vec<Event>>>;

#[derive(Default)]
struct TestGlobal;

#[crate::global_tool]
impl GlobalTool for TestGlobal {
    type Request = u64;
    type Response = u64;
    type Config = u64;

    async fn receive_rpc(&self, _: Pid, _: u64) -> u64 {
        panic!("the owner must use the supplied port, not a substitute GlobalTool");
    }
}

struct TestConnection {
    config: u64,
    events: Events,
}

struct TestPort<'a> {
    connection: &'a TestConnection,
    context: u64,
}

impl CallbackPorts<TestGlobal> for TestConnection {
    type Context<'call>
        = u64
    where
        Self: 'call;
    type Port<'call>
        = TestPort<'call>
    where
        Self: 'call;

    fn config(&self) -> &u64 {
        &self.config
    }

    fn borrow<'call>(
        &'call self,
        context: Self::Context<'call>,
    ) -> Result<Self::Port<'call>, Error> {
        if context != CONTEXT {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "wrong test callback context",
            )
            .into());
        }
        Ok(TestPort {
            connection: self,
            context,
        })
    }
}

#[crate::global_tool]
impl GlobalRPC<TestGlobal> for TestPort<'_> {
    async fn send_rpc(&self, message: u64) -> u64 {
        self.connection
            .events
            .lock()
            .unwrap()
            .push(Event::Rpc(self.context, message));
        self.connection.config + message
    }

    fn config(&self) -> &u64 {
        &self.connection.config
    }
}

#[derive(Default)]
struct TestTool {
    events: Events,
    fail_start: bool,
}

struct PendingBorrowDrop(Events);

impl Drop for PendingBorrowDrop {
    fn drop(&mut self) {
        self.0.lock().unwrap().push(Event::FutureDropped);
    }
}

#[crate::tool]
impl Tool for TestTool {
    type GlobalState = TestGlobal;
    type ThreadState = u64;

    fn init_thread_state(&self, tid: Pid, parent: Option<(Pid, &u64)>) -> u64 {
        assert!(
            parent.is_none(),
            "these controls do not synthesize fork state"
        );
        self.events.lock().unwrap().push(Event::Init(tid.as_raw()));
        37
    }

    async fn handle_thread_start<G: Guest<Self>>(&self, guest: &mut G) -> Result<(), Error> {
        self.events.lock().unwrap().push(Event::ThreadStart(
            guest.tid().as_raw(),
            *guest.thread_state(),
        ));
        if self.fail_start {
            return Err(io::Error::other("test startup failed").into());
        }
        assert_eq!(*guest.config(), CONFIG);
        assert_eq!(guest.send_rpc(1).await, CONFIG + 1);
        *guest.thread_state_mut() += 1;
        Ok(())
    }

    async fn handle_post_exec<G: Guest<Self>>(&self, guest: &mut G) -> Result<(), Errno> {
        self.events
            .lock()
            .unwrap()
            .push(Event::PostExec(guest.tid().as_raw(), *guest.thread_state()));
        assert_eq!(guest.send_rpc(2).await, CONFIG + 2);
        *guest.thread_state_mut() += 10;
        Ok(())
    }

    async fn handle_syscall_event<G: Guest<Self>>(
        &self,
        guest: &mut G,
        _: Syscall,
    ) -> Result<i64, Error> {
        assert_eq!(*guest.thread_state(), 48);
        self.events.lock().unwrap().push(Event::Pending);
        let _drop = PendingBorrowDrop(Arc::clone(&self.events));
        pending::<()>().await;
        // Keep the actual state borrow in the future across the Pending point.
        *guest.thread_state_mut() += 1;
        Ok(7)
    }

    async fn on_exit_thread<G: GlobalRPC<TestGlobal>>(
        &self,
        tid: Pid,
        port: &G,
        state: u64,
        status: ExitStatus,
    ) -> Result<(), Error> {
        self.events
            .lock()
            .unwrap()
            .push(Event::ThreadExit(tid.as_raw(), state, status));
        assert_eq!(port.send_rpc(3).await, CONFIG + 3);
        Ok(())
    }

    async fn on_exit_process<G: GlobalRPC<TestGlobal>>(
        self,
        pid: Pid,
        port: &G,
        status: ExitStatus,
    ) -> Result<(), Error> {
        self.events
            .lock()
            .unwrap()
            .push(Event::ProcessExit(pid.as_raw(), status));
        assert_eq!(port.send_rpc(4).await, CONFIG + 4);
        Ok(())
    }
}

#[derive(Debug, PartialEq, Eq)]
struct PartsObservation {
    tool: usize,
    state: usize,
    port: usize,
    identity: TaskIdentity,
}

#[derive(Default)]
struct TestFactory {
    observations: Vec<PartsObservation>,
    after_callbacks: AtomicUsize,
}

struct TestGuest<'lease, 'port> {
    state: &'lease mut u64,
    identity: TaskIdentity,
    rpc: &'lease TestPort<'port>,
    after_callbacks: &'lease AtomicUsize,
}

impl<'port> TaskGuestFactory<TestTool, TestPort<'port>> for TestFactory {
    type Guest<'lease>
        = TestGuest<'lease, 'port>
    where
        Self: 'lease,
        TestPort<'port>: 'lease;
    type Ending = ();

    fn guest<'lease>(
        &'lease mut self,
        parts: GuestParts<'lease, TestTool, TestPort<'port>>,
        _: &'lease TailResult,
    ) -> Self::Guest<'lease> {
        self.observations.push(PartsObservation {
            tool: std::ptr::from_ref(parts.tool) as usize,
            state: std::ptr::from_ref(&*parts.state) as usize,
            port: std::ptr::from_ref(parts.rpc) as usize,
            identity: parts.identity,
        });
        TestGuest {
            state: parts.state,
            identity: parts.identity,
            rpc: parts.rpc,
            after_callbacks: &self.after_callbacks,
        }
    }

    fn after_callback<'lease>(_: &TestTool, guest: &mut Self::Guest<'lease>) -> Option<()>
    where
        Self: 'lease,
        TestPort<'port>: 'lease,
    {
        guest.after_callbacks.fetch_add(1, Ordering::SeqCst);
        None
    }
}

#[crate::global_tool]
impl GlobalRPC<TestGlobal> for TestGuest<'_, '_> {
    async fn send_rpc(&self, message: u64) -> u64 {
        self.rpc.send_rpc(message).await
    }
    fn config(&self) -> &u64 {
        self.rpc.config()
    }
}

#[crate::tool]
impl Guest<TestTool> for TestGuest<'_, '_> {
    type Memory = LocalMemory;
    type Stack = UnusedStack;
    fn tid(&self) -> Pid {
        self.identity.tid
    }
    fn pid(&self) -> Pid {
        self.identity.pid
    }
    fn ppid(&self) -> Option<Pid> {
        None
    }
    fn memory(&self) -> LocalMemory {
        panic!("no memory access in owner control")
    }
    fn thread_state_mut(&mut self) -> &mut u64 {
        self.state
    }
    fn thread_state(&self) -> &u64 {
        self.state
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
        panic!("no injection")
    }
    async fn tail_inject<S: SyscallInfo>(&mut self, _: S) -> Never {
        panic!("no injection")
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

fn root() -> TaskIdentity {
    TaskIdentity {
        tid: Pid::from_raw(17),
        pid: Pid::from_raw(17),
    }
}

fn fixture(
    domain: AdmissionDomain,
    fail_start: bool,
) -> (ToolOwner<TestTool, TestConnection>, Events) {
    let events = Events::default();
    let tool = TestTool {
        events: Arc::clone(&events),
        fail_start,
    };
    let connection = TestConnection {
        config: CONFIG,
        events: Arc::clone(&events),
    };
    (ToolOwner::new(tool, connection, root(), domain), events)
}

fn start(
    lease: &mut CallbackLease<'_, TestTool, TestConnection>,
    factory: &mut TestFactory,
    tail: &TailResult,
) {
    assert!(
        lease.startup(factory, tail, &SpinDriver).is_ok(),
        "actual startup callbacks must complete"
    );
}

fn exit42(number: i64) -> TerminalEvent {
    TerminalEvent::Syscall {
        number,
        args: [42, 0, 0, 0, 0, 0],
    }
}

#[test]
fn factory_uses_the_owners_exact_tool_state_identity_and_port() {
    let (owner, events) = fixture(AdmissionDomain::InitialRootOnly, false);
    let tail = TailResult::default();
    let mut factory = TestFactory::default();
    let mut lease = owner.try_borrow(CONTEXT).unwrap();
    lease.admit(root()).unwrap();
    let tool_address = std::ptr::from_ref(lease.tool()) as usize;
    let expected = PartsObservation {
        tool: tool_address,
        state: std::ptr::from_ref(lease.state(root().tid).unwrap()) as usize,
        port: std::ptr::from_ref(&lease.port) as usize,
        identity: root(),
    };
    assert!(lease.guest(&mut factory, &tail).is_err());
    start(&mut lease, &mut factory, &tail);
    assert_eq!(factory.observations, [expected]);
    assert_eq!(factory.after_callbacks.load(Ordering::SeqCst), 2);
    assert_eq!(lease.state(root().tid), Some(&48));
    assert_eq!(
        *events.lock().unwrap(),
        [
            Event::Init(17),
            Event::ThreadStart(17, 37),
            Event::Rpc(CONTEXT, 1),
            Event::PostExec(17, 38),
            Event::Rpc(CONTEXT, 2),
        ]
    );
    {
        let (tool, mut guest) = lease.guest(&mut factory, &tail).unwrap();
        assert_eq!(std::ptr::from_ref(tool) as usize, tool_address);
        assert_eq!(guest.tid(), root().tid);
        assert_eq!(guest.pid(), root().pid);
        assert!(std::ptr::eq(guest.config(), &owner.connection().config));
        assert_eq!(drive_ready_with(guest.send_rpc(9), &SpinDriver), CONFIG + 9);
        *guest.thread_state_mut() += 100;
    }
    assert_eq!(lease.state(root().tid), Some(&148));
    assert_eq!(factory.observations[0], factory.observations[1]);
}

#[test]
fn complete_lease_holds_both_locks_and_failed_borrows_do_not_release_them() {
    let (owner, _) = fixture(AdmissionDomain::InitialRootOnly, false);
    let lease = owner.try_borrow(CONTEXT).unwrap();
    for _ in 0..3 {
        assert!(owner.tool.try_lock().is_none());
        assert!(owner.tasks.try_lock().is_none());
        assert!(
            matches!(owner.try_borrow(CONTEXT), Err(Error::Io(error)) if error.kind() == io::ErrorKind::WouldBlock)
        );
    }
    drop(lease);
    assert!(owner.tool.try_lock().is_some());
    assert!(owner.tasks.try_lock().is_some());
    // A failed port binding must release both locks as well.
    assert!(
        matches!(owner.try_borrow(CONTEXT + 1), Err(Error::Io(error)) if error.kind() == io::ErrorKind::PermissionDenied)
    );
    assert!(owner.try_borrow(CONTEXT).is_ok());
}

#[test]
fn initial_root_proof_requires_callbacks_once_and_returns_the_same_connection() {
    let (owner, events) = fixture(AdmissionDomain::InitialRootOnly, false);
    let tail = TailResult::default();
    let mut factory = TestFactory::default();
    assert!(
        owner
            .try_borrow(CONTEXT)
            .unwrap()
            .finish_initial_root()
            .is_err()
    );
    let mut incomplete = owner.try_borrow(CONTEXT).unwrap();
    incomplete.admit(root()).unwrap();
    assert!(incomplete.finish_initial_root().is_err());
    let mut lease = owner.try_borrow(CONTEXT).unwrap();
    lease.admit(root()).unwrap();
    start(&mut lease, &mut factory, &tail);
    let (port, identity) = lease.finish_initial_root().unwrap().into_port();
    assert_eq!(identity, root());
    assert_eq!(port.context, CONTEXT);
    assert!(std::ptr::eq(port.connection, owner.connection()));
    assert!(owner.tool.try_lock().is_some());
    assert!(owner.tasks.try_lock().is_some());
    assert_eq!(drive_ready_with(port.send_rpc(9), &SpinDriver), CONFIG + 9);
    let before_second_start = events.lock().unwrap().len();
    let mut again = owner.try_borrow(CONTEXT).unwrap();
    again.admit(root()).unwrap();
    start(&mut again, &mut factory, &tail);
    assert_eq!(events.lock().unwrap().len(), before_second_start);
    assert_eq!(factory.after_callbacks.load(Ordering::SeqCst), 2);
    assert!(again.finish_initial_root().is_err());
}

#[test]
fn failed_or_restored_startup_cannot_mint_initial_root_completion() {
    let (owner, events) = fixture(AdmissionDomain::InitialRootOnly, true);
    let tail = TailResult::default();
    let mut factory = TestFactory::default();
    let mut lease = owner.try_borrow(CONTEXT).unwrap();
    lease.admit(root()).unwrap();
    assert!(matches!(
        lease.startup(&mut factory, &tail, &SpinDriver),
        Err(StartupFailure::Tool(_))
    ));
    assert!(lease.finish_initial_root().is_err());
    let before_retry = events.lock().unwrap().len();
    let mut lease = owner.try_borrow(CONTEXT).unwrap();
    lease.admit(root()).unwrap();
    assert!(matches!(
        lease.startup(&mut factory, &tail, &SpinDriver),
        Err(StartupFailure::Tool(_))
    ));
    assert_eq!(events.lock().unwrap().len(), before_retry);
    assert!(lease.finish_initial_root().is_err());

    let (restored, _) = fixture(AdmissionDomain::ExistingProcess, false);
    let mut lease = restored.try_borrow(CONTEXT).unwrap();
    lease.restore_started(root().tid, 48).unwrap();
    lease.admit(root()).unwrap();
    assert!(lease.finish_initial_root().is_err());
}

#[test]
fn exit42_retires_thread_then_process_through_the_original_port() {
    let (owner, events) = fixture(AdmissionDomain::ExistingProcess, false);
    let tail = TailResult::default();
    let mut factory = TestFactory::default();
    let mut lease = owner.try_borrow(CONTEXT).unwrap();
    lease.admit(root()).unwrap();
    start(&mut lease, &mut factory, &tail);
    let sibling = TaskIdentity {
        tid: Pid::from_raw(18),
        pid: root().pid,
    };
    lease.admit(sibling).unwrap();
    start(&mut lease, &mut factory, &tail);
    events.lock().unwrap().clear();
    assert!(
        !lease
            .finish_exit(exit42(libc::SYS_exit), &SpinDriver)
            .unwrap()
    );
    assert!(!lease.contains(sibling.tid));
    assert_eq!(lease.state(root().tid), Some(&48));
    lease.admit(root()).unwrap();
    assert!(
        lease
            .finish_exit(exit42(libc::SYS_exit_group), &SpinDriver)
            .unwrap()
    );
    assert!(lease.is_empty());
    assert_eq!(
        *events.lock().unwrap(),
        [
            Event::ThreadExit(18, 38, ExitStatus::Exited(42)),
            Event::Rpc(CONTEXT, 3),
            Event::ThreadExit(17, 48, ExitStatus::Exited(42)),
            Event::Rpc(CONTEXT, 3),
            Event::ProcessExit(17, ExitStatus::Exited(42)),
            Event::Rpc(CONTEXT, 4),
        ]
    );
    drop(lease);
    assert!(owner.try_borrow(CONTEXT).is_err());
}

#[test]
fn a_live_pending_guest_borrow_blocks_retirement_until_future_and_lease_drop() {
    let (owner, events) = fixture(AdmissionDomain::InitialRootOnly, false);
    let tail = TailResult::default();
    let mut factory = TestFactory::default();
    let mut lease = owner.try_borrow(CONTEXT).unwrap();
    lease.admit(root()).unwrap();
    start(&mut lease, &mut factory, &tail);
    events.lock().unwrap().clear();
    {
        let (tool, mut guest) = lease.guest(&mut factory, &tail).unwrap();
        {
            let mut future = tool.handle_syscall_event(&mut guest, Getpid::default().into());
            let mut context = Context::from_waker(Waker::noop());
            assert!(future.as_mut().poll(&mut context).is_pending());
            // Retirement has to acquire the same complete lease. No blocking
            // call is made here, so a mutation cannot hang this unit control.
            assert!(
                matches!(owner.try_borrow(CONTEXT), Err(Error::Io(error)) if error.kind() == io::ErrorKind::WouldBlock)
            );
            assert!(owner.tool.try_lock().is_none());
            assert!(owner.tasks.try_lock().is_none());
            assert_eq!(*events.lock().unwrap(), [Event::Pending]);
        }
        assert_eq!(
            *events.lock().unwrap(),
            [Event::Pending, Event::FutureDropped]
        );
        assert_eq!(*guest.thread_state(), 48);
    }
    drop(lease);
    let mut retirement = owner.try_borrow(CONTEXT).unwrap();
    retirement.admit(root()).unwrap();
    assert!(
        retirement
            .finish_exit(exit42(libc::SYS_exit_group), &SpinDriver)
            .unwrap()
    );
    assert_eq!(
        *events.lock().unwrap(),
        [
            Event::Pending,
            Event::FutureDropped,
            Event::ThreadExit(17, 48, ExitStatus::Exited(42)),
            Event::Rpc(CONTEXT, 3),
            Event::ProcessExit(17, ExitStatus::Exited(42)),
            Event::Rpc(CONTEXT, 4),
        ]
    );
}
