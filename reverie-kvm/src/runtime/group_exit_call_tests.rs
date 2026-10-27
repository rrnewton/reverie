/* Copyright (c) Meta Platforms, Inc. and affiliates.
 * Licensed under the BSD-style license in the LICENSE file.
 */
//! A call made after the caller's thread group has exited does not run.
//!
//! Linux ends every other thread of an exiting group inside or before its
//! next syscall, and that syscall never returns. These tests pin the
//! static-ELF adapter's version of that rule for the root thread, which is
//! never marked cancelled and sees only the group's committed exit status:
//! once that status is committed, `execute` answers `Cancelled` before the
//! executor runs the call, and `KvmGuest::inject` turns the answer into the
//! thread's cancellation, so a handler that keeps injecting ends instead of
//! spinning. The worker half of the rule, and both halves against a real
//! guest, are covered by `vm::group_exit_injection_loop_tests`.

use super::*;

const MEMORY_SIZE: usize = 16 * 1024 * 1024;

/// A backend with a minimal static ELF installed, and that image's executor.
fn installed() -> (KvmBackend, ElfExecutor) {
    let mut backend = KvmBackend::new(MEMORY_SIZE).expect("group exit call tests require /dev/kvm");
    backend
        .install_static_elf(
            &crate::vm::minimal_test_elf(&[0xf4]),
            "/bin/group-exit-call",
        )
        .unwrap();
    let executor = ElfExecutor::new(backend.static_elf.take().unwrap(), false);
    (backend, executor)
}

/// Runs one call through the adapter's `LinuxProvider::execute`, as
/// `KvmGuest::inject` does, in a lifecycle callback.
fn execute(
    backend: &mut KvmBackend,
    executor: &mut ElfExecutor,
    request: SyscallRequest,
) -> Result<ProviderOutcome> {
    let tid = Pid::from_raw(executor.task_tid());
    let mut memory = backend.memory.clone();
    let mut completed = false;
    let mut adapter = StaticElfSyscallExecutor {
        backend,
        executor,
        memory: memory.clone(),
        process_context: ProcessExecutionContext::Lifecycle,
        last_result: None,
        polled_read_attempt: None,
        process_completed: &mut completed,
        callback_site: None,
        original_syscall: None,
        signal_guard: SignalGuard::Ordinary,
    };
    futures::executor::block_on(LinuxProvider::execute(
        &mut adapter,
        tid,
        request.as_provider(),
        &mut memory,
    ))
}

/// Runs `handler` against a KvmGuest built on the static-ELF adapter in a
/// lifecycle callback and returns the handler's outcome.
fn run_handler<F>(
    backend: &mut KvmBackend,
    executor: &mut ElfExecutor,
    handler: F,
) -> HandlerOutcome<std::result::Result<i64, Errno>>
where
    F: for<'g, 'k> FnOnce(
        &'g mut KvmGuest<'k, crate::StraceTool>,
    )
        -> Pin<Box<dyn Future<Output = std::result::Result<i64, Errno>> + 'g>>,
{
    let pid = Pid::from_raw(executor.task_tid());
    let memory = backend.memory.clone();
    let tool = Arc::new(crate::StraceTool);
    let global = Arc::new(crate::StraceLog::default());
    let mut state = ();
    let subscriptions = Subscription::none();
    let stack = Arc::new(AtomicBool::new(false));
    let signal = Arc::new(Mutex::new(None));
    let starts = Arc::new(Mutex::new(Vec::new()));
    let mut completed = false;
    let top = backend.tool_stack_top();
    let mut adapter = StaticElfSyscallExecutor {
        backend,
        executor,
        memory: memory.clone(),
        process_context: ProcessExecutionContext::Lifecycle,
        last_result: None,
        polled_read_attempt: None,
        process_completed: &mut completed,
        callback_site: None,
        original_syscall: None,
        signal_guard: SignalGuard::Ordinary,
    };
    // No instruction executes in this initialized VM control.
    let registers: libc::user_regs_struct = unsafe { std::mem::zeroed() };
    let mut guest = KvmGuest::new(
        pid,
        pid,
        tool.clone(),
        memory,
        &[],
        &registers,
        &mut state,
        &mut adapter,
        global.as_ref(),
        Some(global.clone()),
        &(),
        &subscriptions,
        signal.clone(),
        starts.clone(),
        top,
        stack,
    );
    futures::executor::block_on(drive_handler(
        handler(&mut guest),
        signal,
        starts,
        std::future::pending(),
    ))
}

fn getpid() -> reverie::syscalls::Syscall {
    SyscallRequest::new(libc::SYS_getpid as u64, [0; 6])
        .into_syscall()
        .unwrap()
}

/// Before the group exit a call runs and returns its result. After it, the
/// same adapter answers `Cancelled` and the executor never sees the call: an
/// `exit` that would otherwise stage a pending exit stages none.
#[test]
fn a_call_after_the_group_exit_is_cancelled_before_it_runs() {
    let (mut backend, mut executor) = installed();
    let pid = i64::from(executor.task_tid());
    let getpid = SyscallRequest::new(libc::SYS_getpid as u64, [0; 6]);
    assert!(
        matches!(
            execute(&mut backend, &mut executor, getpid),
            Ok(ProviderOutcome::Return(result)) if result == pid
        ),
        "a call before the group exit runs"
    );

    backend.request_guest_thread_group_exit(reverie::ExitStatus::Exited(7));

    assert!(
        matches!(
            execute(&mut backend, &mut executor, getpid),
            Ok(ProviderOutcome::Cancelled)
        ),
        "a call after the group exit is cancelled"
    );
    let exit = SyscallRequest::new(libc::SYS_exit as u64, [3, 0, 0, 0, 0, 0]);
    assert!(matches!(
        execute(&mut backend, &mut executor, exit),
        Ok(ProviderOutcome::Cancelled)
    ));
    assert!(
        !executor.has_pending_exit(),
        "the cancelled exit must not reach the executor"
    );
}

/// The handler of the next test: a retry loop of injected calls. It is
/// capped at three calls so that a regression fails the test instead of
/// spinning forever.
fn three_injected_calls<'g>(
    guest: &'g mut KvmGuest<'_, crate::StraceTool>,
) -> Pin<Box<dyn Future<Output = std::result::Result<i64, Errno>> + 'g>> {
    Box::pin(async move {
        let mut calls = 0;
        while calls < 3 {
            guest.inject(getpid()).await?;
            calls += 1;
        }
        Ok(calls)
    })
}

/// A handler that keeps injecting, as a retry loop polling for an event
/// that never comes does, has its calls return while the group lives, and
/// ends in the thread's cancellation at its first call once the group has
/// exited.
#[test]
fn a_handler_that_keeps_injecting_ends_once_the_group_has_exited() {
    let (mut backend, mut executor) = installed();
    let outcome = run_handler(&mut backend, &mut executor, three_injected_calls);
    assert!(
        matches!(outcome, HandlerOutcome::Returned(Ok(3))),
        "while the group lives, the handler's calls return"
    );

    let (mut backend, mut executor) = installed();
    backend.request_guest_thread_group_exit(reverie::ExitStatus::Exited(0));
    let outcome = run_handler(&mut backend, &mut executor, three_injected_calls);
    assert!(
        matches!(outcome, HandlerOutcome::ThreadCancelled),
        "after the group exit, the first call ends the thread"
    );
    assert!(!executor.has_pending_exit());
}
