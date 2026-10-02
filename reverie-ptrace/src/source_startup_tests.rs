/* Copyright (c) Meta Platforms, Inc. and affiliates. All rights reserved.
 * Licensed under the BSD-style license in the root LICENSE file. */

//! Readable TRACEEXIT during startup restoration. The original ordinary owner
//! resumes EXIT and consumes final. Already-final ESRCH has a separate cfg test.
use std::cell::RefCell;
use std::collections::BTreeSet;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;
use std::time::Duration;
use std::time::Instant;

use reverie::Errno;
use reverie::GlobalRPC;
use reverie::GlobalTool;
use reverie::Guest;
use reverie::InjectedSyscallEvent;
use reverie::Pid;
use reverie::Subscription;
use reverie::Tool;
use reverie::syscalls::Syscall;
use reverie::syscalls::SyscallArgs;
use reverie::syscalls::SyscallInfo;
use safeptrace::ExitStatus;
use safeptrace::Signal;

use super::*;
use crate::regs::RegAccess;
use crate::task::TaskTimer;
use crate::task::TracedTask;

#[derive(Clone, Debug, PartialEq, Eq)]
struct RegisterImage {
    ip: u64,
    words: Vec<u64>,
}
fn register_image(regs: &libc::user_regs_struct) -> RegisterImage {
    #[cfg(target_arch = "x86_64")]
    let words = vec![
        regs.r15,
        regs.r14,
        regs.r13,
        regs.r12,
        regs.rbp,
        regs.rbx,
        regs.r11,
        regs.r10,
        regs.r9,
        regs.r8,
        regs.rax,
        regs.rcx,
        regs.rdx,
        regs.rsi,
        regs.rdi,
        regs.orig_rax,
        regs.rip,
        regs.cs,
        regs.eflags,
        regs.rsp,
        regs.ss,
        regs.fs_base,
        regs.gs_base,
        regs.ds,
        regs.es,
        regs.fs,
        regs.gs,
    ];
    #[cfg(target_arch = "aarch64")]
    let words = {
        let mut words = regs.regs.to_vec();
        words.extend([regs.sp, regs.pc, regs.pstate]);
        words
    };
    RegisterImage {
        ip: regs.ip(),
        words,
    }
}
// Independent field projection for this actual None/false restoration call.
// Do not call the producer's restored_context_registers as the test oracle.
fn expected_registers(
    before: &libc::user_regs_struct,
    context: &libc::user_regs_struct,
) -> RegisterImage {
    let mut expected = *before;
    #[cfg(target_arch = "x86_64")]
    {
        expected.rip = context.rip;
        expected.orig_rax = context.orig_rax;
        expected.rdi = context.rdi;
        expected.rsi = context.rsi;
        expected.rdx = context.rdx;
        expected.r10 = context.r10;
        expected.r8 = context.r8;
        expected.r9 = context.r9;
        expected.rcx = context.rcx;
        expected.r11 = context.r11;
    }
    #[cfg(target_arch = "aarch64")]
    {
        expected.pc = context.pc;
        expected.regs[..6].copy_from_slice(&context.regs[..6]);
        expected.regs[8] = context.regs[8];
    }
    register_image(&expected)
}
type BeforeRestore = Result<(RegisterImage, RegisterImage), String>;
fn restored_registers_match(
    before: &[BeforeRestore],
    after: &[Result<RegisterImage, String>],
) -> bool {
    matches!((before, after), ([Ok((old, expected))], [Ok(actual)])
        if old.ip != expected.ip && actual == expected)
}

#[derive(Clone, Copy, Debug)]
struct Population {
    failed: bool,
    tasks: usize,
    operations: usize,
    next_task: u64,
}
fn population(history: &CohortHistory) -> Population {
    let h = history.0.lock().unwrap();
    Population {
        failed: h.failed,
        tasks: h.tasks.len(),
        operations: h.tasks.values().map(|t| t.operations.len()).sum(),
        next_task: h.next_task,
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum OwnerPath {
    Startup,
    Ordinary,
}
struct Child {
    tid: Pid,
    cleanup: Arc<TerminalCleanup>,
    member: Option<Member>,
    before: Population,
    restore_died: bool,
    restore_result: Option<String>,
    registers_before: Vec<BeforeRestore>,
    registers_after: Vec<Result<RegisterImage, String>>,
    adopted: Vec<(Option<u64>, bool)>,
    resumes: Vec<(OwnerPath, Result<(), String>)>,
    terminal_branch: Option<Population>,
    callbacks_done: Option<(OwnerPath, usize, usize)>,
    body_done: bool,
    body_status: Option<ExitStatus>,
    notification_sent: bool,
}
#[derive(Default)]
struct Log {
    history: Option<Arc<CohortHistory>>,
    root_cleanup: Option<TerminalCleanup>,
    counts: Option<(Arc<AtomicUsize>, Arc<AtomicUsize>)>,
    children: Vec<Child>,
    created: BTreeSet<Pid>,
    returned: BTreeMap<Pid, i64>,
    restored_parents: BTreeMap<Pid, i64>,
    guest_results: Vec<i64>,
    started: BTreeSet<Pid>,
    terminal: BTreeMap<Pid, ExitStatus>,
    consumed: BTreeSet<Pid>,
    joined: Vec<Population>,
    failures: Vec<String>,
    stop_requested: Arc<tokio::sync::Notify>,
}
impl Log {
    // Latch before waking the original wait's supervisor. No live-owner unwind.
    fn require(&mut self, condition: bool, message: &str) -> bool {
        if !condition {
            self.failures.push(message.to_owned());
            self.stop_requested.notify_one();
        }
        condition
    }
}
thread_local! { static ACTIVE: RefCell<Option<Arc<Mutex<Log>>>> = const { RefCell::new(None) }; }
fn active() -> Option<Arc<Mutex<Log>>> {
    ACTIVE.with(|s| s.borrow().clone())
}
struct Reset;
impl Drop for Reset {
    fn drop(&mut self) {
        ACTIVE.with(|s| {
            s.borrow_mut().take();
        });
    }
}
macro_rules! require_hook {
    ($log:expr, $condition:expr, $message:expr) => {
        if !$log.require($condition, $message) {
            return;
        }
    };
}
pub(crate) fn parent_restored(parent: &Stopped, child: Pid, raw: Option<i64>) {
    let Some(log) = active() else { return };
    let mut log = log.lock().unwrap();
    let Some(raw) = raw else {
        log.require(false, "authenticated parent return");
        return;
    };
    let matches = log.returned.get(&child) == Some(&raw);
    log.require(matches, "authenticated parent returned value");
    let unique = log.restored_parents.insert(child, raw).is_none();
    log.require(unique, "unique authenticated parent restoration");
    log.root_cleanup
        .get_or_insert_with(|| parent.terminal_cleanup());
}
pub(crate) async fn before_restore<L: Tool + 'static>(task: &TracedTask<L>, stopped: &Stopped) {
    let Some(log) = active() else { return };
    require_hook!(
        log.lock().unwrap(),
        matches!(task.timer, TaskTimer::Live(_)),
        "genuine live preparation"
    );
    // This hook is only in the actual Some(saved_context)/SIGSTOP restore arm.
    let history = Arc::clone(&task.global_state.fatal_session.source_cohort);
    let cleanup = Arc::new(stopped.terminal_cleanup());
    let identity = match cleanup.task_identity() {
        Ok(identity) => identity,
        Err(error) => {
            log.lock()
                .unwrap()
                .require(false, &format!("retained generation: {error:?}"));
            return;
        }
    };
    {
        let h = history.0.lock().unwrap();
        let mut log = log.lock().unwrap();
        if log.children.is_empty() {
            require_hook!(
                log,
                !h.failed,
                "first real child must enter eligible history"
            );
            require_hook!(
                log,
                h.tasks.len() == 2,
                "first real child population is two"
            );
        }
        if !h.failed {
            let Some(member) = task.cohort.as_ref() else {
                log.require(false, "real enrolled Command child");
                return;
            };
            let Some(record) = h.tasks.get(&member.index) else {
                log.require(false, "real enrolled Command record");
                return;
            };
            require_hook!(
                log,
                record.identity.same_generation(&identity),
                "Constructed generation"
            );
            require_hook!(
                log,
                matches!(
                    record.origin,
                    Origin::Child {
                        custody: ChildCustody::Constructed,
                        ..
                    }
                ),
                "Constructed cohort membership"
            );
            require_hook!(
                log,
                record.life == Life::Stopped,
                "Constructed stopped life"
            );
            require_hook!(
                log,
                record
                    .stop
                    .as_ref()
                    .is_some_and(|s| s.validate_current().is_ok()),
                "current real control stop"
            );
            let monotonic = log
                .children
                .iter()
                .filter_map(|c| c.member.as_ref())
                .all(|old| old.index < member.index);
            require_hook!(log, monotonic, "nonreused logical identity");
        } else {
            require_hook!(log, h.tasks.is_empty(), "failed history stays empty");
            require_hook!(
                log,
                task.cohort.is_none(),
                "failed history cannot enroll a later generation"
            );
        }
        let distinct = log
            .children
            .iter()
            .all(|old| !old.cleanup.same_generation(&cleanup));
        require_hook!(log, distinct, "distinct retained child generation");
        log.history.get_or_insert(Arc::clone(&history));
        log.counts
            .get_or_insert((Arc::clone(&task.ntasks), Arc::clone(&task.ndaemons)));
    }
    let before = population(&history);
    log.lock().unwrap().children.push(Child {
        tid: stopped.pid(),
        cleanup: Arc::clone(&cleanup),
        member: task.cohort.clone(),
        before,
        restore_died: false,
        restore_result: None,
        registers_before: Vec::new(),
        registers_after: Vec::new(),
        adopted: Vec::new(),
        resumes: Vec::new(),
        terminal_branch: None,
        callbacks_done: None,
        body_done: false,
        body_status: None,
        notification_sent: false,
    });
    let started = Instant::now();
    let deadline = started + Duration::from_secs(1);
    loop {
        if Instant::now() >= deadline {
            log.lock().unwrap().require(
                false,
                &format!(
                    "authenticated parent completion bound elapsed={:?}",
                    started.elapsed()
                ),
            );
            return;
        }
        if log
            .lock()
            .unwrap()
            .restored_parents
            .contains_key(&stopped.pid())
        {
            break;
        }
        tokio::task::yield_now().await;
    }
    require_hook!(
        log.lock().unwrap(),
        cleanup.observed_terminal().is_none(),
        "no final before generation-bound signal"
    );
    if let Err(error) = cleanup.terminate_bound_task() {
        log.lock().unwrap().require(
            false,
            &format!("signal the original retained pidfd: {error:?}"),
        );
        return;
    }
    let started = Instant::now();
    let deadline = started + Duration::from_secs(1);
    loop {
        if Instant::now() >= deadline {
            log.lock().unwrap().require(
                false,
                &format!(
                    "actual EXIT publication bound elapsed={:?}",
                    started.elapsed()
                ),
            );
            return;
        }
        if cleanup.exit_stop_observed() {
            break;
        }
        tokio::task::yield_now().await;
    }
    require_hook!(
        log.lock().unwrap(),
        cleanup.observed_terminal().is_none(),
        "EXIT is not final consumption"
    );
    // No exit_event(), resume(), wait(), or invented restoration result here.
}
// These probes read the same retained task; they never write registers, claim
// EXIT, resume, join, or change the production restoration result. Their
// substantive assertions run only after the unchanged original cleanup.
pub(crate) fn capture_before_restore(stopped: &Stopped, context: &libc::user_regs_struct) {
    let Some(log) = active() else { return };
    let cleanup = stopped.terminal_cleanup();
    let before = stopped
        .getregs()
        .map(|regs| (register_image(&regs), expected_registers(&regs, context)))
        .map_err(|error| format!("{error:?}"));
    let mut log = log.lock().unwrap();
    if let Some(child) = log
        .children
        .iter_mut()
        .find(|c| c.cleanup.same_generation(&cleanup))
    {
        child.registers_before.push(before);
    } else {
        log.require(false, "register preimage belongs to enrolled generation");
    }
}
pub(crate) fn capture_adoption<L: Tool + 'static>(task: &TracedTask<L>, stopped: &Stopped) {
    let Some(log) = active() else { return };
    let cleanup = stopped.terminal_cleanup();
    let identity = cleanup.task_identity();
    let history = task
        .global_state
        .fatal_session
        .source_cohort
        .0
        .lock()
        .unwrap();
    let index = task.cohort.as_ref().map(|member| member.index);
    let adopted = index
        .and_then(|index| history.tasks.get(&index))
        .is_some_and(|record| {
            identity
                .as_ref()
                .is_ok_and(|identity| record.identity.same_generation(identity))
                && matches!(
                    record.origin,
                    Origin::Child {
                        custody: ChildCustody::Restored,
                        ..
                    }
                )
        });
    drop(history);
    let mut log = log.lock().unwrap();
    if let Some(child) = log
        .children
        .iter_mut()
        .find(|c| c.cleanup.same_generation(&cleanup))
    {
        child.adopted.push((index, adopted));
    } else {
        log.require(false, "adoption belongs to enrolled generation");
    }
}
pub(crate) fn restore_result(stopped: &Stopped, result: &Result<(), TraceError>) {
    let Some(log) = active() else { return };
    let cleanup = stopped.terminal_cleanup();
    // A fresh GETREGSET, not the producer's returned Ok or its local register copy.
    let after = stopped
        .getregs()
        .map(|regs| register_image(&regs))
        .map_err(|error| format!("{error:?}"));
    let mut log = log.lock().unwrap();
    let Some(child) = log
        .children
        .iter_mut()
        .find(|c| c.cleanup.same_generation(&cleanup))
    else {
        log.require(false, "restore result belongs to enrolled generation");
        return;
    };
    child.restore_died = matches!(
        result,
        Err(TraceError::Died(_) | TraceError::Errno(Errno::ESRCH))
    );
    child.registers_after.push(after);
    let unique = child
        .restore_result
        .replace(format!("{result:?}"))
        .is_none();
    log.require(unique, "one actual restoration result");
    // A readable EXIT may restore successfully; final death is a distinct path.
    println!(
        "startup actual restore tid={} result={result:?}",
        stopped.pid()
    );
}
pub(crate) fn exit_resumed(
    cleanup: &TerminalCleanup,
    path: OwnerPath,
    result: &Result<(), String>,
) {
    let Some(log) = active() else { return };
    let mut log = log.lock().unwrap();
    if let Some(child) = log
        .children
        .iter_mut()
        .find(|c| c.cleanup.same_generation(cleanup))
    {
        child.resumes.push((path, result.clone()));
    }
}
pub(crate) fn terminal_branch<L: Tool + 'static>(task: &TracedTask<L>, status: ExitStatus) {
    let Some(log) = active() else { return };
    let snapshot = population(&task.global_state.fatal_session.source_cohort);
    let mut log = log.lock().unwrap();
    let Some(child) = log.children.iter_mut().find(|c| c.tid == task.tid()) else {
        log.require(false, "startup terminal belongs to enrolled child");
        return;
    };
    let actual = matches!(child.cleanup.observed_terminal(), Some(Ok(actual)) if actual == status);
    let unique = child.terminal_branch.replace(snapshot).is_none();
    log.require(actual, "startup terminal matches retained actual final");
    log.require(
        status == ExitStatus::Signaled(Signal::SIGKILL, false),
        "actual startup SIGKILL",
    );
    log.require(unique, "one startup terminal boundary");
}
pub(crate) fn callbacks_done(
    cleanup: &TerminalCleanup,
    path: OwnerPath,
    counts: &(Arc<AtomicUsize>, Arc<AtomicUsize>),
) {
    let Some(log) = active() else { return };
    let mut log = log.lock().unwrap();
    if let Some(child) = log
        .children
        .iter_mut()
        .find(|c| c.cleanup.same_generation(cleanup))
    {
        let unique = child
            .callbacks_done
            .replace((
                path,
                counts.0.load(Ordering::SeqCst),
                counts.1.load(Ordering::SeqCst),
            ))
            .is_none();
        log.require(unique, "one original callback/counter completion");
    }
}
pub(crate) fn body_completed(
    cleanup: &TerminalCleanup,
    status: Option<ExitStatus>,
    notification_sent: bool,
) {
    let Some(log) = active() else { return };
    let mut log = log.lock().unwrap();
    if let Some(child) = log
        .children
        .iter_mut()
        .find(|c| c.cleanup.same_generation(cleanup))
    {
        let unique = !child.body_done;
        child.body_done = true;
        child.body_status = status;
        child.notification_sent = notification_sent;
        log.require(unique, "one original child body completion");
        log.require(
            status == Some(ExitStatus::Signaled(Signal::SIGKILL, false)),
            "original child body returned actual SIGKILL",
        );
        log.require(
            notification_sent,
            "original child completion notification delivered",
        );
    }
}
// Each dimension is independent. No notifier read/wait holds the fixture mutex.
// WORKER_DONE is publication, not an OS-thread join.
#[derive(Debug)]
struct Physical {
    actual_final: Option<ExitStatus>,
    final_error: Option<String>,
    terminal_callback: Option<ExitStatus>,
    consuming_callback: bool,
    counters: Option<(OwnerPath, usize, usize)>,
    body_done: bool,
    body_status: Option<ExitStatus>,
    notification_sent: bool,
    worker_done: bool,
}
impl Physical {
    fn complete(&self) -> bool {
        self.actual_final == Some(ExitStatus::Signaled(Signal::SIGKILL, false))
            && self.final_error.is_none()
            && self.terminal_callback == self.actual_final
            && self.consuming_callback
            && self.counters.is_some()
            && self.body_done
            && self.body_status == self.actual_final
            && self.notification_sent
            && self.worker_done
    }
}
fn physical(log: &Arc<Mutex<Log>>, cleanup: &TerminalCleanup) -> Option<Physical> {
    let observed = cleanup.observed_exit_status();
    let worker_done = cleanup.wait(Duration::ZERO);
    let log = log.lock().unwrap();
    let child = log
        .children
        .iter()
        .find(|c| c.cleanup.same_generation(cleanup))?;
    Some(Physical {
        actual_final: observed.as_ref().ok().copied().flatten(),
        final_error: observed.err().map(|error| format!("{error:?}")),
        terminal_callback: log.terminal.get(&child.tid).copied(),
        consuming_callback: log.consumed.contains(&child.tid),
        counters: child.callbacks_done,
        body_done: child.body_done,
        body_status: child.body_status,
        notification_sent: child.notification_sent,
        worker_done,
    })
}

#[derive(Default)]
struct Global;
#[reverie::global_tool]
impl GlobalTool for Global {
    type Config = ();
    type Request = ();
    type Response = ();
    async fn receive_rpc(&self, _: Pid, _: ()) {}
}
#[derive(Default)]
struct Observer;
#[reverie::tool]
impl Tool for Observer {
    type GlobalState = Global;
    type ThreadState = bool;
    fn subscriptions(_: &()) -> Subscription {
        [Sysno::clone, Sysno::write].into_iter().collect()
    }
    fn observe_injected_syscalls(_: &()) -> bool {
        true
    }
    async fn handle_thread_start<G: Guest<Self>>(
        &self,
        guest: &mut G,
    ) -> Result<(), reverie::Error> {
        if let Some(log) = active() {
            let mut log = log.lock().unwrap();
            let unique = log.started.insert(guest.tid());
            log.require(unique, "unique thread-start callback");
        }
        Ok(())
    }
    fn on_injected_syscall_observed(
        &self,
        _: Pid,
        _: &Global,
        _: &mut bool,
        _: Sysno,
        _: SyscallArgs,
        event: InjectedSyscallEvent,
    ) {
        let Some(log) = active() else { return };
        let mut log = log.lock().unwrap();
        match event {
            InjectedSyscallEvent::ChildCreated(child) => {
                let unique = log.created.insert(child);
                log.require(unique, "unique authentic child creation");
            }
            InjectedSyscallEvent::ChildSyscallReturned { child, raw } => {
                let created = log.created.contains(&child);
                log.require(created, "parent return follows authentic child creation");
                log.require(
                    raw == i64::from(child.as_raw()),
                    "actual positive parent result",
                );
                let unique = log.returned.insert(child, raw).is_none();
                log.require(unique, "one authentic parent return");
            }
            _ => {}
        }
    }
    async fn handle_syscall_event<G: Guest<Self>>(
        &self,
        guest: &mut G,
        call: Syscall,
    ) -> Result<i64, reverie::Error> {
        let (nr, args) = call.into_parts();
        if nr == Sysno::write && args.arg0 == 631 {
            use reverie::syscalls::Addr;
            use reverie::syscalls::MemoryAccess;
            let log = active().ok_or_else(|| anyhow::anyhow!("missing fixture observer"))?;
            if args.arg2 != b"startup-joined".len() {
                log.lock()
                    .unwrap()
                    .require(false, "exact startup-joined marker length");
                return Err(anyhow::anyhow!("invalid startup marker length").into());
            }
            let mut bytes = vec![0; args.arg2];
            let address = Addr::from_raw(args.arg1)
                .ok_or_else(|| anyhow::anyhow!("invalid startup marker address"))?;
            guest.memory().read_exact(address, &mut bytes)?;
            if !log
                .lock()
                .unwrap()
                .require(bytes == b"startup-joined", "exact startup-joined marker")
            {
                return Err(anyhow::anyhow!("invalid startup marker").into());
            }
            let cleanup = log
                .lock()
                .unwrap()
                .children
                .last()
                .map(|c| Arc::clone(&c.cleanup));
            let Some(cleanup) = cleanup else {
                log.lock()
                    .unwrap()
                    .require(false, "marker has actual prepared child");
                return Err(anyhow::anyhow!("missing prepared child").into());
            };
            let started = Instant::now();
            let deadline = started + Duration::from_secs(1);
            loop {
                let dimensions = physical(&log, &cleanup);
                if Instant::now() >= deadline {
                    log.lock().unwrap().require(false, &format!(
                        "original child retirement acknowledgment bound elapsed={:?} dimensions={dimensions:?}", started.elapsed()));
                    return Err(
                        anyhow::anyhow!("original child retirement acknowledgment bound").into(),
                    );
                }
                if dimensions.as_ref().is_some_and(Physical::complete) {
                    println!("startup physical dimensions={dimensions:?}");
                    break;
                }
                tokio::task::yield_now().await;
            }
            let history = log.lock().unwrap().history.clone();
            let Some(history) = history else {
                log.lock()
                    .unwrap()
                    .require(false, "marker retains real history");
                return Err(anyhow::anyhow!("missing real history").into());
            };
            let snapshot = population(&history);
            println!("startup original cleanup population={snapshot:?}");
            log.lock().unwrap().joined.push(snapshot);
            return Ok(args.arg2 as i64);
        }
        if nr == Sysno::clone {
            let pid = guest.inject(reverie::syscalls::Getpid::default()).await?;
            if let Some(log) = active() {
                log.lock().unwrap().require(
                    pid == i64::from(guest.pid().as_raw()),
                    "actual injected getpid",
                );
            }
        }
        let result = guest.inject(call).await?;
        if nr == Sysno::clone
            && let Some(log) = active()
        {
            log.lock().unwrap().guest_results.push(result);
        }
        Ok(result)
    }
    fn on_backend_thread_terminal(
        &self,
        tid: Pid,
        _: &Global,
        state: &mut bool,
        status: ExitStatus,
    ) {
        if let Some(log) = active() {
            let mut log = log.lock().unwrap();
            log.require(!*state, "terminal-once callback state");
            let unique = log.terminal.insert(tid, status).is_none();
            log.require(unique, "one backend terminal callback");
        }
        *state = true;
    }
    async fn on_exit_thread<G: GlobalRPC<Global>>(
        &self,
        tid: Pid,
        _: &G,
        state: bool,
        _: ExitStatus,
    ) -> Result<(), reverie::Error> {
        if let Some(log) = active() {
            let mut log = log.lock().unwrap();
            log.require(state, "consuming callback follows terminal callback");
            let unique = log.consumed.insert(tid);
            log.require(unique, "one consuming thread callback");
        }
        Ok(())
    }
}

#[tokio::test(flavor = "current_thread")]
async fn command_startup_traceexit_restores_before_ordinary_cleanup() {
    let fixture = std::path::PathBuf::from(crate::testing::fixture_path("COHORT_BRIDGE_FIXTURE"));
    assert!(fixture.is_absolute());
    let log = Arc::new(Mutex::new(Log::default()));
    ACTIVE.with(|slot| assert!(slot.replace(Some(Arc::clone(&log))).is_none()));
    let _reset = Reset;
    let mut command = reverie::process::Command::new(fixture);
    command.arg("startup-deaths");
    let tracer = crate::TracerBuilder::<Observer>::new(command)
        .spawn()
        .await
        .unwrap();
    let stop_requested = Arc::clone(&log.lock().unwrap().stop_requested);
    let termination = tracer.termination_handle();
    let completion = tracer.wait();
    tokio::pin!(completion);
    let outcome = tokio::select! {
        result = &mut completion => result,
        () = stop_requested.notified() => {
            let cause = log.lock().unwrap().failures.join("; ");
            let requested = termination.as_ref().is_some_and(|handle| {
                handle.terminate(anyhow::anyhow!("startup fixture retained failure: {cause}").into())
            });
            println!("STARTUP_FAILURE_TERMINATION requested={requested} cause={cause}");
            // Same original consuming wait and its unchanged bounded cleanup.
            completion.await
        }
    };
    println!(
        "STARTUP_ORIGINAL_WAIT result={:?} retained_failures={:?}",
        outcome.as_ref().map(|(status, _)| status),
        log.lock().unwrap().failures
    );
    let cleanups: Vec<_> = log
        .lock()
        .unwrap()
        .children
        .iter()
        .map(|c| Arc::clone(&c.cleanup))
        .collect();
    // Report dimensions even on an error, never manufacture a cleanup receipt.
    for cleanup in &cleanups {
        println!("STARTUP_FINAL_DIMENSIONS {:?}", physical(&log, cleanup));
    }
    let (status, _) = outcome.expect("original Command cleanup must complete");
    assert_eq!(status, reverie::process::ExitStatus::Exited(0));
    // Original one-second notifier bounds, outside the fixture mutex.
    for cleanup in &cleanups {
        assert!(cleanup.wait(Duration::from_secs(1)));
        assert!(physical(&log, cleanup).is_some_and(|p| p.complete()));
    }
    let root = log.lock().unwrap().root_cleanup.take().unwrap();
    assert!(root.wait(Duration::from_secs(1)));
    assert_eq!(
        root.observed_exit_status().unwrap(),
        Some(ExitStatus::Exited(0))
    );
    let log = log.lock().unwrap();
    assert_eq!(log.children.len(), 8);
    assert_eq!(log.created.len(), 8);
    assert_eq!(log.returned.len(), 8);
    assert_eq!(log.restored_parents.len(), 8);
    assert_eq!(log.guest_results.len(), 8);
    assert_eq!(log.joined.len(), 8);
    assert_eq!(log.started.len(), 1, "no child thread-start callback");
    assert_eq!(log.terminal.len(), 9);
    assert_eq!(log.consumed.len(), 9);
    let (tasks, daemons) = log.counts.as_ref().unwrap();
    assert_eq!(tasks.load(Ordering::SeqCst), 0);
    assert_eq!(daemons.load(Ordering::SeqCst), 0);
    for (child, raw) in log.children.iter().zip(&log.guest_results) {
        assert_eq!(*raw, i64::from(child.tid.as_raw()));
        assert_eq!(log.restored_parents[&child.tid], *raw);
        assert_eq!(
            child.cleanup.observed_exit_status().unwrap(),
            Some(ExitStatus::Signaled(Signal::SIGKILL, false))
        );
        assert_eq!(
            log.terminal[&child.tid],
            ExitStatus::Signaled(Signal::SIGKILL, false)
        );
        assert!(child.callbacks_done.is_some());
        assert!(child.body_done && child.notification_sent);
        assert_eq!(
            child.body_status,
            Some(ExitStatus::Signaled(Signal::SIGKILL, false))
        );
        assert!(log.consumed.contains(&child.tid));
        assert!(!log.started.contains(&child.tid));
    }
    assert!(
        log.failures.is_empty(),
        "retained live-hook failures: {:?}",
        log.failures
    );
    println!(
        "STARTUP_PHYSICAL_CLEANUP_DIAGNOSTIC children=8 parent_returns=8 terminal_callbacks=9 consumed_callbacks=9 counters=0/0"
    );
    // The old test demanded ESRCH at an inspectable EXIT, despite explicitly
    // excluding final death above. Its failed source/log remain historical.
    // Every physical dimension and original Tracer::wait still precedes these
    // stricter, separate readable-EXIT restoration and owner checks.
    for (index, child) in log.children.iter().enumerate() {
        let [Ok((before, expected))] = child.registers_before.as_slice() else {
            panic!("exact one successful actual pre-restoration read required");
        };
        println!(
            "STARTUP_REGISTER_READBACK child={index} before={before:?} expected={expected:?} after={:?}",
            child.registers_after
        );
        assert_ne!(
            before.ip, expected.ip,
            "actual changed-IP restoration premise"
        );
        assert!(
            restored_registers_match(&child.registers_before, &child.registers_after),
            "actual register restoration must match independent readback after original cleanup"
        );
        assert_eq!(child.restore_result.as_deref(), Some("Ok(())"));
        assert!(!child.restore_died);
        assert_eq!(
            child.adopted,
            vec![(Some((index + 1) as u64), true)],
            "original restored child must be adopted exactly once after original cleanup"
        );
        assert_eq!(child.resumes, vec![(OwnerPath::Ordinary, Ok(()))]);
        assert_eq!(child.callbacks_done, Some((OwnerPath::Ordinary, 1, 0)));
        assert!(child.terminal_branch.is_none(), "no Startup owner adoption");
        assert_eq!(child.member.as_ref().unwrap().index, (index + 1) as u64);
        assert!(!child.before.failed);
        assert_eq!(child.before.tasks, 2);
        assert_eq!(child.before.next_task, (index + 2) as u64);
        let joined = log.joined[index];
        assert!(
            !joined.failed,
            "successful ordinary restoration must not cause full-history failure"
        );
        assert_eq!(joined.tasks, 1, "each original child metadata retired");
        assert_eq!(joined.operations, 0, "no completed native debt retained");
        assert_eq!(joined.next_task, (index + 2) as u64);
    }
    let final_population = population(log.history.as_ref().unwrap());
    assert!(!final_population.failed);
    assert_eq!(final_population.tasks, 0);
    assert_eq!(final_population.operations, 0);
    assert_eq!(final_population.next_task, 9);
    let history = log.history.as_ref().unwrap().0.lock().unwrap();
    assert!(
        history.source_closed && !history.read_open(),
        "ordinary process-child source reads remain refused"
    );
    drop(history);
    let stale = log.children[0].member.as_ref().unwrap().clone();
    drop(log);
    assert!(
        stale
            .native(Sysno::getpid, SyscallArgs::new(0, 0, 0, 0, 0, 0))
            .is_none()
    );
    println!(
        "TRACEEXIT_RESTORATION_AND_ORDINARY_CLEANUP_VERIFIED children=8 parent_returns=8 terminal_callbacks=9 consumed_callbacks=9 counters=0/0 source_closed=true"
    );
}

#[test]
fn restoration_reader_rejects_skipped_missing_duplicate_and_changed_registers() {
    let old = RegisterImage {
        ip: 10,
        words: vec![10, 20, 30],
    };
    let expected = RegisterImage {
        ip: 11,
        words: vec![11, 20, 30],
    };
    let before = vec![Ok((old.clone(), expected.clone()))];
    let after = vec![Ok(expected.clone())];
    assert!(restored_registers_match(&before, &after));
    assert!(
        !restored_registers_match(&before, &[Ok(old)]),
        "skipped physical restoration"
    );
    assert!(!restored_registers_match(&[], &after), "missing preimage");
    assert!(!restored_registers_match(&before, &[]), "missing readback");
    assert!(
        !restored_registers_match(&[before[0].clone(), before[0].clone()], &after),
        "duplicate preimage"
    );
    assert!(
        !restored_registers_match(&before, &[after[0].clone(), after[0].clone()]),
        "duplicate readback"
    );
    assert!(!restored_registers_match(
        &[Err("read fault".into())],
        &after
    ));
    assert!(!restored_registers_match(
        &before,
        &[Err("read fault".into())]
    ));
    assert!(
        !restored_registers_match(&[Ok((expected.clone(), expected.clone()))], &after),
        "unchanged-IP premise cannot prove a write"
    );
    for index in 0..expected.words.len() {
        let mut wrong = expected.clone();
        wrong.words[index] ^= 1;
        assert!(
            !restored_registers_match(&before, &[Ok(wrong)]),
            "register word {index}"
        );
    }
    assert!(restored_registers_match(&before, &after));
}

// Retain 1310's additive Ordinary contract beside the corrected restoration test.
#[path = "source_startup_contract_tests.rs"]
mod contracts;
