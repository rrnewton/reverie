/* Copyright (c) Meta Platforms, Inc. and affiliates. All rights reserved.
 * Licensed under the BSD-style license in the root LICENSE file. */

//! Native Command fixtures, COMPILE ONLY under 631. Hooks inspect actual
//! backend transitions; none construct a task identity, stop or wait result.
use std::cell::RefCell;
use std::path::PathBuf;

use reverie::Error;
use reverie::GlobalTool;
use reverie::Guest;
use reverie::Subscription;
use reverie::Tool;
use reverie::syscalls::Syscall;
use reverie::syscalls::SyscallInfo;

use super::*;

#[derive(Default)]
struct Observations {
    history: Option<Arc<CohortHistory>>,
    before_birth: Option<u64>,
    births: usize,
    execs: usize,
    exits: usize,
    native_stops: usize,
    child_stops: usize,
    prebirth_stops: Vec<(u64, ControlStop)>,
    identities: BTreeMap<u64, TaskIdentity>,
    origins: BTreeMap<u64, (u64, u64)>,
    restored: std::collections::BTreeSet<u64>,
    parents: std::collections::BTreeSet<(u64, u64)>,
    terminal_pending: std::collections::BTreeSet<u64>,
    terminal_done: std::collections::BTreeSet<u64>,
    reclaimed: std::collections::BTreeSet<u64>,
    native_returns: usize,
    native_results: BTreeMap<(u64, i64), usize>,
    measuring: bool,
    peak_tasks: usize,
    peak_operations: usize,
    saw_failed: bool,
    mode: String,
    abandoned: bool,
    members: Vec<Member>,
}
thread_local! {
    static ACTIVE: RefCell<Option<Arc<Mutex<Observations>>>> = const { RefCell::new(None) };
}
fn active() -> Option<Arc<Mutex<Observations>>> {
    ACTIVE.with(|slot| slot.borrow().clone())
}
struct Reset;
pub(super) fn population(history: &History) {
    if let Some(log) = active() {
        let mut log = log.lock().unwrap();
        log.peak_tasks = log.peak_tasks.max(history.tasks.len());
        log.peak_operations = log
            .peak_operations
            .max(history.tasks.values().map(|t| t.operations.len()).sum());
        log.saw_failed |= history.failed;
    }
}
pub(super) fn native_returned(syscall: Sysno, raw: i64) {
    if let Some(log) = active() {
        let mut log = log.lock().unwrap();
        if log.measuring {
            log.native_returns += 1;
            *log.native_results.entry((syscall as u64, raw)).or_default() += 1;
        }
    }
}
pub(super) fn abandon_completion() -> bool {
    active().is_some_and(|log| {
        let mut log = log.lock().unwrap();
        if log.mode == "abandoned" && !log.abandoned {
            log.abandoned = true;
            true
        } else {
            false
        }
    })
}
pub(super) fn retain_native_mutant() -> bool {
    active().is_some_and(|log| log.lock().unwrap().mode == "operations-mutant")
}
pub(super) fn retain_task_mutant() -> bool {
    active().is_some_and(|log| log.lock().unwrap().mode == "lifetimes-mutant")
}
pub(super) fn restored(member: &Member) {
    if let Some(log) = active() {
        let h = member.history.0.lock().unwrap();
        assert!(matches!(
            h.tasks[&member.index].origin,
            Origin::Child {
                custody: ChildCustody::Restored,
                ..
            }
        ));
        assert!(log.lock().unwrap().restored.insert(member.index));
    }
}
pub(super) fn parent_returned(member: &Member, operation: u64) {
    if let Some(log) = active() {
        let h = member.history.0.lock().unwrap();
        assert!(!h.tasks[&member.index].operations.contains_key(&operation));
        assert!(
            log.lock()
                .unwrap()
                .parents
                .insert((member.index, operation))
        );
    }
}
pub(super) fn terminal_completed(member: &Member) {
    if let Some(log) = active() {
        let mut log = log.lock().unwrap();
        assert!(
            log.terminal_done.contains(&member.index),
            "metadata retirement follows the actual final owner"
        );
        assert!(log.reclaimed.insert(member.index));
    }
}
pub(crate) fn original_terminal_pending(terminal: &TerminalCleanup) {
    if let Some(log) = active() {
        let identity = terminal.task_identity().unwrap();
        let mut log = log.lock().unwrap();
        if let Some(id) = log
            .identities
            .iter()
            .find(|(_, i)| i.same_generation(&identity))
            .map(|(id, _)| *id)
        {
            log.terminal_pending.insert(id);
        }
    }
}
pub(crate) fn original_terminal_completed(terminal: &TerminalCleanup) {
    if let Some(log) = active() {
        let identity = terminal.task_identity().unwrap();
        let mut log = log.lock().unwrap();
        if let Some(id) = log
            .identities
            .iter()
            .find(|(_, i)| i.same_generation(&identity))
            .map(|(id, _)| *id)
        {
            assert!(log.terminal_pending.contains(&id));
            assert!(log.terminal_done.insert(id));
        }
    }
}
impl Drop for Reset {
    fn drop(&mut self) {
        ACTIVE.with(|slot| {
            slot.borrow_mut().take();
        });
    }
}

pub(super) fn initial(member: &Member, stopped: &Stopped) {
    let Some(active) = active() else {
        return;
    };
    let history = member.history.0.lock().unwrap();
    assert_eq!(history.tasks.len(), 1);
    assert!(
        history.tasks[&0]
            .stop
            .as_ref()
            .unwrap()
            .validate_current()
            .is_ok()
    );
    active.lock().unwrap().history = Some(Arc::clone(&member.history));
    active.lock().unwrap().members.push(member.clone());
    active.lock().unwrap().identities.insert(
        member.index,
        stopped.terminal_cleanup().task_identity().unwrap(),
    );
}

pub(super) fn before_resume(member: &Member, effect: Effect) {
    let Some(active) = active() else {
        return;
    };
    {
        let history = member.history.0.lock().unwrap();
        assert!(!history.failed);
        assert!(
            !history.tasks[&member.index].operations.is_empty(),
            "actual native/control entry must retain unresolved debt"
        );
    }
    if effect == Effect::Birth {
        active.lock().unwrap().before_birth = Some(member.history.0.lock().unwrap().revision);
    }
}

pub(super) fn birth(member: &Member, cleanup: &TerminalCleanup) {
    let Some(active) = active() else {
        return;
    };
    let history = member.history.0.lock().unwrap();
    let mut log = active.lock().unwrap();
    assert!(
        !history.failed,
        "causal prebirth registration cannot hide a failed history"
    );
    assert!(history.revision > log.before_birth.expect("native entry must precede birth"));
    let child = history.tasks.last_key_value().unwrap().1;
    assert!(
        !child
            .identity
            .same_generation(&history.tasks[&member.index].identity)
    );
    assert!(
        child.stop.is_none(),
        "birth event is not a child stopped receipt"
    );
    assert!(matches!(child.life, Life::Initializing));
    let Origin::Child {
        parent,
        operation,
        custody,
    } = child.origin
    else {
        panic!("child lineage missing");
    };
    assert_eq!(parent, member.index);
    assert!(custody == ChildCustody::AwaitingOwner);
    assert_eq!(
        history.tasks[&parent].operations[&operation].effect,
        Effect::Birth
    );
    assert!(
        history.tasks[&parent].stop.is_none(),
        "prebirth stop cannot survive resume"
    );
    let stops: Vec<_> = log
        .prebirth_stops
        .iter()
        .filter(|(index, _)| *index == parent)
        .collect();
    assert!(
        !stops.is_empty(),
        "retain an actual, previously current prebirth receipt"
    );
    assert!(
        stops
            .iter()
            .all(|(_, stop)| stop.validate_current().is_err()),
        "old genuine prebirth stops must stay invalid"
    );
    log.births += 1;
    let id = *history.tasks.last_key_value().unwrap().0;
    assert!(
        log.identities
            .insert(id, cleanup.task_identity().unwrap())
            .is_none()
    );
    assert!(log.origins.insert(id, (parent, operation)).is_none());
}

pub(super) fn prepared(member: &Member, prepared: &PreparedNewborn) {
    let Some(active) = active() else {
        return;
    };
    if matches!(prepared, PreparedNewborn::Live { .. }) {
        let history = member.history.0.lock().unwrap();
        let task = &history.tasks[&member.index];
        assert!(
            task.stop
                .as_ref()
                .expect("original initial wait issued a child stop")
                .validate_current()
                .is_ok()
        );
        let Origin::Child {
            parent,
            operation,
            custody,
        } = task.origin
        else {
            panic!("child origin missing");
        };
        assert!(custody == ChildCustody::Constructed);
        assert_eq!(
            history.tasks[&parent].operations[&operation].effect,
            Effect::Birth,
            "preparing a child cannot complete its parent's native operation"
        );
        active.lock().unwrap().child_stops += 1;
    }
}

pub(super) fn exec(member: &Member) {
    let Some(active) = active() else {
        return;
    };
    let history = member.history.0.lock().unwrap();
    assert!(
        history.tasks[&member.index]
            .operations
            .values()
            .any(|op| op.effect == Effect::Exec)
    );
    assert!(history.tasks[&member.index].stop.is_none());
    active.lock().unwrap().execs += 1;
}

pub(super) fn wait(member: &Member, result: &Result<Wait, TraceError>) {
    let Some(active) = active() else {
        return;
    };
    if let Ok(Wait::Stopped(stopped, event)) = result {
        let pending_effect = effect(stopped);
        let mut history = member.history.0.lock().unwrap();
        let task = &history.tasks[&member.index];
        if let Some(operation) = task.invocation {
            assert!(
                task.operations.contains_key(&operation),
                "a generic consumed stop cannot complete its native invocation"
            );
        }
        if matches!(event, Event::Exit) {
            assert!(task.life == Life::Exiting);
            assert!(
                task.operations
                    .values()
                    .any(|op| op.effect == Effect::Terminal)
            );
            active.lock().unwrap().exits += 1;
        }
        if let Some(stop) = &task.stop {
            assert!(stop.validate_current().is_ok());
            active.lock().unwrap().native_stops += 1;
        }
        if pending_effect == Effect::Birth {
            let stop = history
                .tasks
                .get_mut(&member.index)
                .unwrap()
                .stop
                .take()
                .expect("real native birth entry receipt");
            active
                .lock()
                .unwrap()
                .prebirth_stops
                .push((member.index, stop));
        }
    }
}

pub(super) fn terminal_pending(member: &Member) {
    let Some(active) = active() else {
        return;
    };
    let history = member.history.0.lock().unwrap();
    let task = &history.tasks[&member.index];
    assert!(task.stop.is_none());
    assert!(task.life == Life::Exiting);
    assert!(
        task.operations
            .values()
            .any(|op| op.effect == Effect::Terminal && op.outcome == Outcome::Waiting)
    );
    active.lock().unwrap().exits += 1;
}

#[derive(Default)]
struct Global {
    markers: Mutex<Vec<Vec<u8>>>,
}
#[reverie::global_tool]
impl GlobalTool for Global {
    type Config = String;
    type Request = ();
    type Response = ();
    async fn receive_rpc(&self, _: reverie::Pid, _: ()) {}
}
#[derive(Default)]
struct Observer;
#[reverie::tool]
impl Tool for Observer {
    type GlobalState = Global;
    type ThreadState = ();
    fn subscriptions(mode: &String) -> Subscription {
        let mut calls = vec![
            Sysno::write,
            Sysno::clone,
            Sysno::clone3,
            #[cfg(target_arch = "x86_64")]
            Sysno::fork,
            #[cfg(target_arch = "x86_64")]
            Sysno::vfork,
            Sysno::execve,
            Sysno::execveat,
            Sysno::exit,
            Sysno::exit_group,
        ];
        if mode.starts_with("operations") || mode == "abandoned" {
            calls.extend([Sysno::getpid, Sysno::close, Sysno::read]);
        }
        calls.into_iter().collect()
    }
    fn observe_injected_syscalls(mode: &String) -> bool {
        mode.starts_with("lifetimes")
    }
    async fn handle_syscall_event<G: Guest<Self>>(
        &self,
        guest: &mut G,
        call: Syscall,
    ) -> Result<i64, Error> {
        let (nr, args) = call.into_parts();
        if nr == Sysno::write && args.arg0 == 631 {
            use reverie::syscalls::Addr;
            use reverie::syscalls::MemoryAccess;
            let mut bytes = vec![0; args.arg2];
            guest
                .memory()
                .read_exact(Addr::from_raw(args.arg1).unwrap(), &mut bytes)?;
            if bytes == b"operations-start" {
                let log = active().unwrap();
                let mut log = log.lock().unwrap();
                assert!(!log.measuring);
                log.measuring = true;
            }
            if bytes == b"operations-done" {
                let log = active().unwrap();
                let mut log = log.lock().unwrap();
                assert!(log.measuring);
                log.measuring = false;
            }
            if bytes == b"joined-raw" {
                let completed = guest
                    .local_global_state()
                    .unwrap()
                    .markers
                    .lock()
                    .unwrap()
                    .len()
                    + 1;
                loop {
                    let done = active()
                        .unwrap()
                        .lock()
                        .unwrap()
                        .terminal_done
                        .iter()
                        .filter(|&&id| id != 0)
                        .count();
                    if done >= completed {
                        break;
                    }
                    tokio::task::yield_now().await;
                }
            }
            guest
                .local_global_state()
                .unwrap()
                .markers
                .lock()
                .unwrap()
                .push(bytes);
            return Ok(args.arg2 as i64);
        }
        Ok(guest.inject(call).await?)
    }
}

async fn run(mode: &str) {
    let fixture = PathBuf::from(
        std::env::var_os("COHORT_BRIDGE_FIXTURE")
            .expect("requires separately admitted, hash-bound fixture binary"),
    );
    assert!(fixture.is_absolute());
    let observations = Arc::new(Mutex::new(Observations::default()));
    observations.lock().unwrap().mode = mode.to_owned();
    ACTIVE.with(|slot| {
        assert!(slot.replace(Some(Arc::clone(&observations))).is_none());
    });
    let _reset = Reset;
    let mut command = reverie::process::Command::new(fixture);
    command.arg(match mode {
        "operations-mutant" | "abandoned" => "operations",
        "lifetimes-mutant" => "lifetimes",
        _ => mode,
    });
    let tracer = crate::TracerBuilder::<Observer>::new(command)
        .config(mode.to_owned())
        .spawn()
        .await
        .unwrap();
    // No internal timeout creates a receipt. Future native admission must own
    // external bounds and inspect ordinary wait_completion custody in full.
    // wait() routes CleanupPending through the original private quarantine;
    // an error fails this assertion and cannot count as completed custody.
    let (status, global) = tracer
        .wait()
        .await
        .expect("original Command cleanup must complete");
    assert_eq!(status, reverie::process::ExitStatus::Exited(0));
    let log = observations.lock().unwrap();
    let history = log.history.as_ref().unwrap().0.lock().unwrap();
    assert!(log.native_stops > 0);
    assert!(
        !log.terminal_pending.is_empty(),
        "real consuming terminal owner was observed while incomplete"
    );
    println!(
        "cohort retained peak tasks={} operations={} final tasks={} operations={} native returns={} registered={} final owners={}",
        log.peak_tasks,
        log.peak_operations,
        history.tasks.len(),
        history
            .tasks
            .values()
            .map(|t| t.operations.len())
            .sum::<usize>(),
        log.native_returns,
        log.identities.len(),
        log.terminal_done.len()
    );
    match mode {
        "pthread" => {
            assert_eq!(log.births, 1);
            assert_eq!(log.identities.len(), 2);
            assert_eq!(log.child_stops, 1);
            assert!(
                log.restored.contains(&1),
                "actual child restoration was observed"
            );
            let (parent, operation) = log.origins[&1];
            assert!(
                log.parents.contains(&(parent, operation)),
                "only the actual parent return and restored continuation settle this birth"
            );
            assert_eq!(
                *global.markers.lock().unwrap(),
                [b"root".to_vec(), b"child".to_vec(), b"joined".to_vec()]
            );
        }
        "exec" => {
            assert_eq!(log.execs, 1);
            assert!(
                history.failed && log.saw_failed,
                "unresolved exec transfer permanently refuses even after compaction"
            );
            assert_eq!(
                *global.markers.lock().unwrap(),
                [b"before-exec".to_vec(), b"terminal".to_vec()]
            );
        }
        "terminal" => {
            assert_eq!(*global.markers.lock().unwrap(), [b"terminal".to_vec()]);
            assert!(
                history.tasks.values().all(|task| task
                    .operations
                    .values()
                    .all(|op| op.effect != Effect::Terminal)),
                "actual original terminal completion must settle terminal debt"
            );
            assert!(log.reclaimed.contains(&0));
        }
        "operations" | "operations-mutant" => {
            assert_eq!(
                *global.markers.lock().unwrap(),
                [b"operations-start".to_vec(), b"operations-done".to_vec()]
            );
            assert_eq!(
                log.native_returns, 517,
                "actual typed completions must precede the population oracle"
            );
            assert_eq!(
                log.native_results
                    .iter()
                    .filter(|((nr, raw), _)| *nr == Sysno::getpid as u64 && *raw > 0)
                    .map(|(_, n)| n)
                    .sum::<usize>(),
                256
            );
            assert_eq!(
                log.native_results[&(Sysno::close as u64, -i64::from(libc::EBADF))],
                256
            );
            assert_eq!(log.native_results[&(Sysno::close as u64, 0)], 2);
            assert_eq!(log.native_results[&(Sysno::read as u64, 5)], 1);
            assert_eq!(log.native_results[&(Sysno::write as u64, 5)], 1);
            assert_eq!(log.native_results[&(Sysno::pipe2 as u64, 0)], 1);
            assert!(log.terminal_done.contains(&0));
            assert!(
                log.peak_operations <= 4,
                "retained operation population grew after genuine cleanup"
            );
            assert!(
                !history.failed && !log.saw_failed,
                "compaction must not make this positive pass"
            );
            assert!(history.tasks.is_empty());
        }
        "lifetimes" | "lifetimes-mutant" => {
            assert_eq!(log.births, 64);
            assert_eq!(log.child_stops, 64);
            assert_eq!(log.restored.len(), 64);
            assert_eq!(log.parents.len(), 64);
            assert_eq!(log.terminal_done.len(), 65);
            assert_eq!(
                *global.markers.lock().unwrap(),
                vec![b"joined-raw".to_vec(); 64]
            );
            assert!(
                log.peak_tasks <= 2,
                "retained task population grew after genuine cleanup"
            );
            assert!(log.peak_operations <= 6);
            assert!(
                !history.failed && !log.saw_failed,
                "compaction must not make this positive pass"
            );
            assert!(history.tasks.is_empty());
        }
        "abandoned" => {
            assert!(log.abandoned);
            assert!(history.failed && log.saw_failed);
            assert!(history.tasks.is_empty());
            assert_eq!(
                *global.markers.lock().unwrap(),
                [b"operations-start".to_vec(), b"operations-done".to_vec()]
            );
        }
        _ => unreachable!(),
    }
    // Keep causal final-owner evidence even when production storage is reclaimed.
    assert_eq!(
        log.identities
            .keys()
            .copied()
            .collect::<std::collections::BTreeSet<_>>(),
        log.terminal_done
    );
    let stale = log.members[0].clone();
    drop(history);
    drop(log);
    assert!(
        stale
            .native(
                Sysno::getpid,
                reverie::syscalls::SyscallArgs::new(0, 0, 0, 0, 0, 0)
            )
            .is_none(),
        "a reclaimed logical generation must not be admitted again"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn command_pthread_registers_before_child_execution() {
    run("pthread").await;
}
#[tokio::test(flavor = "current_thread")]
async fn command_exec_keeps_unresolved_transfer_debt() {
    run("exec").await;
}
#[tokio::test(flavor = "current_thread")]
async fn command_terminal_retains_debt_until_original_owner_completes() {
    run("terminal").await;
}

#[tokio::test(flavor = "current_thread")]
async fn command_many_native_returns_are_bounded() {
    run("operations").await;
}
#[tokio::test(flavor = "current_thread")]
async fn command_sequential_thread_generations_are_bounded() {
    run("lifetimes").await;
}
#[tokio::test(flavor = "current_thread")]
async fn command_abandoned_return_observer_stays_failed() {
    run("abandoned").await;
}
#[tokio::test(flavor = "current_thread")]
#[should_panic(expected = "retained operation population grew after genuine cleanup")]
async fn missing_native_retirement_mutant_fails_after_cleanup() {
    run("operations-mutant").await;
}
#[tokio::test(flavor = "current_thread")]
#[should_panic(expected = "retained task population grew after genuine cleanup")]
async fn missing_task_retirement_mutant_fails_after_cleanup() {
    run("lifetimes-mutant").await;
}
