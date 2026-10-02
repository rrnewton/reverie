/* Copyright (c) Meta Platforms, Inc. and affiliates. All rights reserved.
 * Licensed under the BSD-style license in the root LICENSE file. */

//! Separate already-final control: original notifier consumes final without TRACEEXIT.
//! The old startup-death test remains unchanged and unqualified.
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
use crate::task::TaskTimer;
use crate::task::TracedTask;

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
    at_restore: Option<Population>,
    restore_died: bool,
    restore_result: Option<String>,
    resumes: Vec<(OwnerPath, Result<(), String>)>,
    terminal_branch: Option<Population>,
    callbacks_done: Option<(OwnerPath, usize, usize)>,
    body_done: bool,
    body_status: Option<ExitStatus>,
    notification_sent: bool,
    options_set: bool,
    signal_sent: bool,
    already_final: bool,
    exit_attempts: usize,
    reaped_by_parent: bool,
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
    parent_latched: BTreeSet<Pid>,
    parent_released: BTreeSet<Pid>,
    sigchld_default: bool,
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
    {
        let mut state = log.lock().unwrap();
        let disposition_observed = state.sigchld_default;
        require_hook!(
            state,
            disposition_observed,
            "actual SIGCHLD default/no-SA_NOCLDWAIT query"
        );
    }
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
        at_restore: None,
        restore_died: false,
        restore_result: None,
        resumes: Vec::new(),
        terminal_branch: None,
        callbacks_done: None,
        body_done: false,
        body_status: None,
        notification_sent: false,
        options_set: false,
        signal_sent: false,
        already_final: false,
        exit_attempts: 0,
        reaped_by_parent: false,
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
        {
            let state = log.lock().unwrap();
            if state.restored_parents.contains_key(&stopped.pid())
                && state.parent_latched.contains(&stopped.pid())
                && !state.parent_released.contains(&stopped.pid())
            {
                break;
            }
        }
        tokio::task::yield_now().await;
    }
    // Full existing normal profile, with only this genuinely enrolled child's
    // TRACEEXIT bit removed. SETOPTIONS uses the original Stopped capability.
    use nix::sys::ptrace::Options;
    let normal = Options::PTRACE_O_TRACEEXEC
        | Options::PTRACE_O_EXITKILL
        | Options::PTRACE_O_TRACECLONE
        | Options::PTRACE_O_TRACEFORK
        | Options::PTRACE_O_TRACEVFORK
        | Options::PTRACE_O_TRACEVFORKDONE
        | Options::PTRACE_O_TRACEEXIT
        | Options::PTRACE_O_TRACESECCOMP
        | Options::PTRACE_O_TRACESYSGOOD;
    let selected = normal & !Options::PTRACE_O_TRACEEXIT;
    if let Err(error) = stopped.setoptions(selected) {
        log.lock()
            .unwrap()
            .require(false, &format!("original child SETOPTIONS: {error:?}"));
        return;
    }
    {
        let mut state = log.lock().unwrap();
        let child = state
            .children
            .iter_mut()
            .find(|c| c.cleanup.same_generation(&cleanup));
        if let Some(child) = child {
            child.options_set = true;
        } else {
            state.require(false, "SETOPTIONS retained generation");
            return;
        }
    }
    println!(
        "FINAL_OPTIONS normal={:#x} selected={:#x} result=Ok",
        normal.bits(),
        selected.bits()
    );
    require_hook!(
        log.lock().unwrap(),
        cleanup.observed_terminal().is_none(),
        "no final before generation-bound signal"
    );
    cleanup.arm_final_test_registers();
    if let Err(error) = cleanup.terminate_bound_task() {
        log.lock().unwrap().require(
            false,
            &format!("signal the original retained pidfd: {error:?}"),
        );
        return;
    }
    {
        let mut state = log.lock().unwrap();
        if let Some(child) = state
            .children
            .iter_mut()
            .find(|c| c.cleanup.same_generation(&cleanup))
        {
            child.signal_sent = true;
        }
    }
    let started = Instant::now();
    let deadline = started + Duration::from_secs(1);
    loop {
        if Instant::now() >= deadline {
            log.lock().unwrap().require(
                false,
                &format!(
                    "actual original final publication bound elapsed={:?}",
                    started.elapsed()
                ),
            );
            return;
        }
        if matches!(
            cleanup.observed_terminal(),
            Some(Ok(ExitStatus::Signaled(Signal::SIGKILL, false)))
        ) && cleanup.wait(Duration::ZERO)
        {
            break;
        }
        tokio::task::yield_now().await;
    }
    let (publications, claims, _) = cleanup.final_test_activity();
    require_hook!(
        log.lock().unwrap(),
        publications == 0 && claims == 0,
        "already-final control has no EXIT publication or claim"
    );
    // The real parent is still stopped in its callback: its zombie remains
    // unreaped until the unchanged GETREGSET and original child body complete.
    // No exit_event(), resume(), final consumer, or invented error here.
}
pub(crate) fn restore_result(stopped: &Stopped, result: &Result<(), TraceError>) {
    let Some(log) = active() else { return };
    let cleanup = stopped.terminal_cleanup();
    let history = log.lock().unwrap().history.clone();
    let snapshot = history.as_ref().map(|history| population(history));
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
    child.at_restore = snapshot;
    let registers = cleanup.final_test_registers();
    let raw_esrch = registers.as_deref() == Some(&[(libc::NT_PRSTATUS, Err(Errno::ESRCH))]);
    let unique = child
        .restore_result
        .replace(format!("{result:?}"))
        .is_none();
    log.require(unique, "one actual restoration result");
    log.require(raw_esrch, "unchanged actual GETREGSET returned ESRCH");
    let held = log.parent_latched.contains(&stopped.pid())
        && !log.parent_released.contains(&stopped.pid());
    log.require(
        held,
        "real parent still held before wait4 during restoration",
    );
    println!(
        "FINAL_RAW_GETREGSET tid={} result={registers:?}",
        stopped.pid()
    );
    // Ok remains a premise failure after original ordinary cleanup.
    println!(
        "final actual restore tid={} result={result:?}",
        stopped.pid()
    );
}
pub(crate) fn already_final(cleanup: &TerminalCleanup, status: ExitStatus) {
    let Some(log) = active() else { return };
    let mut state = log.lock().unwrap();
    if let Some(child) = state
        .children
        .iter_mut()
        .find(|c| c.cleanup.same_generation(cleanup))
    {
        let unique = !child.already_final;
        child.already_final = true;
        state.require(unique, "one original already-final helper selection");
        state.require(
            status == ExitStatus::Signaled(Signal::SIGKILL, false),
            "already-final actual SIGKILL",
        );
    } else {
        state.require(false, "already-final enrolled generation");
    }
}
pub(crate) fn exit_attempt(cleanup: &TerminalCleanup) {
    let Some(log) = active() else { return };
    if let Some(child) = log
        .lock()
        .unwrap()
        .children
        .iter_mut()
        .find(|c| c.cleanup.same_generation(cleanup))
    {
        child.exit_attempts += 1;
    }
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
    registry_retired: bool,
    real_parent_reaped: bool,
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
            && self.registry_retired
    }
}
fn physical(log: &Arc<Mutex<Log>>, cleanup: &TerminalCleanup) -> Option<Physical> {
    let observed = cleanup.observed_exit_status();
    let worker_done = cleanup.wait(Duration::ZERO);
    let registry_retired = cleanup.final_test_activity().2;
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
        registry_retired,
        real_parent_reaped: child.reaped_by_parent,
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
            if args.arg2 != b"startup-final-reaped".len()
                && args.arg2 != b"startup-final-sigchld-default".len()
            {
                log.lock()
                    .unwrap()
                    .require(false, "exact startup-joined marker length");
                return Err(anyhow::anyhow!("invalid startup marker length").into());
            }
            let mut bytes = vec![0; args.arg2];
            let address = Addr::from_raw(args.arg1)
                .ok_or_else(|| anyhow::anyhow!("invalid startup marker address"))?;
            guest.memory().read_exact(address, &mut bytes)?;
            if bytes == b"startup-final-sigchld-default" {
                let mut state = log.lock().unwrap();
                let first = !state.sigchld_default && state.children.is_empty();
                state.require(first, "one actual pre-clone SIGCHLD query");
                state.sigchld_default = true;
                return Ok(args.arg2 as i64);
            }
            if !log.lock().unwrap().require(
                bytes == b"startup-final-reaped",
                "exact startup-final-reaped marker",
            ) {
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
            {
                let mut state = log.lock().unwrap();
                if let Some(child) = state
                    .children
                    .iter_mut()
                    .find(|c| c.cleanup.same_generation(&cleanup))
                {
                    child.reaped_by_parent = true;
                }
            }
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
                if dimensions
                    .as_ref()
                    .is_some_and(|p| p.complete() && p.real_parent_reaped)
                {
                    println!("final physical dimensions={dimensions:?}");
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
            println!("final original cleanup population={snapshot:?}");
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
            let child_pid = Pid::from_raw(result as i32);
            {
                let mut state = log.lock().unwrap();
                state.guest_results.push(result);
                let authenticated = state.restored_parents.get(&child_pid) == Some(&result);
                state.require(
                    authenticated,
                    "parent latch after authenticated return/restoration",
                );
                let unique = state.parent_latched.insert(child_pid);
                state.require(unique, "one parent callback latch");
            }
            // Keep the original guest parent stopped, not waiting in wait4.
            // Its child body/callbacks have independent original async owners.
            let started = Instant::now();
            let deadline = started + Duration::from_secs(1);
            loop {
                let cleanup = log
                    .lock()
                    .unwrap()
                    .children
                    .iter()
                    .find(|c| c.tid == child_pid)
                    .map(|c| Arc::clone(&c.cleanup));
                let dimensions = cleanup.as_ref().and_then(|c| physical(&log, c));
                if Instant::now() >= deadline {
                    log.lock().unwrap().require(false, &format!(
                        "held-parent original child completion bound elapsed={:?} dimensions={dimensions:?}",
                        started.elapsed()));
                    return Err(
                        anyhow::anyhow!("held-parent original child completion bound").into(),
                    );
                }
                if dimensions.as_ref().is_some_and(Physical::complete) {
                    break;
                }
                if !log.lock().unwrap().failures.is_empty() {
                    return Err(anyhow::anyhow!("retained already-final fixture failure").into());
                }
                tokio::task::yield_now().await;
            }
            log.lock().unwrap().parent_released.insert(child_pid);
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

// Only called after the original Tracer::wait has returned. Taking a handle
// and checking completion do not join; only the actual join result counts.
async fn join_original_worker(cleanup: &TerminalCleanup) -> bool {
    let Some(handle) = cleanup.take_final_test_worker() else {
        println!("FINAL_WORKER_JOIN missing original handle");
        return false;
    };
    let started = Instant::now();
    let deadline = started + Duration::from_secs(1);
    loop {
        if Instant::now() >= deadline {
            println!("FINAL_WORKER_JOIN bound elapsed={:?}", started.elapsed());
            return false;
        }
        if handle.is_finished() {
            break;
        }
        tokio::task::yield_now().await;
    }
    let joined = handle.join().is_ok();
    println!(
        "FINAL_WORKER_JOIN finished=true joined={joined} registry_retired={}",
        cleanup.final_test_activity().2
    );
    joined && cleanup.final_test_activity().2
}

#[tokio::test(flavor = "current_thread")]
async fn command_startup_final_without_traceexit_permanently_refuses_after_cleanup() {
    let fixture = std::path::PathBuf::from(crate::testing::fixture_path("COHORT_BRIDGE_FIXTURE"));
    assert!(fixture.is_absolute());
    let log = Arc::new(Mutex::new(Log::default()));
    ACTIVE.with(|slot| assert!(slot.replace(Some(Arc::clone(&log))).is_none()));
    let _reset = Reset;
    let mut command = reverie::process::Command::new(fixture);
    command.arg("startup-final");
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
            println!("FINAL_FAILURE_TERMINATION requested={requested} cause={cause}");
            // Same original consuming wait and its unchanged bounded cleanup.
            completion.await
        }
    };
    println!(
        "FINAL_ORIGINAL_WAIT result={:?} retained_failures={:?}",
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
        println!("FINAL_FINAL_DIMENSIONS {:?}", physical(&log, cleanup));
    }
    let root = log.lock().unwrap().root_cleanup.take();
    let mut worker_joins = Vec::new();
    for cleanup in &cleanups {
        worker_joins.push(join_original_worker(cleanup).await);
    }
    if let Some(root) = &root {
        worker_joins.push(join_original_worker(root).await);
    }
    println!("FINAL_ORIGINAL_WORKER_JOINS results={worker_joins:?}");
    let (status, _) = outcome.expect("original Command cleanup must complete");
    assert_eq!(status, reverie::process::ExitStatus::Exited(0));
    // Original one-second notifier bounds, outside the fixture mutex.
    for cleanup in &cleanups {
        assert!(cleanup.wait(Duration::from_secs(1)));
        assert!(physical(&log, cleanup).is_some_and(|p| p.complete()));
    }
    let root = root.expect("actual original parent generation");
    assert!(root.wait(Duration::from_secs(1)));
    assert_eq!(
        root.observed_exit_status().unwrap(),
        Some(ExitStatus::Exited(0))
    );
    assert_eq!(
        worker_joins,
        vec![true; 9],
        "all nine original worker handles joined"
    );
    let log = log.lock().unwrap();
    assert!(log.sigchld_default);
    assert_eq!(log.parent_latched.len(), 8);
    assert_eq!(log.parent_released.len(), 8);
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
        assert!(child.reaped_by_parent);
        assert!(child.options_set && child.signal_sent);
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
        "FINAL_PHYSICAL_CLEANUP_DIAGNOSTIC children=8 parent_returns=8 terminal_callbacks=9 consumed_callbacks=9 counters=0/0"
    );
    // Every physical dimension and original Tracer::wait precedes the premise.
    for child in &log.children {
        assert!(
            child.restore_died,
            "actual restoration did not reach Died/ESRCH"
        );
        assert!(
            child.already_final,
            "original already-final helper selection"
        );
        let (publications, claims, retired) = child.cleanup.final_test_activity();
        assert_eq!(
            (
                publications,
                claims,
                child.exit_attempts,
                child.resumes.len()
            ),
            (0, 0, 0, 0),
            "separate already-final route has zero EXIT publication/claim/attempt/success"
        );
        assert!(retired, "original registry retirement");
        assert_eq!(
            child.cleanup.final_test_registers(),
            Some(vec![(libc::NT_PRSTATUS, Err(Errno::ESRCH))])
        );
        assert_eq!(child.callbacks_done.unwrap().0, OwnerPath::Startup);
        assert!(
            child.terminal_branch.is_some(),
            "actual startup terminal boundary required"
        );
    }
    let first = &log.children[0];
    assert!(!first.before.failed && first.before.tasks == 2);
    let before_closure = first
        .at_restore
        .expect("actual restore readback before closure");
    assert!(
        !before_closure.failed && before_closure.tasks == 2,
        "first genuine restore error precedes any failed-history closure"
    );
    let final_population = population(log.history.as_ref().unwrap());
    println!(
        "FINAL_ORIGINAL_CLEANUP_COMPLETE children=8 parent_returns=8 real_parent_reaps=8 terminal_callbacks=9 consumed_callbacks=9 counters=0/0 worker_joins=9 exit_publications=0 exit_claims=0 exit_resume_attempts=0 exit_resume_successes=0 actual_getregset_esrch=8 final={final_population:?}"
    );
    // All physical/status/callback/cleanup checks precede the causal before oracle.
    assert!(
        log.joined.iter().all(|p| p.failed),
        "already-final history failed to refuse permanently after original cleanup"
    );
    assert!(
        final_population.failed && final_population.tasks == 0 && final_population.operations == 0
    );
    for child in &log.children {
        let p = child.terminal_branch.unwrap();
        assert!(p.failed && p.tasks == 0 && p.operations == 0);
    }
    for child in &log.children[1..] {
        assert!(child.before.failed && child.before.tasks == 0 && child.before.operations == 0);
        assert_eq!(
            child.before.next_task, first.before.next_task,
            "closed history never reenrolls"
        );
    }
    assert!(
        log.joined
            .iter()
            .all(|p| p.tasks == 0 && p.operations == 0 && p.next_task == first.before.next_task)
    );
    assert_eq!(final_population.next_task, first.before.next_task);
    let stale = first.member.as_ref().unwrap().clone();
    drop(log);
    assert!(
        stale
            .native(Sysno::getpid, SyscallArgs::new(0, 0, 0, 0, 0, 0))
            .is_none()
    );
    println!("FINAL_PERMANENT_REFUSAL_VERIFIED");
}
