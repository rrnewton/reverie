/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! Drives real `reverie::Tool`s through `reverie_narf_core` and the fake
//! kernel in `support/fake_kernel.rs`.
//!
//! counter1 and counter2 are compiled unmodified from reverie-examples.

extern crate alloc;

#[path = "support/fake_kernel.rs"]
mod fake_kernel;

#[allow(dead_code)]
#[path = "../../reverie-examples/counter1_tool.rs"]
mod counter1_tool;

#[allow(dead_code)]
#[path = "../../reverie-examples/counter2_tool.rs"]
mod counter2_tool;

use core::cell::Cell;
use core::future::Future;
use core::pin::Pin;
use core::sync::atomic::AtomicBool;
use core::sync::atomic::AtomicU64;
use core::sync::atomic::Ordering;
use core::task::Context;
use core::task::Poll;

use async_trait::async_trait;
use fake_kernel::ENOSYS_RET;
use fake_kernel::FAKE_UID;
use fake_kernel::FakeHost;
use fake_kernel::FakeKernel;
use fake_kernel::Native;
use fake_kernel::PIPE_FD;
use fake_kernel::Via;
use fake_kernel::request;
use reverie::Error;
use reverie::ExitStatus;
use reverie::GlobalTool;
use reverie::Guest;
use reverie::Pid;
use reverie::Stack;
use reverie::Subscription;
use reverie::ThreadOwnership;
use reverie::Tool;
use reverie::syscalls::Addr;
use reverie::syscalls::Errno;
use reverie::syscalls::MemoryAccess;
use reverie::syscalls::Syscall;
use reverie::syscalls::SyscallArgs;
use reverie::syscalls::Sysno;
use reverie::syscalls::libc;
use reverie_narf_core::Disposition;
use reverie_narf_core::LifecycleOutcome;
use reverie_narf_core::NarfFatal;
use reverie_narf_core::NarfSyscallRequest;
use reverie_narf_core::RepollWait;
use reverie_narf_core::SyscallEntry;
use reverie_narf_core::TaskExit;

const BASE: usize = 0x10_0000;
const NONE: [u64; 6] = [0; 6];
const CLONE_THREAD: u64 = (libc::CLONE_THREAD | libc::CLONE_VM) as u64;

fn pid(raw: i32) -> Pid {
    Pid::from_raw(raw)
}

fn host<T: Tool + 'static>() -> FakeHost<T>
where
    <T::GlobalState as GlobalTool>::Config: Default,
{
    match FakeHost::<T>::new(Default::default()) {
        Ok(host) => host,
        Err(fatal) => panic!("host: {fatal:?}"),
    }
}

fn complete(result: Result<Disposition, NarfFatal>) -> i64 {
    match result {
        Ok(Disposition::Complete(value)) => value,
        other => panic!("expected a completed syscall, got {other:?}"),
    }
}

fn context_managed(result: Result<Disposition, NarfFatal>) {
    match result {
        Ok(Disposition::ContextManaged) => {}
        other => panic!("expected a context-managed syscall, got {other:?}"),
    }
}

fn exited(process_exited: bool) -> Result<TaskExit, NarfFatal> {
    Ok(TaskExit { process_exited })
}

fn assert_teardowns(kernel: &FakeKernel, expected: &[(i32, Result<TaskExit, NarfFatal>)]) {
    let actual = kernel.teardowns();
    assert_eq!(
        format!("{actual:?}"),
        format!("{expected:?}"),
        "task_exited calls and results"
    );
}

// ----------------------------------------------------------------------------
// counter1

#[test]
fn counter1_counts_and_tail_injects_through_the_core() {
    let host = host::<counter1_tool::CounterLocal>();
    let kernel = FakeKernel::new();
    let root = kernel.spawn_root(&host, BASE);
    assert_eq!(
        kernel.thread_start(&host, root).ok(),
        Some(LifecycleOutcome::Continue)
    );
    kernel.poke(root, BASE + 0x100, b"hello");

    let getpid = request(Sysno::getpid, NONE);
    let write = request(Sysno::write, [1, (BASE + 0x100) as u64, 5, 0, 0, 0]);
    let uname = request(Sysno::uname, NONE);
    assert_eq!(complete(kernel.syscall(&host, root, getpid)), 1000);
    assert_eq!(complete(kernel.syscall(&host, root, write)), 5);
    assert_eq!(complete(kernel.syscall(&host, root, uname)), ENOSYS_RET);

    assert_eq!(host.global().total(), 3);
    assert_eq!(kernel.output(), b"hello");
    let expected: Vec<Native> = [getpid, write, uname]
        .into_iter()
        .map(|request| Native {
            tid: 1000,
            request,
            via: Via::Original,
        })
        .collect();
    assert_eq!(kernel.natives(), expected);
    assert_eq!(kernel.violations(), []);
}

#[test]
fn counter1_follows_threads_forks_and_exits() {
    let host = host::<counter1_tool::CounterLocal>();
    let kernel = FakeKernel::new();
    let root = kernel.spawn_root(&host, BASE);

    let thread = complete(kernel.syscall(
        &host,
        root,
        request(Sysno::clone, [CLONE_THREAD, 0, 0, 0, 0, 0]),
    ));
    assert_eq!(thread, 1001);
    let thread = pid(1001);
    assert_eq!((host.live_threads(), host.live_processes()), (2, 1));
    assert_eq!(
        complete(kernel.syscall(&host, thread, request(Sysno::getpid, NONE))),
        1000
    );
    assert_eq!(
        complete(kernel.syscall(&host, thread, request(Sysno::gettid, NONE))),
        1001
    );

    let child = complete(kernel.syscall(&host, root, request(Sysno::fork, NONE)));
    assert_eq!(child, 1002);
    let child = pid(1002);
    assert_eq!((host.live_threads(), host.live_processes()), (3, 2));
    assert_eq!(
        complete(kernel.syscall(&host, child, request(Sysno::getppid, NONE))),
        1000
    );

    context_managed(kernel.syscall(&host, thread, request(Sysno::exit, NONE)));
    assert_teardowns(&kernel, &[(1001, exited(false))]);
    context_managed(kernel.syscall(&host, child, request(Sysno::exit_group, NONE)));
    assert_teardowns(&kernel, &[(1002, exited(true))]);
    context_managed(kernel.syscall(&host, root, request(Sysno::exit_group, NONE)));
    assert_teardowns(&kernel, &[(1000, exited(true))]);

    assert_eq!((host.live_threads(), host.live_processes()), (0, 0));
    assert_eq!(host.global().total(), 8);
    assert_eq!(kernel.natives().len(), 8);
    assert!(
        kernel
            .natives()
            .iter()
            .all(|native| native.via == Via::Original)
    );
    assert_eq!(kernel.violations(), []);
}

#[test]
fn parked_read_reexecutes_without_calling_the_tool_again() {
    let host = host::<counter1_tool::CounterLocal>();
    let kernel = FakeKernel::new();
    let root = kernel.spawn_root(&host, BASE);
    let read = request(Sysno::read, [PIPE_FD, (BASE + 0x200) as u64, 16, 0, 0, 0]);

    context_managed(kernel.syscall(&host, root, read));
    assert_eq!(host.global().total(), 1);
    // A backstop tick with nothing to read parks the task again.
    context_managed(kernel.reexecute(&host, root));
    assert_eq!(host.global().total(), 1);

    kernel.push_pipe(b"abc");
    assert_eq!(complete(kernel.reexecute(&host, root)), 3);
    assert_eq!(host.global().total(), 1, "the Tool saw the read once");
    assert_eq!(kernel.peek(root, BASE + 0x200, 3), b"abc");
    assert_eq!(kernel.natives().len(), 3);
    assert!(
        kernel
            .natives()
            .iter()
            .all(|n| n.via == Via::Original && n.request == read)
    );

    assert_eq!(
        complete(kernel.syscall(&host, root, request(Sysno::getpid, NONE))),
        1000
    );
    assert_eq!(host.global().total(), 2);
    assert_eq!(kernel.violations(), []);
}

#[test]
fn unexpected_or_mismatched_reexecution_fails_closed() {
    let host = host::<counter1_tool::CounterLocal>();
    let kernel = FakeKernel::new();
    let root = kernel.spawn_root(&host, BASE);
    let getpid = request(Sysno::getpid, NONE);

    let result = kernel.enter(&host, root, SyscallEntry::reexecution(getpid));
    assert!(
        matches!(result, Err(NarfFatal::UnexpectedReexecution)),
        "{result:?}"
    );

    let read = request(Sysno::read, [PIPE_FD, BASE as u64, 1, 0, 0, 0]);
    context_managed(kernel.syscall(&host, root, read));
    let result = kernel.enter(&host, root, SyscallEntry::reexecution(getpid));
    assert!(
        matches!(result, Err(NarfFatal::ReexecutionMismatch { parked, reexecuted })
            if parked == read && reexecuted == getpid),
        "{result:?}"
    );
    assert_eq!(
        kernel.natives().len(),
        1,
        "neither re-execution ran anything"
    );
}

// ----------------------------------------------------------------------------
// counter2: per-thread state and exit teardown (host only: it uses std)

#[test]
fn counter2_tears_down_each_thread_and_process_exactly_once() {
    let host = host::<counter2_tool::CounterLocal>();
    let kernel = FakeKernel::new();
    let root = kernel.spawn_root(&host, BASE);
    let getpid = request(Sysno::getpid, NONE);

    complete(kernel.syscall(&host, root, getpid));
    complete(kernel.syscall(&host, root, request(Sysno::gettid, NONE)));
    let thread = pid(complete(kernel.syscall(
        &host,
        root,
        request(Sysno::clone, [CLONE_THREAD, 0, 0, 0, 0, 0]),
    )) as i32);
    for _ in 0..4 {
        complete(kernel.syscall(&host, thread, getpid));
    }
    assert_eq!(host.with_thread_state(thread, |n| *n), Some(4));
    context_managed(kernel.syscall(&host, thread, request(Sysno::exit, NONE)));
    assert_teardowns(&kernel, &[(1001, exited(false))]);
    assert_eq!(host.with_thread_state(thread, |n| *n), None);
    assert_eq!(
        host.with_process_tool(root, |tool| tool.process_totals()),
        Some((5, 1))
    );

    let child = pid(complete(kernel.syscall(&host, root, request(Sysno::fork, NONE))) as i32);
    complete(kernel.syscall(&host, child, getpid));
    assert_eq!(host.with_thread_state(root, |n| *n), Some(4));
    assert_eq!(host.with_thread_state(child, |n| *n), Some(1));
    context_managed(kernel.syscall(&host, child, request(Sysno::exit_group, NONE)));
    assert_teardowns(&kernel, &[(1002, exited(true))]);
    assert_eq!(host.global().totals(), (2, 1, 1));

    context_managed(kernel.syscall(&host, root, request(Sysno::exit_group, NONE)));
    assert_teardowns(&kernel, &[(1000, exited(true))]);
    assert_eq!(host.global().totals(), (12, 2, 3));

    // A second exit report for any task runs no hook and changes nothing.
    for tid in [1000, 1001, 1002] {
        let again = host.task_exited(pid(tid), ExitStatus::Exited(0), ExitStatus::Exited(0));
        assert!(
            matches!(again, Err(NarfFatal::UnknownTask(t)) if t == pid(tid)),
            "{again:?}"
        );
    }
    assert_eq!(host.global().totals(), (12, 2, 3));
    assert_eq!(kernel.violations(), []);
}

/// The thread-exit lines [`record_thread_exit`] received, in order.
static THREAD_EXIT_LINES: std::sync::Mutex<Vec<(i32, u64)>> = std::sync::Mutex::new(Vec::new());

fn record_thread_exit(tid: Pid, syscalls: u64) {
    THREAD_EXIT_LINES
        .lock()
        .unwrap()
        .push((tid.as_raw(), syscalls));
}

/// A backend without `std` gets counter2's thread-exit line only through a
/// reporter. The host builds the root's Tool and each forked process's Tool
/// with the backend's constructor, so the reporter sees every thread's exit,
/// with that thread's own count.
#[test]
fn counter2_tool_constructor_reaches_every_process() {
    THREAD_EXIT_LINES.lock().unwrap().clear();
    let host = host::<counter2_tool::CounterLocal>().with_tool_constructor(|pid, config| {
        <counter2_tool::CounterLocal as Tool>::new(pid, config)
            .with_thread_exit_reporter(record_thread_exit)
    });
    let kernel = FakeKernel::new();
    let root = kernel.spawn_root(&host, BASE);
    let getpid = request(Sysno::getpid, NONE);

    complete(kernel.syscall(&host, root, getpid));
    let thread = pid(complete(kernel.syscall(
        &host,
        root,
        request(Sysno::clone, [CLONE_THREAD, 0, 0, 0, 0, 0]),
    )) as i32);
    for _ in 0..3 {
        complete(kernel.syscall(&host, thread, getpid));
    }
    context_managed(kernel.syscall(&host, thread, request(Sysno::exit, NONE)));
    let child = pid(complete(kernel.syscall(&host, root, request(Sysno::fork, NONE))) as i32);
    complete(kernel.syscall(&host, child, getpid));
    context_managed(kernel.syscall(&host, child, request(Sysno::exit_group, NONE)));
    context_managed(kernel.syscall(&host, root, request(Sysno::exit_group, NONE)));

    assert_teardowns(
        &kernel,
        &[
            (1001, exited(false)),
            (1002, exited(true)),
            (1000, exited(true)),
        ],
    );
    assert_eq!(
        *THREAD_EXIT_LINES.lock().unwrap(),
        [(1001, 4), (1002, 2), (1000, 4)]
    );
    assert_eq!(host.global().totals(), (10, 2, 3));
    assert_eq!(kernel.violations(), []);
}

// ----------------------------------------------------------------------------
// Guest methods

/// Records what each Guest method reports into the calling thread's state.
#[derive(Default)]
struct GuestView;

#[async_trait]
impl Tool for GuestView {
    type GlobalState = ();
    type ThreadState = Vec<u64>;

    async fn handle_syscall_event<G: Guest<Self>>(
        &self,
        guest: &mut G,
        syscall: Syscall,
    ) -> Result<i64, Error> {
        let regs = guest.regs().await;
        let auxv = guest.auxv();
        let marker: u64 = Addr::from_raw(regs.rdi as usize)
            .and_then(|addr| guest.memory().read_value(addr).ok())
            .unwrap_or(0);
        let view = vec![
            guest.tid().as_raw() as u64,
            guest.pid().as_raw() as u64,
            guest.ppid().map_or(0, |ppid| ppid.as_raw() as u64),
            regs.orig_rax,
            regs.rdi,
            regs.rsp,
            u64::from(auxv.at_uid().unwrap_or(0)),
            u64::from(auxv.at_gid().unwrap_or(0)),
            marker,
        ];
        *guest.thread_state_mut() = view;
        guest.tail_inject(syscall).await
    }
}

#[test]
fn guest_methods_report_the_calling_task() {
    const MARKER: u64 = 0x1122_3344_5566_7788;
    let host = host::<GuestView>();
    let kernel = FakeKernel::new();
    let root = kernel.spawn_root(&host, BASE);
    kernel.poke(root, BASE + 0x10, &MARKER.to_ne_bytes());
    let rsp = kernel.rsp(root);
    let probe = request(Sysno::getpid, [(BASE + 0x10) as u64, 0, 0, 0, 0, 0]);

    let thread = pid(complete(kernel.syscall(
        &host,
        root,
        request(Sysno::clone, [CLONE_THREAD, 0, 0, 0, 0, 0]),
    )) as i32);
    let child = pid(complete(kernel.syscall(&host, root, request(Sysno::fork, NONE))) as i32);
    assert_eq!(complete(kernel.syscall(&host, thread, probe)), 1000);
    assert_eq!(complete(kernel.syscall(&host, child, probe)), 1002);

    let base = (BASE + 0x10) as u64;
    let getpid = Sysno::getpid.id() as u64;
    assert_eq!(
        host.with_thread_state(thread, Vec::clone),
        Some(vec![
            1001, 1000, 0, getpid, base, rsp, FAKE_UID, 1000, MARKER
        ])
    );
    assert_eq!(
        host.with_thread_state(child, Vec::clone),
        Some(vec![
            1002, 1002, 1000, getpid, base, rsp, FAKE_UID, 1002, MARKER
        ])
    );
    // The fork was the leader's last syscall, so its state holds that view.
    let fork = Sysno::fork.id() as u64;
    assert_eq!(
        host.with_thread_state(root, Vec::clone),
        Some(vec![1000, 1000, 0, fork, 0, rsp, FAKE_UID, 1000, 0])
    );
    assert_eq!(kernel.violations(), []);
}

/// Stages bytes on the guest stack and writes them with an injected write.
#[derive(Default)]
struct StackWriter;

#[async_trait]
impl Tool for StackWriter {
    type GlobalState = ();
    type ThreadState = Vec<u64>;

    async fn handle_syscall_event<G: Guest<Self>>(
        &self,
        guest: &mut G,
        _syscall: Syscall,
    ) -> Result<i64, Error> {
        let mut stack = guest.stack().await;
        let addr = stack.push(*b"narfstak");
        let guard = stack.commit()?;
        let busy = guest.stack().await.commit().err();
        let write = Syscall::from_raw(Sysno::write, SyscallArgs::new(1, addr.as_raw(), 8, 0, 0, 0));
        let written = guest.inject(write).await?;
        drop(guard);
        let again = guest.stack().await.commit().err();
        *guest.thread_state_mut() = vec![
            addr.as_raw() as u64,
            busy.map_or(0, |errno| errno.into_raw() as u64),
            again.map_or(0, |errno| errno.into_raw() as u64),
        ];
        Ok(written)
    }
}

#[test]
fn stack_is_staged_below_the_red_zone_and_checked_out_once() {
    let host = host::<StackWriter>();
    let kernel = FakeKernel::new();
    let root = kernel.spawn_root(&host, BASE);
    let rsp = kernel.rsp(root) as usize;
    kernel.poke(root, rsp - 128, &[0xaa; 128]);

    assert_eq!(
        complete(kernel.syscall(&host, root, request(Sysno::getpid, NONE))),
        8
    );

    assert_eq!(kernel.output(), b"narfstak");
    let ebusy = reverie::syscalls::Errno::EBUSY.into_raw() as u64;
    assert_eq!(
        host.with_thread_state(root, Vec::clone),
        Some(vec![(rsp - 128 - 8) as u64, ebusy, 0])
    );
    assert_eq!(
        kernel.peek(root, rsp - 128, 128),
        [0xaa; 128],
        "red zone untouched"
    );
    // The Tool replaced the getpid with its write; the original never ran.
    assert_eq!(kernel.natives().len(), 1);
    assert_eq!(kernel.natives()[0].via, Via::Injected);
}

#[derive(Default)]
struct Daemonizer;

#[async_trait]
impl Tool for Daemonizer {
    type GlobalState = ();
    type ThreadState = ();

    async fn handle_syscall_event<G: Guest<Self>>(
        &self,
        guest: &mut G,
        syscall: Syscall,
    ) -> Result<i64, Error> {
        guest.daemonize().await;
        guest.tail_inject(syscall).await
    }
}

#[test]
fn daemonize_reaches_the_kernel() {
    let host = host::<Daemonizer>();
    let kernel = FakeKernel::new();
    let root = kernel.spawn_root(&host, BASE);
    assert!(!kernel.daemon(root));
    assert_eq!(
        complete(kernel.syscall(&host, root, request(Sysno::getpid, NONE))),
        1000
    );
    assert!(kernel.daemon(root));
}

// ----------------------------------------------------------------------------
// The transition contract

/// Forwards the intercepted syscall twice and returns the sum.
#[derive(Default)]
struct ForwardTwice;

#[async_trait]
impl Tool for ForwardTwice {
    type GlobalState = ();
    type ThreadState = ();

    async fn handle_syscall_event<G: Guest<Self>>(
        &self,
        guest: &mut G,
        syscall: Syscall,
    ) -> Result<i64, Error> {
        let first = guest.inject(syscall).await?;
        let second = guest.inject(syscall).await?;
        Ok(first + second)
    }
}

#[test]
fn repeated_forward_of_the_original_runs_it_natively_once() {
    let host = host::<ForwardTwice>();
    let kernel = FakeKernel::new();
    let root = kernel.spawn_root(&host, BASE);
    let versioned = NarfSyscallRequest {
        number: (7 << 24) | Sysno::getpid.id() as u32,
        args: NONE,
    };

    assert_eq!(complete(kernel.syscall(&host, root, versioned)), 2000);
    assert_eq!(
        kernel.natives(),
        [
            Native {
                tid: 1000,
                request: versioned,
                via: Via::Original
            },
            Native {
                tid: 1000,
                request: versioned,
                via: Via::Injected
            },
        ],
        "the second forward is an injection that keeps the version byte"
    );
    assert_eq!(kernel.violations(), []);
}

/// Injects the intercepted syscall and then keeps going as if it returned.
#[derive(Default)]
struct InjectThenContinue;

std::thread_local! {
    /// What `InjectThenContinue` saw on this test's thread, in order: each
    /// inject's result. (Every test's fake kernel numbers tasks from 1000.)
    static INJECT_RESULTS: core::cell::RefCell<Vec<(i32, Result<i64, i32>)>> =
        const { core::cell::RefCell::new(Vec::new()) };
}

#[async_trait]
impl Tool for InjectThenContinue {
    type GlobalState = ();
    type ThreadState = u64;

    async fn handle_syscall_event<G: Guest<Self>>(
        &self,
        guest: &mut G,
        syscall: Syscall,
    ) -> Result<i64, Error> {
        *guest.thread_state_mut() += 1;
        let result = guest.inject(syscall).await;
        let tid = guest.tid().as_raw();
        INJECT_RESULTS.with_borrow_mut(|results| {
            results.push((tid, result.map_err(|errno| errno.into_raw())))
        });
        // The thread state is reachable again after the await.
        *guest.thread_state_mut() += 100;
        Ok(result? + 1)
    }
}

fn inject_results(tid: Pid) -> Vec<Result<i64, i32>> {
    INJECT_RESULTS.with_borrow(|results| {
        results
            .iter()
            .filter(|(t, _)| *t == tid.as_raw())
            .map(|(_, r)| *r)
            .collect()
    })
}

#[test]
fn inject_that_parks_resumes_the_tool_at_reexecution() {
    let host = host::<InjectThenContinue>();
    let kernel = FakeKernel::new();
    let root = kernel.spawn_root(&host, BASE);
    let read = request(Sysno::read, [PIPE_FD, (BASE + 0x100) as u64, 4, 0, 0, 0]);

    // The inject parks: the Tool is suspended at its await, not failed.
    context_managed(kernel.syscall(&host, root, read));
    assert_eq!(inject_results(root), []);
    // While suspended the thread state is checked in and readable.
    assert_eq!(host.with_thread_state(root, |state| *state), Some(1));
    // A backstop tick with nothing to read parks it again.
    context_managed(kernel.reexecute(&host, root));
    assert_eq!(inject_results(root), []);

    kernel.push_pipe(b"xy");
    // The re-execution runs the read and resumes the same future with its
    // value; the Tool's own result (value + 1) completes the syscall.
    assert_eq!(complete(kernel.reexecute(&host, root)), 3);
    assert_eq!(inject_results(root), [Ok(2)]);
    assert_eq!(host.with_thread_state(root, |state| *state), Some(101));
    assert_eq!(kernel.peek(root, BASE + 0x100, 2), b"xy");
    assert_eq!(
        kernel.natives().len(),
        3,
        "first entry and two re-executions"
    );
    assert!(
        kernel
            .natives()
            .iter()
            .all(|n| n.via == Via::Original && n.request == read)
    );

    // The next syscall is an ordinary new callback.
    let getpid = request(Sysno::getpid, NONE);
    assert_eq!(complete(kernel.syscall(&host, root, getpid)), 1001);
    assert_eq!(inject_results(root), [Ok(2), Ok(1000)]);
    assert_eq!(kernel.violations(), []);
}

#[test]
fn interrupted_parked_inject_returns_erestartsys_like_ptrace() {
    let host = host::<InjectThenContinue>();
    let kernel = FakeKernel::new();
    let root = kernel.spawn_root(&host, BASE);
    let read = request(Sysno::read, [PIPE_FD, BASE as u64, 1, 0, 0, 0]);

    context_managed(kernel.syscall(&host, root, read));
    // A different entry arrives instead of the re-execution, as when a
    // signal handler runs while the task is parked: the suspended inject
    // gets ERESTARTSYS, its result is discarded, and the new entry is
    // handled as a callback of its own.
    let getpid = request(Sysno::getpid, NONE);
    assert_eq!(complete(kernel.syscall(&host, root, getpid)), 1001);
    assert_eq!(
        inject_results(root),
        [Err(Errno::ERESTARTSYS.into_raw()), Ok(1000)]
    );
    assert_eq!(host.with_thread_state(root, |state| *state), Some(202));
    assert_eq!(
        kernel.natives().len(),
        2,
        "nothing ran for the interrupted Tool"
    );
    assert_eq!(kernel.violations(), []);
}

/// Keeps injecting after its parked inject was interrupted.
#[derive(Default)]
struct InjectAfterInterruption;

#[async_trait]
impl Tool for InjectAfterInterruption {
    type GlobalState = ();
    type ThreadState = ();

    async fn handle_syscall_event<G: Guest<Self>>(
        &self,
        guest: &mut G,
        syscall: Syscall,
    ) -> Result<i64, Error> {
        let result = guest.inject(syscall).await;
        if result.is_err() {
            let _ = guest
                .inject(Syscall::from_raw(
                    Sysno::getpid,
                    SyscallArgs::new(0, 0, 0, 0, 0, 0),
                ))
                .await;
        }
        Ok(result?)
    }
}

#[test]
fn transition_after_interruption_fails_closed() {
    let host = host::<InjectAfterInterruption>();
    let kernel = FakeKernel::new();
    let root = kernel.spawn_root(&host, BASE);
    let read = request(Sysno::read, [PIPE_FD, BASE as u64, 1, 0, 0, 0]);

    context_managed(kernel.syscall(&host, root, read));
    let result = kernel.syscall(&host, root, request(Sysno::getpid, NONE));
    assert!(
        matches!(result, Err(NarfFatal::TransitionAfterInterruption)),
        "{result:?}"
    );
    assert_eq!(kernel.natives().len(), 1, "the late inject did not run");
}

/// Starts an inject, lets it park, and returns without awaiting it again.
#[derive(Default)]
struct AbandonParkedInject;

#[async_trait]
impl Tool for AbandonParkedInject {
    type GlobalState = ();
    type ThreadState = ();

    async fn handle_syscall_event<G: Guest<Self>>(
        &self,
        guest: &mut G,
        syscall: Syscall,
    ) -> Result<i64, Error> {
        let mut inject = guest.inject(syscall);
        let parked = core::future::poll_fn(|cx| Poll::Ready(inject.as_mut().poll(cx))).await;
        assert!(parked.is_pending());
        Ok(7)
    }
}

#[test]
fn abandoning_a_parked_inject_fails_closed() {
    let host = host::<AbandonParkedInject>();
    let kernel = FakeKernel::new();
    let root = kernel.spawn_root(&host, BASE);
    let read = request(Sysno::read, [PIPE_FD, BASE as u64, 1, 0, 0, 0]);

    let result = kernel.syscall(&host, root, read);
    assert!(
        matches!(result, Err(NarfFatal::InjectParked { number }) if number == read.number),
        "{result:?}"
    );
}

/// A lifecycle callback whose inject parks: no guest syscall re-executes.
#[derive(Default)]
struct ParkAtThreadStart;

#[async_trait]
impl Tool for ParkAtThreadStart {
    type GlobalState = ();
    type ThreadState = ();

    async fn handle_thread_start<G: Guest<Self>>(&self, guest: &mut G) -> Result<(), Error> {
        let args = SyscallArgs::new(PIPE_FD as usize, BASE, 1, 0, 0, 0);
        guest.inject(Syscall::from_raw(Sysno::read, args)).await?;
        Ok(())
    }
}

#[test]
fn lifecycle_inject_that_parks_fails_closed() {
    let host = host::<ParkAtThreadStart>();
    let kernel = FakeKernel::new();
    let root = kernel.spawn_root(&host, BASE);

    let result = kernel.thread_start(&host, root);
    assert!(
        matches!(result, Err(NarfFatal::InjectParked { number }) if number == Sysno::read.id() as u32),
        "{result:?}"
    );
}

/// A suspended Tool whose task exits drops cleanly and releases its Tool.
#[test]
fn exit_while_suspended_tears_down_the_process() {
    let host = host::<InjectThenContinue>();
    let kernel = FakeKernel::new();
    let root = kernel.spawn_root(&host, BASE);
    let read = request(Sysno::read, [PIPE_FD, BASE as u64, 1, 0, 0, 0]);

    context_managed(kernel.syscall(&host, root, read));
    // The kernel kills the parked task without re-executing its syscall.
    assert!(matches!(
        host.task_exited(root, ExitStatus::Exited(9), ExitStatus::Exited(9)),
        Ok(TaskExit {
            process_exited: true
        })
    ));
    assert_eq!(host.live_processes(), 0);
    assert_eq!(inject_results(root), [], "the suspended Tool never resumed");
}

#[test]
fn inject_of_exit_is_terminal_not_a_failure() {
    let host = host::<InjectThenContinue>();
    let kernel = FakeKernel::new();
    let root = kernel.spawn_root(&host, BASE);

    context_managed(kernel.syscall(&host, root, request(Sysno::exit_group, [3, 0, 0, 0, 0, 0])));
    assert_teardowns(&kernel, &[(1000, exited(true))]);
    assert_eq!(kernel.violations(), []);
}

/// A future that is pending on its first poll and never wakes anyone.
struct YieldOnce(bool);

impl Future for YieldOnce {
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<()> {
        if self.0 {
            Poll::Ready(())
        } else {
            self.0 = true;
            Poll::Pending
        }
    }
}

#[derive(Default)]
struct Steps(AtomicU64);

#[async_trait]
impl GlobalTool for Steps {
    type Request = u64;
    type Response = ();
    type Config = ();

    async fn receive_rpc(&self, _from: Pid, step: u64) {
        self.0.fetch_add(step, Ordering::SeqCst);
    }
}

/// Suspends on something other than a Guest transition.
#[derive(Default)]
struct Suspender;

#[async_trait]
impl Tool for Suspender {
    type GlobalState = Steps;
    type ThreadState = ();

    async fn handle_syscall_event<G: Guest<Self>>(
        &self,
        guest: &mut G,
        syscall: Syscall,
    ) -> Result<i64, Error> {
        guest.send_rpc(1).await;
        YieldOnce(false).await;
        guest.send_rpc(100).await;
        guest.tail_inject(syscall).await
    }
}

#[test]
fn pending_tool_fails_closed_after_one_poll() {
    let host = host::<Suspender>();
    let kernel = FakeKernel::new();
    let root = kernel.spawn_root(&host, BASE);

    let result = kernel.syscall(&host, root, request(Sysno::getpid, NONE));
    assert!(
        matches!(result, Err(NarfFatal::ToolSuspended)),
        "{result:?}"
    );
    assert_eq!(
        host.global().0.load(Ordering::SeqCst),
        1,
        "polled exactly once"
    );
    assert_eq!(kernel.natives(), [], "nothing ran natively");
    assert_eq!(
        kernel.repoll_waits(),
        [1000],
        "the kernel was asked to wait and could not"
    );
    // The thread's state was checked back in; the task is not wedged.
    assert_eq!(host.with_thread_state(root, |_| ()), Some(()));
}

#[test]
fn pending_tool_is_polled_again_after_the_kernel_yields() {
    let host = host::<Suspender>();
    let kernel = FakeKernel::new();
    let root = kernel.spawn_root(&host, BASE);
    kernel.script_repoll(&[RepollWait::Yielded]);

    // The first poll stops at `YieldOnce`. After the kernel let other tasks
    // run, the second poll finishes with the tail inject.
    let getpid = request(Sysno::getpid, NONE);
    assert_eq!(complete(kernel.syscall(&host, root, getpid)), 1000);
    assert_eq!(host.global().0.load(Ordering::SeqCst), 101);
    assert_eq!(kernel.repoll_waits(), [1000], "one wait, between the polls");
    assert_eq!(
        kernel.natives(),
        [Native {
            tid: 1000,
            request: getpid,
            via: Via::Original
        }]
    );
    assert_eq!(kernel.violations(), []);
}

/// Meets two tasks' callbacks through the global state: a `getpid` callback
/// finishes only once a `gettid` callback has run. Every syscall is then
/// forwarded.
#[derive(Default)]
struct Rendezvous;

/// Whether `Rendezvous`'s `gettid` callback has run.
#[derive(Default)]
struct Meeting(AtomicBool);

/// `Meeting`'s requests; each answers whether the `gettid` callback ran.
const ASK: u64 = 0;
const ARRIVE: u64 = 1;

#[async_trait]
impl GlobalTool for Meeting {
    type Request = u64;
    type Response = bool;
    type Config = ();

    async fn receive_rpc(&self, _from: Pid, request: u64) -> bool {
        if request == ARRIVE {
            self.0.store(true, Ordering::SeqCst);
        }
        self.0.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl Tool for Rendezvous {
    type GlobalState = Meeting;
    type ThreadState = ();

    async fn handle_syscall_event<G: Guest<Self>>(
        &self,
        guest: &mut G,
        syscall: Syscall,
    ) -> Result<i64, Error> {
        match syscall {
            Syscall::Getpid(_) => {
                while !guest.send_rpc(ASK).await {
                    YieldOnce(false).await;
                }
            }
            Syscall::Gettid(_) => {
                guest.send_rpc(ARRIVE).await;
            }
            _ => {}
        }
        guest.tail_inject(syscall).await
    }
}

#[test]
fn waiting_tool_finishes_once_another_tasks_callback_runs() {
    let host = host::<Rendezvous>();
    let kernel = FakeKernel::new();
    let root = kernel.spawn_root(&host, BASE);
    let child = complete(kernel.syscall(&host, root, request(Sysno::fork, NONE)));
    let child = pid(child as i32);
    kernel.script_repoll(&[RepollWait::Yielded]);

    // The root's callback waits; while it is switched out, the child's
    // callback runs to completion on the same host.
    let mut arrived = None;
    let mut others = || {
        if arrived.is_none() {
            arrived = Some(kernel.syscall(&host, child, request(Sysno::gettid, NONE)));
        }
    };
    let waited = kernel.syscall_with_others(&host, root, request(Sysno::getpid, NONE), &mut others);
    assert_eq!(complete(waited), 1000);
    let arrived = arrived.expect("the child's callback ran during the wait");
    assert_eq!(complete(arrived), 1001);
    assert_eq!(
        kernel.repoll_waits(),
        [1000],
        "only the root's callback waited"
    );
    assert_eq!(kernel.violations(), []);
}

#[test]
fn waiting_tool_fails_closed_when_no_other_task_runs() {
    let host = host::<Rendezvous>();
    let kernel = FakeKernel::new();
    let root = kernel.spawn_root(&host, BASE);
    kernel.script_repoll(&[RepollWait::Yielded; 3]);

    // Three waits end with nobody having run the `gettid` callback, and the
    // fourth is refused.
    let result = kernel.syscall(&host, root, request(Sysno::getpid, NONE));
    assert!(
        matches!(result, Err(NarfFatal::ToolSuspended)),
        "{result:?}"
    );
    assert_eq!(kernel.repoll_waits(), [1000; 4]);
    assert_eq!(kernel.natives(), [], "nothing ran natively");
}

std::thread_local! {
    /// Polls and drops of `Forever` futures on this test's thread.
    static FOREVER: Cell<(u64, u64)> = const { Cell::new((0, 0)) };
}

/// Pending on every poll; counts its polls and its drops.
struct Forever;

impl Future for Forever {
    type Output = Result<i64, Error>;

    fn poll(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Self::Output> {
        let (polls, drops) = FOREVER.get();
        FOREVER.set((polls + 1, drops));
        Poll::Pending
    }
}

impl Drop for Forever {
    fn drop(&mut self) {
        let (polls, drops) = FOREVER.get();
        FOREVER.set((polls, drops + 1));
    }
}

/// Waits for something that never happens.
#[derive(Default)]
struct NeverReady;

#[async_trait]
impl Tool for NeverReady {
    type GlobalState = ();
    type ThreadState = ();

    async fn handle_syscall_event<G: Guest<Self>>(
        &self,
        _guest: &mut G,
        _syscall: Syscall,
    ) -> Result<i64, Error> {
        Forever.await
    }
}

#[test]
fn tool_killed_while_waiting_is_dropped_and_its_task_torn_down() {
    let host = host::<NeverReady>();
    let kernel = FakeKernel::new();
    let root = kernel.spawn_root(&host, BASE);
    kernel.script_repoll(&[RepollWait::Yielded, RepollWait::Killed]);

    // SIGKILL arrives during the second wait: the kernel owns the task, so
    // the future is dropped without another poll.
    context_managed(kernel.syscall(&host, root, request(Sysno::getpid, NONE)));
    assert_eq!(
        FOREVER.get(),
        (2, 1),
        "polled before each wait, dropped once"
    );
    assert_eq!(kernel.repoll_waits(), [1000, 1000]);
    assert_eq!(kernel.natives(), [], "nothing ran natively");
    assert_eq!(kernel.violations(), []);
    // The callback checked the task back in, so the kill tears it down.
    assert_teardowns(&kernel, &[(1000, exited(true))]);
    assert_eq!((host.live_threads(), host.live_processes()), (0, 0));
}

/// Daemonizes, then waits once before it forwards the syscall.
#[derive(Default)]
struct DaemonizeThenYield;

#[async_trait]
impl Tool for DaemonizeThenYield {
    type GlobalState = ();
    type ThreadState = ();

    async fn handle_syscall_event<G: Guest<Self>>(
        &self,
        guest: &mut G,
        syscall: Syscall,
    ) -> Result<i64, Error> {
        guest.daemonize().await;
        YieldOnce(false).await;
        guest.tail_inject(syscall).await
    }
}

#[test]
fn failed_tool_is_not_polled_again() {
    let host = host::<DaemonizeThenYield>();
    let kernel = FakeKernel::new();
    let root = kernel.spawn_root(&host, BASE);
    kernel.script_repoll(&[RepollWait::Yielded]);
    kernel.refuse_daemonize(Errno::EPERM);

    // The refusal is recorded while the future is still pending: the
    // callback ends with it, without a wait and without another poll.
    let result = kernel.syscall(&host, root, request(Sysno::getpid, NONE));
    assert!(
        matches!(result, Err(NarfFatal::DaemonizeRefused(Errno::EPERM))),
        "{result:?}"
    );
    assert_eq!(kernel.repoll_waits(), []);
    assert_eq!(kernel.natives(), [], "the tail inject never ran");
    assert!(!kernel.daemon(root));
}

/// Injects the intercepted syscall, then waits once before it returns.
#[derive(Default)]
struct InjectThenYield;

#[async_trait]
impl Tool for InjectThenYield {
    type GlobalState = ();
    type ThreadState = ();

    async fn handle_syscall_event<G: Guest<Self>>(
        &self,
        guest: &mut G,
        syscall: Syscall,
    ) -> Result<i64, Error> {
        let value = guest.inject(syscall).await?;
        YieldOnce(false).await;
        Ok(value + 1)
    }
}

#[test]
fn resumed_tool_is_polled_again_after_the_kernel_yields() {
    let host = host::<InjectThenYield>();
    let kernel = FakeKernel::new();
    let root = kernel.spawn_root(&host, BASE);
    kernel.script_repoll(&[RepollWait::Yielded]);
    let read = request(Sysno::read, [PIPE_FD, (BASE + 0x100) as u64, 4, 0, 0, 0]);

    // A parked inject suspends the Tool until the re-execution; that is not a
    // wait for another task.
    context_managed(kernel.syscall(&host, root, read));
    assert_eq!(kernel.repoll_waits(), []);

    kernel.push_pipe(b"xy");
    // The re-execution resumes the future with the read's value, and the
    // future waits once more before it finishes.
    assert_eq!(complete(kernel.reexecute(&host, root)), 3);
    assert_eq!(kernel.repoll_waits(), [1000]);
    assert_eq!(kernel.peek(root, BASE + 0x100, 2), b"xy");
    assert_eq!(kernel.natives().len(), 2, "the read and its re-execution");
    assert_eq!(kernel.violations(), []);
}

/// Tail-injects `getpid` at thread start, when there is no syscall to answer.
#[derive(Default)]
struct StartTail;

#[async_trait]
impl Tool for StartTail {
    type GlobalState = ();
    type ThreadState = ();

    async fn handle_thread_start<G: Guest<Self>>(&self, guest: &mut G) -> Result<(), Error> {
        guest
            .tail_inject(Syscall::from_raw(
                Sysno::getpid,
                SyscallArgs::new(0, 0, 0, 0, 0, 0),
            ))
            .await
    }
}

#[test]
fn lifecycle_tail_that_returns_fails_closed() {
    let host = host::<StartTail>();
    let kernel = FakeKernel::new();
    let root = kernel.spawn_root(&host, BASE);
    let result = kernel.thread_start(&host, root);
    assert!(
        matches!(result, Err(NarfFatal::TailInjectOutsideSyscall)),
        "{result:?}"
    );
    assert_eq!(
        kernel.violations(),
        [],
        "the tail was injected, not the original"
    );
}

/// Subscribes only to `write`.
#[derive(Default)]
struct WriteCounter;

#[async_trait]
impl Tool for WriteCounter {
    type GlobalState = Steps;
    type ThreadState = ();

    fn subscriptions(_cfg: &()) -> Subscription {
        let mut subscription = Subscription::none();
        subscription.syscall(Sysno::write);
        subscription
    }

    async fn handle_syscall_event<G: Guest<Self>>(
        &self,
        guest: &mut G,
        syscall: Syscall,
    ) -> Result<i64, Error> {
        guest.send_rpc(1).await;
        guest.tail_inject(syscall).await
    }
}

#[test]
fn unsubscribed_syscalls_run_natively_and_still_register_children() {
    let host = host::<WriteCounter>();
    let kernel = FakeKernel::new();
    let root = kernel.spawn_root(&host, BASE);

    assert_eq!(
        complete(kernel.syscall(&host, root, request(Sysno::getpid, NONE))),
        1000
    );
    let child = pid(complete(kernel.syscall(&host, root, request(Sysno::fork, NONE))) as i32);
    assert_eq!(host.live_processes(), 2);
    let write = request(Sysno::write, [1, BASE as u64, 2, 0, 0, 0]);
    assert_eq!(complete(kernel.syscall(&host, child, write)), 2);
    assert_eq!(
        host.global().0.load(Ordering::SeqCst),
        1,
        "only the write reached the Tool"
    );
    assert_eq!(kernel.natives().len(), 3);
}

// ----------------------------------------------------------------------------
// Refused configurations

#[derive(Default)]
struct WantsCpuid;

#[async_trait]
impl Tool for WantsCpuid {
    type GlobalState = ();
    type ThreadState = ();

    fn subscriptions(_cfg: &()) -> Subscription {
        let mut subscription = Subscription::all_syscalls();
        subscription.cpuid();
        subscription
    }
}

#[derive(Default)]
struct WantsHostThreads;

#[async_trait]
impl Tool for WantsHostThreads {
    type GlobalState = ();
    type ThreadState = ();

    fn thread_ownership(_cfg: &()) -> ThreadOwnership {
        ThreadOwnership::Host
    }
}

#[derive(Default)]
struct WantsSignalDequeues;

#[async_trait]
impl Tool for WantsSignalDequeues {
    type GlobalState = ();
    type ThreadState = ();

    fn observe_signal_dequeues(_cfg: &()) -> bool {
        true
    }
}

#[test]
fn event_sources_narf_cannot_deliver_are_refused() {
    assert!(matches!(
        FakeHost::<WantsCpuid>::new(()),
        Err(NarfFatal::UnsupportedSubscription)
    ));
    assert!(matches!(
        FakeHost::<WantsHostThreads>::new(()),
        Err(NarfFatal::UnsupportedThreadOwnership)
    ));
    assert!(matches!(
        FakeHost::<WantsSignalDequeues>::new(()),
        Err(NarfFatal::UnsupportedSignalDequeues)
    ));
}

// ----------------------------------------------------------------------------
// Exit statuses: the thread's own for on_exit_thread, the process's for
// on_exit_process

/// Records the status each exit hook receives.
#[derive(Default)]
struct ExitStatuses;

std::thread_local! {
    /// On this test's thread, in order: `(true, tid, status)` for each
    /// `on_exit_thread`, `(false, pid, status)` for each `on_exit_process`.
    static EXIT_STATUSES: core::cell::RefCell<Vec<(bool, i32, ExitStatus)>> =
        const { core::cell::RefCell::new(Vec::new()) };
}

#[async_trait]
impl Tool for ExitStatuses {
    type GlobalState = ();
    type ThreadState = ();

    async fn on_exit_thread<G: reverie::GlobalRPC<Self::GlobalState>>(
        &self,
        tid: Pid,
        _global_state: &G,
        _thread_state: Self::ThreadState,
        exit_status: ExitStatus,
    ) -> Result<(), Error> {
        EXIT_STATUSES.with_borrow_mut(|seen| seen.push((true, tid.as_raw(), exit_status)));
        Ok(())
    }

    async fn on_exit_process<G: reverie::GlobalRPC<Self::GlobalState>>(
        self,
        pid: Pid,
        _global_state: &G,
        exit_status: ExitStatus,
    ) -> Result<(), Error> {
        EXIT_STATUSES.with_borrow_mut(|seen| seen.push((false, pid.as_raw(), exit_status)));
        Ok(())
    }
}

/// A thread's `exit(5)` reaches its `on_exit_thread` as 5, and the leader's
/// later `exit_group(7)` reaches the leader's `on_exit_thread` and
/// `on_exit_process` as 7, as reverie-ptrace reports them. When the leader
/// instead calls `exit(3)` first and a thread calls `exit(5)` last, the
/// leader's hook gets 3 and the process's gets the last thread's 5, the
/// status `wait4` reports (Linux's `synchronize_group_exit`). And a last
/// thread whose own `exit(5)` lost the race to a sibling's `exit_group(7)`
/// gets 5 while its process gets the group's 7.
#[test]
fn exit_hooks_get_the_thread_and_process_statuses() {
    let exit = |code: u64| request(Sysno::exit, [code, 0, 0, 0, 0, 0]);
    let exit_group = |code: u64| request(Sysno::exit_group, [code, 0, 0, 0, 0, 0]);
    let spawn_thread = |kernel: &FakeKernel, host: &FakeHost<ExitStatuses>, root| {
        pid(complete(kernel.syscall(
            host,
            root,
            request(Sysno::clone, [CLONE_THREAD, 0, 0, 0, 0, 0]),
        )) as i32)
    };

    let tools = host::<ExitStatuses>();
    let kernel = FakeKernel::new();
    let root = kernel.spawn_root(&tools, BASE);
    let thread = spawn_thread(&kernel, &tools, root);
    context_managed(kernel.syscall(&tools, thread, exit(5)));
    context_managed(kernel.syscall(&tools, root, exit_group(7)));
    assert_eq!(
        EXIT_STATUSES.take(),
        [
            (true, 1001, ExitStatus::Exited(5)),
            (true, 1000, ExitStatus::Exited(7)),
            (false, 1000, ExitStatus::Exited(7)),
        ]
    );

    let tools = host::<ExitStatuses>();
    let kernel = FakeKernel::new();
    let root = kernel.spawn_root(&tools, BASE);
    let thread = spawn_thread(&kernel, &tools, root);
    context_managed(kernel.syscall(&tools, root, exit(3)));
    context_managed(kernel.syscall(&tools, thread, exit(5)));
    assert_eq!(
        EXIT_STATUSES.take(),
        [
            (true, 1000, ExitStatus::Exited(3)),
            (true, 1001, ExitStatus::Exited(5)),
            (false, 1000, ExitStatus::Exited(5)),
        ]
    );
    assert_eq!(kernel.violations(), []);

    let tools = host::<ExitStatuses>();
    let kernel = FakeKernel::new();
    let root = kernel.spawn_root(&tools, BASE);
    assert_eq!(
        tools
            .task_exited(root, ExitStatus::Exited(5), ExitStatus::Exited(7))
            .ok(),
        Some(TaskExit {
            process_exited: true
        })
    );
    assert_eq!(
        EXIT_STATUSES.take(),
        [
            (true, 1000, ExitStatus::Exited(5)),
            (false, 1000, ExitStatus::Exited(7)),
        ]
    );
}
