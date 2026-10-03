/* Copyright (c) Meta Platforms, Inc. and affiliates. All rights reserved.
 * Licensed under the BSD-style license in the root LICENSE file. */

//! Command membership and an inactive, already-stopped source-read boundary.
//! Original owners retain every physical effect. Failed observer metadata can
//! be discarded only irreversibly: this history can never enroll or issue again.
//! Locks cover metadata only, never ptrace, memory, callbacks or an await.
use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::Weak;

use reverie::syscalls::SyscallArgs;
use reverie::syscalls::Sysno;
use safeptrace::ControlStop;
use safeptrace::Event;
use safeptrace::Stopped;
use safeptrace::TaskIdentity;
use safeptrace::TerminalCleanup;
use safeptrace::Wait;
use tokio::sync::Notify;

use super::PreparedNewborn;
use super::TraceError;
use crate::regs::RegAccess;

#[derive(Default)]
pub(super) struct CohortHistory(Mutex<History>, Arc<Notify>);

// Declare BEFORE a metadata guard. Wakers may reenter: Drop must run only
// after that later-declared guard has released the cohort/cell mutexes.
struct ChangeNotice(Vec<Arc<Notify>>);
impl Drop for ChangeNotice {
    fn drop(&mut self) {
        for notify in &self.0 {
            notify.notify_waiters();
        }
    }
}
impl CohortHistory {
    fn changing(&self) -> ChangeNotice {
        ChangeNotice(vec![Arc::clone(&self.1)])
    }
}
#[derive(Default)]
struct History {
    revision: u64,
    failed: bool,
    initialized: bool,
    // Terminal paths without an ordinary child continuation stay closed.
    // This permanent bit is never cleared by a successful child retirement.
    source_closed: bool,
    retirements: BTreeMap<u64, RetirementDebt>,
    next_task: u64,
    tasks: BTreeMap<u64, Task>,
    hold: Option<Arc<()>>,
}
struct Task {
    identity: Arc<TaskIdentity>,
    stop: Option<ControlStop>,
    origin: Origin,
    life: Life,
    next_operation: u64,
    operations: BTreeMap<u64, Operation>,
    invocation: Option<u64>,
    #[cfg(target_arch = "x86_64")]
    receive_timer: Option<Arc<ReceiveTimerObservation>>,
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum Origin {
    Command,
    Child {
        parent: u64,
        operation: u64,
        custody: ChildCustody,
    },
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum ChildCustody {
    AwaitingOwner,
    NativeChild,
    Constructed,
    Restored,
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum Life {
    Initializing,
    Stopped,
    Executing,
    Exiting,
    Terminal,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Effect {
    Execution,
    Native,
    Birth,
    Exec,
    Terminal,
    Unknown,
}
struct Operation {
    effect: Effect,
    outcome: Outcome,
    // Only the actual native clone3 ENTRY installs this debt. No userspace
    // clone_args snapshot stands in for the kernel's eventual argument copy.
    indirect_birth: Option<IndirectBirth>,
    peers: Option<Weak<NativePeers>>,
    #[cfg(target_arch = "x86_64")]
    receive_timer: Option<Arc<ReceiveTimerObservation>>,
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum IndirectBirth {
    AwaitingChild,
    Child(u64), // original event's monotonically allocated cohort member
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Outcome {
    Waiting,
    Stopped,
    Unknown,
}

/// Monotonic logical IDs, never reusable Vec slots or numeric PIDs.
#[derive(Clone)]
pub(super) struct Member {
    history: Arc<CohortHistory>,
    index: u64,
}
pub(super) struct ResumeOperation {
    member: Member,
    number: u64,
}
pub(super) struct ChildMembership {
    member: Member,
}
pub(super) struct ParentCompletion {
    member: Member,
    operation: u64,
    child: u64,
    completed: bool,
}
pub(crate) struct TerminalOperation(ResumeOperation);

struct RetirementDebt {
    identity: TaskIdentity,
    token: Arc<()>,
    terminal: bool,
    physical: bool,
    callbacks: bool,
}

/// Retained by the existing spawned child continuation across consuming self.
/// It observes completion; it owns no stop, callback, wait or join handle.
pub(super) struct ChildRetirement {
    member: Member,
    terminal: Arc<TerminalCleanup>,
    token: Arc<()>,
    completed: bool,
}

/// Lives inside the original injection future through control and restoration.
/// Resumes of that invocation do not create additional native debts.
pub(super) struct NativeOperation {
    member: Member,
    number: u64,
    syscall: Sysno,
    completed: bool,
    source_ioctl: Option<super::source_epoch::NativeIoctl>,
    peers: Option<Arc<NativePeers>>,
    #[cfg(target_arch = "x86_64")]
    receive_timer: Option<Arc<ReceiveTimerObservation>>,
}
pub(super) struct NativeReturn {
    owner: NativeOperation,
    raw: i64,
}

/// Observation of one positively restored private invocation. The current
/// ControlStop stays in the original Task; this receipt duplicates no authority.
#[cfg(target_arch = "x86_64")]
pub(crate) struct RestoredNativeContext {
    member: Member,
    operation: u64,
    control_revision: u64,
    timer_publication: Option<Arc<ReceiveTimerObservation>>,
}

#[cfg(target_arch = "x86_64")]
impl RestoredNativeContext {
    pub(super) fn receive_timer_published(&self) -> bool {
        let h = self.member.history.0.lock().unwrap();
        h.read_open()
            && self.timer_publication.as_ref().is_some_and(|timer| {
                *timer.progress.lock().unwrap()
                    == ReceiveTimerProgress::Restored {
                        control_revision: self.control_revision,
                    }
            })
    }

    pub(crate) fn validate(&self, stopped: &Stopped) -> Result<(), safeptrace::Errno> {
        use safeptrace::Errno;
        let identity = stopped.terminal_cleanup().task_identity()?;
        let h = self.member.history.0.lock().unwrap();
        let task = h.tasks.get(&self.member.index).ok_or(Errno::ESTALE)?;
        if !h.read_open()
            || !task.identity.same_generation(&identity)
            || !task.quiescent()
            || self.operation.checked_add(1) != Some(task.next_operation)
        {
            return Err(Errno::ESTALE);
        }
        let stop = task.stop.as_ref().ok_or(Errno::ESTALE)?;
        if stop.control_revision() != self.control_revision {
            return Err(Errno::ESTALE);
        }
        // A peer completion may legitimately advance h.revision. This exact
        // task's issued control revision and operation frontier must not move.
        stop.validate_current()
    }
}

impl History {
    fn fail(&mut self) {
        self.failed = true;
        // These are observer links, not original wait/effect/cleanup owners.
        // Every enrollment/mutation entry refuses this permanent failed state.
        self.tasks.clear();
        self.retirements.clear();
        #[cfg(test)]
        tests::population(self);
    }
    fn advance(&mut self) {
        match self.revision.checked_add(1) {
            Some(n) => self.revision = n,
            None => self.fail(),
        }
    }
    fn insert(&mut self, identity: TaskIdentity, origin: Origin) -> Option<u64> {
        if self.failed {
            return None;
        }
        let id = self.next_task;
        let Some(next) = id.checked_add(1) else {
            self.fail();
            return None;
        };
        self.next_task = next;
        self.tasks.insert(
            id,
            Task {
                identity: Arc::new(identity),
                stop: None,
                origin,
                life: Life::Initializing,
                next_operation: 0,
                operations: BTreeMap::new(),
                invocation: None,
                #[cfg(target_arch = "x86_64")]
                receive_timer: None,
            },
        );
        #[cfg(test)]
        tests::population(self);
        Some(id)
    }
    fn operation(&mut self, id: u64, effect: Effect) -> Option<u64> {
        if self.failed {
            return None;
        }
        let Some(task) = self.tasks.get_mut(&id) else {
            self.fail();
            return None;
        };
        let n = task.next_operation;
        let Some(next) = n.checked_add(1) else {
            self.fail();
            return None;
        };
        #[cfg(target_arch = "x86_64")]
        if let Some(timer) = &task.receive_timer {
            if !matches!(
                *timer.progress.lock().unwrap(),
                ReceiveTimerProgress::Restored { .. }
            ) {
                self.fail();
                return None;
            }
            task.receive_timer = None;
        }
        task.next_operation = next;
        task.operations.insert(
            n,
            Operation {
                effect,
                outcome: Outcome::Waiting,
                indirect_birth: None,
                peers: None,
                #[cfg(target_arch = "x86_64")]
                receive_timer: None,
            },
        );
        #[cfg(test)]
        tests::population(self);
        Some(n)
    }
}
impl CohortHistory {
    /// Shutdown may signal a group whose original leader has retired. Close
    /// future admission under the SAME lock as acquisition, and refuse the
    /// physical signal while the current source job still owns its interval.
    pub(super) fn before_group_signal(&self) -> Result<(), safeptrace::Errno> {
        let _change = self.changing();
        let mut h = self.0.lock().unwrap();
        h.fail();
        if h.hold.is_some() {
            Err(safeptrace::Errno::EBUSY)
        } else {
            Ok(())
        }
    }
    /// The existing ExitFuture has been selected, before it cancels the
    /// ordinary continuation. This is handoff debt, not final completion.
    pub(super) fn terminal_selected(&self, result: &Result<Stopped, TraceError>) {
        let identity = match result {
            Ok(stopped) => stopped.terminal_cleanup().task_identity(),
            Err(TraceError::Died(zombie)) => zombie.terminal_cleanup().task_identity(),
            _ => return,
        };
        let Ok(identity) = identity else {
            self.fail();
            return;
        };
        let _change = self.changing();
        let mut h = self.0.lock().unwrap();
        if h.failed {
            return;
        }
        let Some(index) = h
            .tasks
            .iter()
            .find(|(_, t)| t.identity.same_generation(&identity))
            .map(|(index, _)| *index)
        else {
            h.source_closed = true;
            return;
        };
        h.terminal_pending(index);
        let task = h.tasks.get_mut(&index).unwrap();
        task.life = Life::Exiting;
        task.stop = None;
        h.advance();
    }
    pub(super) fn fail(&self) {
        let _change = self.changing();
        self.0.lock().unwrap().fail();
    }
    pub(super) fn initial_command(self: &Arc<Self>, stopped: &Stopped) -> Option<Member> {
        let identity = stopped.terminal_cleanup().task_identity().ok()?;
        let stop = stopped.control_stop().ok()?;
        let _change = self.changing();
        let mut h = self.0.lock().unwrap();
        if h.initialized || h.failed {
            h.fail();
            return None;
        }
        h.initialized = true;
        h.advance();
        let index = h.insert(identity, Origin::Command)?;
        h.tasks.get_mut(&index)?.stop = Some(stop);
        h.operation(index, Effect::Exec)?;
        let member = Member {
            history: Arc::clone(self),
            index,
        };
        drop(h);
        #[cfg(test)]
        tests::initial(&member, stopped);
        Some(member)
    }
    pub(super) fn terminal_owner(
        self: &Arc<Self>,
        terminal: &TerminalCleanup,
    ) -> Option<TerminalOperation> {
        let identity = terminal.task_identity().ok()?;
        let _change = self.changing();
        let mut h = self.0.lock().unwrap();
        let Some(index) = h
            .tasks
            .iter()
            .find(|(_, t)| t.identity.same_generation(&identity))
            .map(|(index, _)| *index)
        else {
            h.source_closed = true;
            return None;
        };
        h.terminal_pending(index);
        h.advance();
        let task = h.tasks.get_mut(&index)?;
        task.stop = None;
        task.life = Life::Exiting;
        let number = h.operation(index, Effect::Terminal)?;
        let member = Member {
            history: Arc::clone(self),
            index,
        };
        drop(h);
        #[cfg(test)]
        tests::terminal_pending(&member);
        Some(TerminalOperation(ResumeOperation { member, number }))
    }
}

/// Physical exclusions plus the exact run revision and requesting member.
/// Neither Clone nor serializable. Arc references retain this SAME interval.
pub(crate) struct FollowedHold {
    history: Arc<CohortHistory>,
    revision: u64,
    sender: u64,
    ticket: Arc<()>,
    controls: BTreeMap<u64, Arc<safeptrace::ControlHold>>,
    transferred: bool,
}

impl Member {
    /// Only the original ordinary child spawn installs this single observer.
    pub(super) fn ordinary_child(&self, terminal: Arc<TerminalCleanup>) -> Option<ChildRetirement> {
        let identity = terminal.task_identity().ok()?;
        let _change = self.history.changing();
        let mut h = self.history.0.lock().unwrap();
        if h.failed || h.source_closed {
            return None;
        }
        if !h.tasks.get(&self.index).is_some_and(|task| {
            task.identity.same_generation(&identity)
                && matches!(
                    task.origin,
                    Origin::Child {
                        custody: ChildCustody::Constructed,
                        ..
                    }
                )
        }) || h.retirements.contains_key(&self.index)
        {
            h.fail();
            return None;
        }
        let token = Arc::new(());
        h.retirements.insert(
            self.index,
            RetirementDebt {
                identity,
                token: token.clone(),
                terminal: false,
                physical: false,
                callbacks: false,
            },
        );
        h.advance();
        Some(ChildRetirement {
            member: self.clone(),
            terminal,
            token,
            completed: false,
        })
    }

    /// After consuming tool_exit_ordinary AND session.finished. No callback
    /// success is inferred from a final wait or from the task counters.
    pub(super) fn ordinary_callbacks_completed(&self, terminal: &TerminalCleanup) {
        let identity = terminal.task_identity();
        let _change = self.history.changing();
        let mut h = self.history.0.lock().unwrap();
        if h.failed {
            return;
        }
        if let Some(debt) = h.retirements.get_mut(&self.index)
            && debt.physical
            && identity
                .as_ref()
                .is_ok_and(|id| debt.identity.same_generation(id))
        {
            debt.callbacks = true;
            h.advance();
        } else {
            // Root, startup, and direct terminal paths have no such producer.
            h.source_closed = true;
        }
    }

    pub(super) fn acquire(&self) -> Result<FollowedHold, safeptrace::Errno> {
        use safeptrace::Errno;
        let _change = self.history.changing();
        let mut h = self.history.0.lock().unwrap();
        if !h.read_open() || !h.tasks.contains_key(&self.index) {
            return Err(Errno::ESTALE);
        }
        if h.hold.is_some() {
            return Err(Errno::EBUSY);
        }
        let mut controls = BTreeMap::new();
        for (&id, task) in &h.tasks {
            #[cfg(target_arch = "x86_64")]
            if task.receive_timer.as_ref().is_some_and(|timer| {
                !matches!(
                    *timer.progress.lock().unwrap(),
                    ReceiveTimerProgress::Restored { .. }
                )
            }) {
                return Err(Errno::EBUSY);
            }
            if !task.quiescent() {
                return Err(Errno::EBUSY);
            }
            // Partial failure drops all earlier gates before issuing authority.
            controls.insert(
                id,
                Arc::new(task.stop.as_ref().ok_or(Errno::ESTALE)?.hold()?),
            );
        }
        let revision = h.revision.checked_add(1).ok_or(Errno::EOVERFLOW)?;
        let ticket = Arc::new(());
        h.revision = revision;
        h.hold = Some(Arc::clone(&ticket));
        Ok(FollowedHold {
            history: Arc::clone(&self.history),
            revision,
            sender: self.index,
            ticket,
            controls,
            transferred: false,
        })
    }
}

/// The existing original invocation's finite peer custody. The sender's
/// FatalTaskStop also retains this owner; canceled Tool futures cannot drop gates.
pub(crate) struct NativePeers {
    member: Member,
    number: u64,
    entry: safeptrace::SyscallEntry,
    ticket: Arc<()>,
    members: BTreeMap<u64, TaskIdentity>,
    sender: Weak<crate::tracer::FatalTaskStop>,
    session: Weak<super::FatalSession>,
    state: Mutex<NativePeerState>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum NativePeerPhase {
    Armed,
    Executing,
    Restored,
    Retired,
    Unknown,
}
struct NativePeerState {
    revision: u64,
    phase: NativePeerPhase,
    bindings: Vec<(
        Arc<crate::tracer::FatalTaskStop>,
        Arc<safeptrace::ControlHold>,
    )>,
}
impl Member {
    pub(super) fn native_sendto_with_peers(
        &self,
        mut hold: FollowedHold,
        entry: safeptrace::SyscallEntry,
        tasks: &[Arc<crate::tracer::FatalTaskStop>],
        session: &Arc<super::FatalSession>,
    ) -> Result<NativeOperation, safeptrace::Errno> {
        use safeptrace::Errno;
        hold.validate()?;
        if !Arc::ptr_eq(&hold.history, &self.history)
            || hold.sender != self.index
            || entry.arch != 0xc000003e
            || entry.number != Sysno::sendto as u64
            || !entry.seccomp
        {
            return Err(Errno::EPROTO);
        }
        let actual = hold
            .sender()
            .with_stopped(|stopped| stopped.syscall_entry())?
            .map_err(|_| Errno::EPROTO)?;
        if actual != entry {
            return Err(Errno::EPROTO);
        }
        let mut members = BTreeMap::new();
        let mut bindings = Vec::new();
        let mut sender = None;
        {
            let h = self.history.0.lock().unwrap();
            if tasks.len() != h.tasks.len() {
                return Err(Errno::ESTALE);
            }
            for (&id, member) in &h.tasks {
                let mut matching = tasks.iter().filter(|task| {
                    task.terminal
                        .task_identity()
                        .is_ok_and(|identity| member.identity.same_generation(&identity))
                });
                let task = matching.next().ok_or(Errno::ESTALE)?;
                if matching.next().is_some()
                    || task.frozen.load(std::sync::atomic::Ordering::Acquire)
                {
                    return Err(Errno::ESTALE);
                }
                members.insert(id, task.terminal.task_identity()?);
                bindings.push((Arc::clone(task), Arc::clone(&hold.controls[&id])));
                if id == self.index {
                    sender = Some(Arc::clone(task));
                }
            }
        }
        let sender = sender.ok_or(Errno::ESTALE)?;
        if sender.peer_invocation.lock().unwrap().is_some() {
            return Err(Errno::EBUSY);
        }
        #[cfg(all(test, cohort_final_test, target_arch = "x86_64"))]
        peer_tests::bind_controls(&bindings)?;
        #[cfg(not(all(test, cohort_final_test, target_arch = "x86_64")))]
        crate::tracer::FatalTaskStop::bind_peer_controls(&bindings)?;
        let a = entry.arguments;
        let args = SyscallArgs::new(
            a[0] as usize,
            a[1] as usize,
            a[2] as usize,
            a[3] as usize,
            a[4] as usize,
            a[5] as usize,
        );
        let Some(mut native) = self.native(Sysno::sendto, args) else {
            // No sender gate or native effect was released on this branch.
            crate::tracer::FatalTaskStop::release_peer_controls(&bindings, false)?;
            return Err(Errno::ESTALE);
        };
        let _change = self.history.changing();
        let mut h = self.history.0.lock().unwrap();
        if h.failed
            || !h
                .tasks
                .get(&self.index)
                .is_some_and(|task| task.operations.contains_key(&native.number))
        {
            drop(h);
            crate::tracer::FatalTaskStop::release_peer_controls(&bindings, false)?;
            return Err(Errno::ESTALE);
        }
        let peers = Arc::new(NativePeers {
            member: self.clone(),
            number: native.number,
            entry,
            ticket: Arc::clone(&hold.ticket),
            members,
            sender: Arc::downgrade(&sender),
            session: Arc::downgrade(session),
            state: Mutex::new(NativePeerState {
                revision: h.revision,
                phase: NativePeerPhase::Armed,
                bindings,
            }),
        });
        h.tasks
            .get_mut(&self.index)
            .unwrap()
            .operations
            .get_mut(&native.number)
            .unwrap()
            .peers = Some(Arc::downgrade(&peers));
        *sender.peer_invocation.lock().unwrap() = Some(Arc::clone(&peers));
        native.peers = Some(Arc::clone(&peers));
        // From here onward every error retains the original physical owner.
        hold.transferred = true;
        hold.controls.clear();
        drop(h);
        {
            let mut state = peers.state.lock().unwrap();
            let index = state
                .bindings
                .iter()
                .position(|(task, _)| Arc::ptr_eq(task, &sender))
                .ok_or(Errno::ESTALE)?;
            crate::tracer::FatalTaskStop::release_peer_controls(
                &state.bindings[index..index + 1],
                false,
            )?;
            state.bindings.remove(index); // only this sender may now resume
        }
        Ok(native)
    }
}
impl NativePeers {
    fn matches_history(&self, h: &History, state: &NativePeerState) -> bool {
        h.read_open()
            && h.revision == state.revision
            && h.hold
                .as_ref()
                .is_some_and(|ticket| Arc::ptr_eq(ticket, &self.ticket))
            && h.tasks.keys().eq(self.members.keys())
            && h.tasks.iter().all(|(id, task)| {
                self.members[id].same_generation(&task.identity)
                    && if *id == self.member.index {
                        match state.phase {
                            NativePeerPhase::Armed | NativePeerPhase::Executing => {
                                task.invocation == Some(self.number)
                                    && task.operations.len() == 1
                                    && task.operations.contains_key(&self.number)
                            }
                            NativePeerPhase::Restored => task.quiescent(),
                            NativePeerPhase::Retired | NativePeerPhase::Unknown => false,
                        }
                    } else {
                        task.quiescent()
                    }
            })
            && state
                .bindings
                .iter()
                .all(|(_, control)| control.validate().is_ok())
            && crate::tracer::FatalTaskStop::peer_controls_match(&state.bindings, false)
    }
    fn before_resume(
        &self,
        h: &History,
        member: u64,
        entry: Option<safeptrace::SyscallEntry>,
    ) -> bool {
        let state = self.state.lock().unwrap();
        state.phase == NativePeerPhase::Armed
            && self.matches_history(h, &state)
            && member == self.member.index
            && entry == Some(self.entry)
            && h.tasks
                .get(&member)
                .is_some_and(|task| task.invocation == Some(self.number))
    }
    fn resumed(&self, revision: u64) {
        let mut state = self.state.lock().unwrap();
        state.phase = NativePeerPhase::Executing;
        state.revision = revision;
    }
    /// A source observer's None never authorizes this physical transition.
    /// The new original route checks this before actually releasing the stop.
    pub(super) fn require_resume_owner(
        &self,
        operation: Option<&ResumeOperation>,
    ) -> Result<(), safeptrace::Errno> {
        let h = self.member.history.0.lock().unwrap();
        let state = self.state.lock().unwrap();
        if state.phase != NativePeerPhase::Executing
            || !self.matches_history(&h, &state)
            || !operation.is_some_and(|operation| {
                operation.number == self.number
                    && operation.member.index == self.member.index
                    && Arc::ptr_eq(&operation.member.history, &self.member.history)
            })
        {
            return Err(safeptrace::Errno::ESTALE);
        }
        Ok(())
    }
    fn returned(&self, h: &History) -> bool {
        let state = self.state.lock().unwrap();
        state.phase == NativePeerPhase::Executing
            && self.matches_history(h, &state)
            && h.tasks.get(&self.member.index).is_some_and(|task| {
                task.life == Life::Stopped
                    && task.invocation == Some(self.number)
                    && task
                        .operations
                        .get(&self.number)
                        .is_some_and(|op| op.outcome == Outcome::Stopped)
            })
    }
    fn restored(&self) {
        self.state.lock().unwrap().phase = NativePeerPhase::Restored;
    }
    fn unknown(&self) {
        self.state.lock().unwrap().phase = NativePeerPhase::Unknown;
        if let Some(session) = self.session.upgrade() {
            session.fail(
                anyhow::anyhow!("original peer-held Sendto lost return/restoration custody").into(),
            );
        }
    }
    pub(super) fn retire(self: &Arc<Self>) -> Result<(), safeptrace::Errno> {
        use safeptrace::Errno;
        let _change = self.member.history.changing();
        let mut h = self.member.history.0.lock().unwrap();
        let mut state = self.state.lock().unwrap();
        if state.phase != NativePeerPhase::Restored
            || !self.matches_history(&h, &state)
            || !h.tasks.values().all(Task::quiescent)
        {
            return Err(Errno::ESTALE);
        }
        let sender = self.sender.upgrade().ok_or(Errno::ESTALE)?;
        let mut slot = sender.peer_invocation.lock().unwrap();
        if !slot.as_ref().is_some_and(|owner| Arc::ptr_eq(owner, self)) {
            return Err(Errno::ESTALE);
        }
        crate::tracer::FatalTaskStop::release_peer_controls(&state.bindings, false)?;
        state.bindings.clear();
        h.hold = None;
        state.phase = NativePeerPhase::Retired;
        *slot = None;
        Ok(())
    }
    /// Only the original all-task-frozen cleanup barrier calls this. This is
    /// fatal custody transfer, never a successful native result or source read.
    pub(crate) fn retire_frozen(
        self: &Arc<Self>,
        tasks: &[Arc<crate::tracer::FatalTaskStop>],
    ) -> Result<(), safeptrace::Errno> {
        use std::sync::atomic::Ordering;

        use safeptrace::Errno;
        if tasks.len() != self.members.len()
            || tasks
                .iter()
                .any(|task| !task.frozen.load(Ordering::Acquire))
            || self.members.values().any(|identity| {
                tasks
                    .iter()
                    .filter(|task| {
                        task.terminal
                            .task_identity()
                            .is_ok_and(|actual| identity.same_generation(&actual))
                    })
                    .count()
                    != 1
            })
        {
            return Err(Errno::ESTALE);
        }
        let _change = self.member.history.changing();
        let mut h = self.member.history.0.lock().unwrap();
        let mut state = self.state.lock().unwrap();
        if state.phase == NativePeerPhase::Retired
            || !h
                .hold
                .as_ref()
                .is_some_and(|ticket| Arc::ptr_eq(ticket, &self.ticket))
            || !crate::tracer::FatalTaskStop::peer_controls_match(&state.bindings, true)
        {
            return Err(Errno::ESTALE);
        }
        let sender = self.sender.upgrade().ok_or(Errno::ESTALE)?;
        let mut slot = sender.peer_invocation.lock().unwrap();
        if !slot.as_ref().is_some_and(|owner| Arc::ptr_eq(owner, self)) {
            return Err(Errno::ESTALE);
        }
        h.fail(); // irreversible even if a later physical release refuses
        crate::tracer::FatalTaskStop::release_peer_controls(&state.bindings, true)?;
        state.bindings.clear();
        h.hold = None;
        state.phase = NativePeerPhase::Unknown;
        *slot = None;
        Ok(())
    }
}

impl Task {
    #[cfg(target_arch = "x86_64")]
    fn checked_return_phase(&self, number: u64) -> bool {
        number.checked_add(1) == Some(self.next_operation)
            && matches!(
                self.origin,
                Origin::Command
                    | Origin::Child {
                        custody: ChildCustody::Restored,
                        ..
                    }
            )
            && self.operations.get(&number).is_some_and(|operation| {
                checked_return_phase(
                    self.life,
                    self.operations.len(),
                    self.invocation,
                    number,
                    operation.effect,
                    operation.outcome,
                ) && operation.indirect_birth.is_none()
                    && operation.peers.is_none()
            })
    }

    fn quiescent(&self) -> bool {
        settled_phase(
            self.life,
            self.operations.len(),
            self.invocation,
            matches!(
                self.origin,
                Origin::Command
                    | Origin::Child {
                        custody: ChildCustody::Restored,
                        ..
                    }
            ),
        )
    }
}

#[cfg(target_arch = "x86_64")]
fn checked_return_phase(
    life: Life,
    operations: usize,
    invocation: Option<u64>,
    number: u64,
    effect: Effect,
    outcome: Outcome,
) -> bool {
    life == Life::Stopped
        && operations == 1
        && invocation == Some(number)
        && effect == Effect::Native
        && outcome == Outcome::Stopped
}

// A metadata predicate, never a stop/hold constructor.
fn settled_phase(life: Life, operations: usize, invocation: Option<u64>, restored: bool) -> bool {
    life == Life::Stopped && operations == 0 && invocation.is_none() && restored
}

impl FollowedHold {
    pub(crate) fn validate(&self) -> Result<(), safeptrace::Errno> {
        let h = self.history.0.lock().unwrap();
        if !h.read_open()
            || h.revision != self.revision
            || !h
                .hold
                .as_ref()
                .is_some_and(|t| Arc::ptr_eq(t, &self.ticket))
            || !h.tasks.contains_key(&self.sender)
            || !h.tasks.keys().eq(self.controls.keys())
            || h.tasks.values().any(|t| !t.quiescent())
        {
            return Err(safeptrace::Errno::ESTALE);
        }
        for control in self.controls.values() {
            control.validate()?;
        }
        Ok(())
    }

    pub(super) fn sender(&self) -> Arc<safeptrace::ControlHold> {
        Arc::clone(&self.controls[&self.sender])
    }
}

impl Drop for FollowedHold {
    fn drop(&mut self) {
        if self.transferred {
            return;
        }
        // Release physical gates before clearing the run-level signal fence.
        self.controls.clear();
        let _change = self.history.changing();
        let mut h = self.history.0.lock().unwrap();
        assert!(
            h.hold
                .as_ref()
                .is_some_and(|t| Arc::ptr_eq(t, &self.ticket))
        );
        h.hold = None;
    }
}
fn syscall_effect(nr: u64) -> Effect {
    if [
        Sysno::clone,
        Sysno::clone3,
        #[cfg(target_arch = "x86_64")]
        Sysno::fork,
        #[cfg(target_arch = "x86_64")]
        Sysno::vfork,
    ]
    .iter()
    .any(|&n| n as u64 == nr)
    {
        Effect::Birth
    } else if nr == Sysno::execve as u64 || nr == Sysno::execveat as u64 {
        Effect::Exec
    } else if nr == Sysno::exit as u64 || nr == Sysno::exit_group as u64 {
        Effect::Terminal
    } else if nr as i32 == -1 {
        Effect::Execution
    } else if nr >= 0x4000_0000 {
        Effect::Unknown
    } else {
        Effect::Native
    }
}
#[cfg(test)]
fn effect(stopped: &Stopped) -> Effect {
    match stopped.pending_syscall_entry() {
        Ok(None) => Effect::Execution,
        Ok(Some(entry)) => syscall_effect(entry.number),
        Err(_) => Effect::Unknown,
    }
}
fn exposed(syscall: Sysno, args: SyscallArgs) -> bool {
    match syscall {
        Sysno::clone => args.arg0 & libc::CLONE_UNTRACED as usize != 0,
        Sysno::clone3 | Sysno::execve | Sysno::execveat => false,
        #[cfg(target_arch = "x86_64")]
        Sysno::fork | Sysno::vfork => false,
        Sysno::ioctl => args.arg1 as u32 != 0x5421,
        Sysno::prctl => matches!(args.arg0 as u32, 22 | 0x59616d61),
        nr => super::source_epoch::observes(nr),
    }
}
impl Member {
    pub(super) fn native(&self, syscall: Sysno, args: SyscallArgs) -> Option<NativeOperation> {
        self.native_classified(syscall, args, None)
    }
    pub(super) fn native_classified(
        &self,
        syscall: Sysno,
        args: SyscallArgs,
        source_ioctl: Option<super::source_epoch::NativeIoctl>,
    ) -> Option<NativeOperation> {
        let _change = self.history.changing();
        let mut h = self.history.0.lock().unwrap();
        if h.failed {
            return None;
        }
        // Synchronous return is not retirement of asynchronous/exposed writers.
        // Keep the old exposure family closed; controlled birth has its own
        // original parent and child owners. No SourceEpoch rule is changed.
        if exposed(syscall, args) && !(syscall == Sysno::ioctl && source_ioctl.is_some()) {
            h.fail();
            return None;
        }
        if h.tasks
            .get(&self.index)
            .is_none_or(|t| t.invocation.is_some())
        {
            h.fail();
            return None;
        }
        h.advance();
        let number = h.operation(self.index, syscall_effect(syscall as u64))?;
        let task = h.tasks.get_mut(&self.index)?;
        if syscall == Sysno::clone3 {
            task.operations.get_mut(&number)?.indirect_birth = Some(IndirectBirth::AwaitingChild);
        }
        task.invocation = Some(number);
        drop(h);
        #[cfg(test)]
        tests::before_resume(self, syscall_effect(syscall as u64));
        #[cfg(all(test, cohort_final_test, target_arch = "x86_64"))]
        clone3_tests::native(self, number, syscall);
        Some(NativeOperation {
            member: self.clone(),
            number,
            syscall,
            completed: false,
            source_ioctl,
            peers: None,
            #[cfg(target_arch = "x86_64")]
            receive_timer: None,
        })
    }
    pub(super) fn close_observation(&self) {
        self.history.fail();
    }
    pub(super) fn observe_entry(&self, entry: safeptrace::SyscallEntry) {
        self.observe_entry_classified(entry, false);
    }
    pub(super) fn observe_entry_classified(
        &self,
        entry: safeptrace::SyscallEntry,
        original_ioctl: bool,
    ) {
        // SourceEpoch still permanently revokes clone3. The followed cohort
        // instead retains the real native invocation until its typed outcome;
        // a pre-effect Tool observation alone does not authorize execution.
        if entry.arch != 0xc000_003e {
            self.history.fail();
        } else if let Some(nr) = Sysno::new(entry.number as usize) {
            let a = entry.arguments;
            if exposed(
                nr,
                SyscallArgs::new(
                    a[0] as usize,
                    a[1] as usize,
                    a[2] as usize,
                    a[3] as usize,
                    a[4] as usize,
                    a[5] as usize,
                ),
            ) && !(nr == Sysno::ioctl && original_ioctl)
            {
                self.history.fail();
            }
        } else if entry.number as i32 >= 0 {
            self.history.fail();
        }
    }
    pub(super) fn before_entry_observation(&self, stopped: &Stopped) -> Option<ResumeOperation> {
        self.before_resume_inner(stopped, true, false)
    }
    pub(super) fn before_resume(&self, stopped: &Stopped) -> Option<ResumeOperation> {
        self.before_resume_inner(stopped, false, false)
    }
    pub(super) fn before_resume_classified(
        &self,
        stopped: &Stopped,
        administrative: bool,
        original_ioctl: bool,
    ) -> Option<ResumeOperation> {
        self.before_resume_inner(stopped, administrative, original_ioctl)
    }
    fn before_resume_inner(
        &self,
        stopped: &Stopped,
        administrative: bool,
        original_ioctl: bool,
    ) -> Option<ResumeOperation> {
        if self.history.0.lock().unwrap().failed {
            return None;
        }
        let observed_entry = stopped.pending_syscall_entry();
        let peer_entry = observed_entry.as_ref().ok().copied().flatten();
        let (mut effect, exposure) = match observed_entry {
            Ok(None) => (Effect::Execution, false),
            Ok(Some(entry)) => {
                let exposure = super::source_epoch::observed_syscalls()
                    .iter()
                    .find(|&&nr| nr as u64 == entry.number)
                    .is_some_and(|&nr| {
                        let a = entry.arguments;
                        exposed(
                            nr,
                            SyscallArgs::new(
                                a[0] as usize,
                                a[1] as usize,
                                a[2] as usize,
                                a[3] as usize,
                                a[4] as usize,
                                a[5] as usize,
                            ),
                        )
                    });
                (
                    syscall_effect(entry.number),
                    exposure && !(entry.number == Sysno::ioctl as u64 && original_ioctl),
                )
            }
            Err(_) => (Effect::Unknown, true),
        };
        if administrative {
            effect = Effect::Execution;
        }
        let identity = stopped.terminal_cleanup().task_identity();
        let _change = self.history.changing();
        let mut h = self.history.0.lock().unwrap();
        if h.failed {
            return None;
        }
        if exposure || effect == Effect::Unknown {
            h.fail();
            return None;
        }
        if !identity.as_ref().is_ok_and(|id| {
            h.tasks
                .get(&self.index)
                .is_some_and(|t| t.identity.same_generation(id))
        }) {
            h.fail();
            return None;
        }
        let invocation = h.tasks.get(&self.index)?.invocation;
        let peers = invocation.and_then(|number| {
            h.tasks
                .get(&self.index)?
                .operations
                .get(&number)?
                .peers
                .as_ref()?
                .upgrade()
        });
        if peers
            .as_ref()
            .is_some_and(|owner| !owner.before_resume(&h, self.index, peer_entry))
        {
            h.fail();
            return None;
        }
        h.advance();
        if let Some(owner) = &peers {
            owner.resumed(h.revision);
        }
        // CONT of an unowned native syscall has no typed return/restoration
        // owner. Do not reinterpret a later generic stop as its completion.
        if invocation.is_none() && !matches!(effect, Effect::Execution | Effect::Terminal) {
            h.fail();
            return None;
        }
        let number = match invocation {
            Some(n) => n,
            None => h.operation(self.index, effect)?,
        };
        let task = h.tasks.get_mut(&self.index)?;
        task.stop = None;
        task.life = Life::Executing;
        task.operations.get_mut(&number)?.outcome = Outcome::Waiting;
        drop(h);
        #[cfg(test)]
        tests::before_resume(self, effect);
        Some(ResumeOperation {
            member: self.clone(),
            number,
        })
    }
    fn child_event(&self, operation: u64, cleanup: &TerminalCleanup) {
        #[cfg(all(test, cohort_final_test, target_arch = "x86_64"))]
        clone3_tests::child_event();
        let identity = cleanup.task_identity();
        let _change = self.history.changing();
        let mut h = self.history.0.lock().unwrap();
        if h.failed {
            return;
        }
        let Ok(identity) = identity else {
            h.fail();
            return;
        };
        if h.tasks
            .values()
            .any(|t| t.identity.same_generation(&identity))
            || !h
                .tasks
                .get(&self.index)
                .and_then(|t| t.operations.get(&operation))
                .is_some_and(|op| {
                    op.effect == Effect::Birth
                        && !matches!(op.indirect_birth, Some(IndirectBirth::Child(_)))
                })
        {
            h.fail();
            return;
        }
        h.advance();
        let Some(child) = h.insert(
            identity,
            Origin::Child {
                parent: self.index,
                operation,
                custody: ChildCustody::AwaitingOwner,
            },
        ) else {
            return;
        };
        let op = h
            .tasks
            .get_mut(&self.index)
            .unwrap()
            .operations
            .get_mut(&operation)
            .unwrap();
        if op.indirect_birth.is_some() {
            op.indirect_birth = Some(IndirectBirth::Child(child));
        }
        drop(h);
        #[cfg(test)]
        tests::birth(self, cleanup);
    }
    pub(super) fn retain_child(
        &self,
        cleanup: &TerminalCleanup,
    ) -> Option<(ChildMembership, ParentCompletion)> {
        let identity = cleanup.task_identity().ok()?;
        let _change = self.history.changing();
        let mut h = self.history.0.lock().unwrap();
        if h.failed {
            return None;
        }
        let Some(index) = h
            .tasks
            .iter()
            .find(|(_, t)| t.identity.same_generation(&identity))
            .map(|(id, _)| *id)
        else {
            h.fail();
            return None;
        };
        let Origin::Child {
            parent,
            operation,
            custody,
        } = &mut h.tasks.get_mut(&index)?.origin
        else {
            h.fail();
            return None;
        };
        if *parent != self.index || *custody != ChildCustody::AwaitingOwner {
            h.fail();
            return None;
        }
        *custody = ChildCustody::NativeChild;
        let operation = *operation;
        h.advance();
        Some((
            ChildMembership {
                member: Member {
                    history: Arc::clone(&self.history),
                    index,
                },
            },
            ParentCompletion {
                member: self.clone(),
                operation,
                child: index,
                completed: false,
            },
        ))
    }
    pub(super) fn child_restored(&self) {
        let _change = self.history.changing();
        let mut h = self.history.0.lock().unwrap();
        if h.failed {
            return;
        }
        let Some(task) = h.tasks.get_mut(&self.index) else {
            h.fail();
            return;
        };
        let Origin::Child { custody, .. } = &mut task.origin else {
            h.fail();
            return;
        };
        if *custody != ChildCustody::Constructed {
            h.fail();
            return;
        }
        *custody = ChildCustody::Restored;
        h.advance();
        drop(h);
        #[cfg(test)]
        tests::restored(self);
    }
    pub(super) fn exec_observed(&self) {
        let _change = self.history.changing();
        let mut h = self.history.0.lock().unwrap();
        if h.failed {
            return;
        }
        if let Some(task) = h.tasks.get_mut(&self.index) {
            task.stop = None;
        }
        h.operation(self.index, Effect::Exec);
        drop(h);
        #[cfg(test)]
        tests::exec(self);
        // Original exec transfer owners remain responsible for all effects.
        self.history.fail();
    }
    pub(super) fn initial_ready(&self) {
        let _change = self.history.changing();
        let mut h = self.history.0.lock().unwrap();
        if h.failed {
            return;
        }
        if let Some(task) = h.tasks.get_mut(&self.index)
            && matches!(task.origin, Origin::Command)
        {
            task.operations.remove(&0);
        } else {
            h.fail();
        }
    }
    pub(super) fn terminal_observed(&self) {
        let _change = self.history.changing();
        let mut h = self.history.0.lock().unwrap();
        h.terminal_pending(self.index);
        // The callback may follow retirement, but cannot resurrect a task.
        if let Some(task) = h.tasks.get_mut(&self.index) {
            task.stop = None;
            task.life = Life::Terminal;
        }
        h.advance();
    }
}

impl History {
    fn terminal_pending(&mut self, index: u64) {
        if let Some(debt) = self.retirements.get_mut(&index) {
            debt.terminal = true;
        } else {
            self.source_closed = true;
        }
    }
    fn read_open(&self) -> bool {
        self.initialized
            && !self.failed
            && !self.source_closed
            && !self.retirements.values().any(|debt| debt.terminal)
    }
}

impl ChildRetirement {
    /// Last action of the original spawned child body, after its actual final
    /// status check, consuming callbacks, handoff bookkeeping and publication.
    pub(super) fn completed(mut self, status: Option<reverie::process::ExitStatus>) {
        #[cfg(all(test, cohort_final_test, target_arch = "x86_64"))]
        if retirement_tests::abandon_completion() {
            return;
        }
        let actual = self.terminal.observed_terminal();
        let retired = self.terminal.wait(std::time::Duration::ZERO);
        let _change = self.member.history.changing();
        let mut h = self.member.history.0.lock().unwrap();
        if h.failed {
            return;
        }
        let valid = status.is_some()
            && matches!(actual, Some(Ok(actual)) if Some(actual) == status)
            && retired
            && !h.tasks.contains_key(&self.member.index)
            && h.retirements.get(&self.member.index).is_some_and(|debt| {
                Arc::ptr_eq(&debt.token, &self.token)
                    && debt.terminal
                    && debt.physical
                    && debt.callbacks
            });
        if !valid {
            h.source_closed = true;
            return;
        }
        h.retirements.remove(&self.member.index);
        h.advance();
        self.completed = true;
    }
}
impl Drop for ChildRetirement {
    fn drop(&mut self) {
        if !self.completed {
            let _change = self.member.history.changing();
            let mut h = self.member.history.0.lock().unwrap();
            // Permanent refusal, without retiring unresolved observer work.
            h.source_closed = true;
            // These tokens are only source-admission observers. Once closed
            // forever they must not retain generations for later children.
            // The original tasks/operations and physical owners are untouched.
            h.retirements.clear();
            h.advance();
        }
    }
}
impl ResumeOperation {
    pub(super) fn observe(self, result: &Result<Wait, TraceError>) {
        let (identity, stop) = match result {
            Ok(Wait::Stopped(s, _)) => (
                s.terminal_cleanup().task_identity().ok(),
                s.control_stop().ok(),
            ),
            _ => (None, None),
        };
        if let Ok(Wait::Stopped(_, Event::NewChild(_, child))) = result {
            self.member
                .child_event(self.number, &child.terminal_cleanup());
        }
        let _change = self.member.history.changing();
        let mut h = self.member.history.0.lock().unwrap();
        if h.failed {
            return;
        }
        let Some(task) = h.tasks.get_mut(&self.member.index) else {
            return;
        };
        let Some(operation) = task.operations.get_mut(&self.number) else {
            h.fail();
            return;
        };
        #[cfg(target_arch = "x86_64")]
        if operation.receive_timer.is_some()
            && !matches!(result, Ok(Wait::Stopped(_, Event::Syscall)))
        {
            // A marked timer has no signal/restart/private-trap continuation.
            // Keep the original wait/cleanup owner, close only observation.
            h.fail();
            return;
        }
        if operation.indirect_birth.is_some()
            && !matches!(
                result,
                Ok(Wait::Stopped(
                    _,
                    Event::Syscall | Event::NewChild(..) | Event::VforkDone
                ))
            )
        {
            // An interrupted/unknown stop is not a no-child outcome. Keep
            // Linux delivery under its original owner, close only publication.
            h.fail();
            return;
        }
        match result {
            Ok(Wait::Stopped(_, event))
                if identity
                    .as_ref()
                    .is_some_and(|id| task.identity.same_generation(id)) =>
            {
                task.stop = stop;
                task.life = if matches!(event, Event::Exit) {
                    Life::Exiting
                } else {
                    Life::Stopped
                };
                operation.outcome = Outcome::Stopped;
                if operation.effect == Effect::Execution
                    && !matches!(event, Event::NewChild(..) | Event::Exec(_) | Event::Exit)
                {
                    task.operations.remove(&self.number);
                }
            }
            _ => {
                h.fail();
                return;
            }
        }
        if matches!(result, Ok(Wait::Stopped(_, Event::Exit))) {
            h.operation(self.member.index, Effect::Terminal);
        }
        drop(h);
        if matches!(result, Ok(Wait::Stopped(_, Event::Exec(_)))) {
            self.member.exec_observed();
        } else {
            #[cfg(test)]
            tests::wait(&self.member, result);
        }
    }
}
impl Drop for ResumeOperation {
    fn drop(&mut self) {
        let _change = self.member.history.changing();
        let mut h = self.member.history.0.lock().unwrap();
        let exiting = h
            .tasks
            .get(&self.member.index)
            .is_some_and(|t| t.life == Life::Exiting);
        if let Some(op) = h
            .tasks
            .get_mut(&self.member.index)
            .and_then(|t| t.operations.get_mut(&self.number))
            && op.outcome == Outcome::Waiting
        {
            op.outcome = Outcome::Unknown;
            if !exiting {
                h.fail();
            }
        }
    }
}
impl NativeOperation {
    pub(super) fn peer_custody(&self) -> Option<Arc<NativePeers>> {
        self.peers.as_ref().map(Arc::clone)
    }
    /// Called at the original exit owner BEFORE register restoration.
    pub(super) fn syscall_return(self, stopped: &Stopped, raw: i64) -> Option<NativeReturn> {
        let valid = stopped.syscall_exit_result().is_ok_and(|r| r == raw)
            && stopped
                .getregs()
                .is_ok_and(|r| r.orig_syscall() as i32 == self.syscall as i32);
        #[cfg(all(test, cohort_final_test, target_arch = "x86_64"))]
        clone3_tests::returned(&self.member, self.syscall, stopped, raw, valid);
        self.returned(stopped, raw, valid)
    }
    pub(super) fn private_return(self, stopped: &Stopped, raw: i64) -> Option<NativeReturn> {
        #[cfg(target_arch = "x86_64")]
        if self.receive_timer.is_some() {
            return None;
        }
        if self.syscall == Sysno::clone3 {
            // Indirect birth requires the actual typed EXIT, never a trap's RAX.
            return None;
        }
        let valid = super::is_expected_private_syscall_trap(
            stopped,
            (super::cp::PRIVATE_PAGE_OFFSET + super::cp::SYSCALL_INSTR_SIZE) as u64,
            false,
        )
        .unwrap_or(false)
            && stopped.getregs().is_ok_and(|r| {
                r.orig_syscall() as i32 == self.syscall as i32 && r.ret() as i64 == raw
            });
        self.returned(stopped, raw, valid)
    }
    fn returned(self, stopped: &Stopped, raw: i64, valid: bool) -> Option<NativeReturn> {
        let identity = stopped.terminal_cleanup().task_identity();
        let h = self.member.history.0.lock().unwrap();
        let valid = valid
            && self.peers.as_ref().is_none_or(|peers| peers.returned(&h))
            && !h.failed
            && identity.as_ref().is_ok_and(|id| {
                h.tasks
                    .get(&self.member.index)
                    .is_some_and(|t| t.identity.same_generation(id))
            });
        drop(h);
        valid.then_some(NativeReturn { owner: self, raw })
    }
    pub(super) fn child_returned(mut self) {
        let _change = self.member.history.changing();
        let mut h = self.member.history.0.lock().unwrap();
        if !h.failed {
            if let Some(task) = h.tasks.get_mut(&self.member.index)
                && !task.operations.contains_key(&self.number)
            {
                task.invocation = None;
                self.completed = true;
            } else {
                h.fail();
            }
        }
    }
}
impl Drop for NativeOperation {
    fn drop(&mut self) {
        #[cfg(target_arch = "x86_64")]
        if !self.completed
            && let Some(timer) = &self.receive_timer
        {
            timer.fail(&self.member.history);
        }
        let _change = self.member.history.changing();
        let mut h = self.member.history.0.lock().unwrap();
        if !self.completed
            && h.tasks
                .get(&self.member.index)
                .is_some_and(|t| t.life != Life::Exiting && t.operations.contains_key(&self.number))
        {
            // Cancellation/error/restart/exec supplies no completion. Original
            // physical custody is untouched; publication is permanently shut.
            h.fail();
        }
        drop(h);
        if !self.completed
            && let Some(peers) = &self.peers
        {
            peers.unknown();
        }
    }
}
impl NativeReturn {
    /// Restore a private helper's authenticated EXIT while its native debt
    /// remains present. No metadata lock spans ptrace. The old restoration and
    /// legacy source-history behavior are separate and unchanged.
    #[cfg(target_arch = "x86_64")]
    pub(super) fn restore_checked(
        mut self,
        stopped: &Stopped,
        expected_exit: &safeptrace::Regs,
        desired: &safeptrace::Regs,
    ) -> Result<RestoredNativeContext, TraceError> {
        use safeptrace::Errno;
        let _timer_change = self
            .owner
            .receive_timer
            .as_ref()
            .map(|timer| ChangeNotice(vec![Arc::clone(&timer.changed)]));
        let result = (|| {
            if let Some(timer) = &self.owner.receive_timer {
                let mut expected = timer.entered;
                expected.rax = 0;
                if !ControlStop::registers_equal(&expected, expected_exit) {
                    return Err(Errno::EPROTO.into());
                }
            }
            if (self.owner.receive_timer.is_some() && self.raw != 0)
                || matches!(self.raw, -512 | -513 | -514 | -516)
                || self.raw == -(libc::EINTR as i64)
                || self.owner.source_ioctl.is_some()
                || self.owner.peers.is_some()
                || expected_exit.orig_rax != self.owner.syscall as u64
                || expected_exit.rax as i64 != self.raw
                || desired.rax != expected_exit.rax
                || stopped.syscall_exit_result()? != self.raw
            {
                return Err(Errno::EPROTO.into());
            }
            let identity = stopped.terminal_cleanup().task_identity()?;
            let (stop, revision, control_revision) = {
                let _change = self.owner.member.history.changing();
                let mut h = self.owner.member.history.0.lock().unwrap();
                if !h.read_open() || h.hold.is_some() {
                    return Err(Errno::ESTALE.into());
                }
                let revision = h.revision;
                let task = h
                    .tasks
                    .get_mut(&self.owner.member.index)
                    .ok_or(Errno::ESTALE)?;
                if !task.identity.same_generation(&identity)
                    || !task.checked_return_phase(self.owner.number)
                {
                    return Err(Errno::ESTALE.into());
                }
                let stop = task.stop.take().ok_or(Errno::ESTALE)?;
                stop.validate_current()?;
                let control_revision = stop.control_revision();
                (stop, revision, control_revision)
            };
            let stop = stop.setregs_checked(expected_exit, desired)?;
            stop.validate_current()?;
            let issued_revision = stop.control_revision();
            if control_revision.checked_add(1) != Some(issued_revision) {
                return Err(Errno::ESTALE.into());
            }
            let _change = self.owner.member.history.changing();
            let mut h = self.owner.member.history.0.lock().unwrap();
            if !h.read_open() || h.hold.is_some() || h.revision != revision {
                return Err(Errno::ESTALE.into());
            }
            let next_revision = revision.checked_add(1).ok_or(Errno::EOVERFLOW)?;
            let task = h
                .tasks
                .get_mut(&self.owner.member.index)
                .ok_or(Errno::ESTALE)?;
            if !task.identity.same_generation(&identity)
                || !task.checked_return_phase(self.owner.number)
                || task.stop.is_some()
            {
                return Err(Errno::ESTALE.into());
            }
            stop.validate_current()?;
            if let Some(timer) = &self.owner.receive_timer {
                if !task
                    .receive_timer
                    .as_ref()
                    .is_some_and(|t| Arc::ptr_eq(t, timer))
                    || !task.operations[&self.owner.number]
                        .receive_timer
                        .as_ref()
                        .is_some_and(|t| Arc::ptr_eq(t, timer))
                    || *timer.progress.lock().unwrap() != ReceiveTimerProgress::PendingNative
                {
                    return Err(Errno::ESTALE.into());
                }
                *timer.progress.lock().unwrap() = ReceiveTimerProgress::PendingPublication {
                    control_revision: issued_revision,
                };
            }
            task.stop = Some(stop);
            task.operations.remove(&self.owner.number);
            task.invocation = None;
            h.revision = next_revision;
            self.owner.completed = true;
            Ok(RestoredNativeContext {
                member: self.owner.member.clone(),
                operation: self.owner.number,
                control_revision: issued_revision,
                timer_publication: self.owner.receive_timer.take(),
            })
        })();
        if result.is_err() {
            // A failed write/readback/owner join cannot erase its unknown native
            // effect. Close source observation; original stop/cleanup owners live.
            self.owner.member.history.fail();
        }
        result
    }

    pub(super) fn restored(mut self) {
        #[cfg(target_arch = "x86_64")]
        if self.owner.receive_timer.is_some() {
            return;
        }
        #[cfg(all(test, cohort_final_test, target_arch = "x86_64"))]
        if self
            .owner
            .peers
            .as_ref()
            .is_some_and(peer_tests::abandon_restoration)
        {
            return;
        }
        #[cfg(test)]
        if tests::abandon_completion() {
            return;
        }
        // Restart pseudo-errors leave continuation semantics unresolved.
        // Short counts, zero and final errno are genuine completed results.
        if matches!(self.raw, -512 | -513 | -514 | -516) {
            return;
        }
        let _change = self.owner.member.history.changing();
        let mut h = self.owner.member.history.0.lock().unwrap();
        if h.failed {
            return;
        }
        let Some(task) = h.tasks.get_mut(&self.owner.member.index) else {
            h.fail();
            return;
        };
        let Some(op) = task.operations.get(&self.owner.number) else {
            h.fail();
            return;
        };
        if self.owner.syscall == Sysno::clone3
            && (op.indirect_birth != Some(IndirectBirth::AwaitingChild)
                || !(-4095..=-1).contains(&self.raw)
                || self.raw == -(safeptrace::Errno::EINTR.into_raw() as i64))
        {
            // Positive/zero without an actual child, or an interrupted/unknown
            // result, never proves that the indirect birth had no effect.
            h.fail();
            return;
        }
        if op.effect == Effect::Native || (op.effect == Effect::Birth && self.raw < 0) {
            #[cfg(test)]
            tests::native_returned(self.owner.syscall, self.raw);
            #[cfg(test)]
            if tests::retain_native_mutant() {
                task.invocation = None;
                self.owner.completed = true;
                return;
            }
            if let Some(source) = self.owner.source_ioctl.take() {
                if self.raw != -(libc::ENOTTY as i64) {
                    h.fail();
                    return;
                }
                if !source.restored(self.raw) {
                    h.fail();
                    return;
                }
            }
            task.operations.remove(&self.owner.number);
            task.invocation = None;
            self.owner.completed = true;
            if let Some(peers) = &self.owner.peers {
                peers.restored();
            }
        } else {
            h.fail();
        }
    }
}
impl ChildMembership {
    pub(super) fn prepared(self, prepared: &PreparedNewborn) -> Member {
        let identity_stop = match prepared {
            PreparedNewborn::Live { child, .. } => Some((
                child.terminal_cleanup().task_identity(),
                child.control_stop().ok(),
            )),
            _ => None,
        };
        let _change = self.member.history.changing();
        let mut h = self.member.history.0.lock().unwrap();
        if !h.failed {
            if let Some(task) = h.tasks.get_mut(&self.member.index) {
                if let Origin::Child { custody, .. } = &mut task.origin {
                    *custody = ChildCustody::Constructed;
                }
                if let Some((identity, stop)) = identity_stop {
                    if identity
                        .as_ref()
                        .is_ok_and(|id| task.identity.same_generation(id))
                    {
                        task.stop = stop;
                        task.life = Life::Stopped;
                    } else {
                        h.fail();
                    }
                } else {
                    // InitialChildWait can already own a genuine terminal
                    // result. It does not supply this ledger's retirement
                    // acknowledgment; permanently disable, never guess it.
                    h.fail();
                }
            } else {
                h.fail();
            }
        }
        let active = !h.failed;
        drop(h);
        #[cfg(test)]
        if active {
            tests::prepared(&self.member, prepared);
        }
        #[cfg(not(test))]
        let _ = active;
        self.member
    }
}
impl ParentCompletion {
    pub(super) fn returned_and_restored(mut self, raw: i64) {
        let _change = self.member.history.changing();
        let mut h = self.member.history.0.lock().unwrap();
        if h.failed {
            return;
        }
        let Some(task) = h.tasks.get_mut(&self.member.index) else {
            h.fail();
            return;
        };
        if task.operations.get(&self.operation).is_some_and(|op| {
            op.effect == Effect::Birth
                && match op.indirect_birth {
                    None => true,
                    Some(IndirectBirth::Child(child)) => {
                        child == self.child && task.invocation == Some(self.operation) && raw > 0
                    }
                    Some(IndirectBirth::AwaitingChild) => false,
                }
        }) {
            task.operations.remove(&self.operation);
            self.completed = true;
            drop(h);
            #[cfg(test)]
            tests::parent_returned(&self.member, self.operation);
        } else {
            h.fail();
        }
    }
}
impl Drop for ParentCompletion {
    fn drop(&mut self) {
        if !self.completed {
            self.member.history.fail();
        }
    }
}
impl TerminalOperation {
    /// Only original final status AND retire_ordinary_terminal acknowledge it.
    pub(crate) fn completed(self) {
        let _change = self.0.member.history.changing();
        let mut h = self.0.member.history.0.lock().unwrap();
        if h.failed {
            return;
        }
        let Some(task) = h.tasks.get(&self.0.member.index) else {
            return;
        };
        let restored = matches!(
            task.origin,
            Origin::Child {
                custody: ChildCustody::Restored,
                ..
            }
        );
        if task
            .operations
            .values()
            .any(|op| !matches!(op.effect, Effect::Terminal | Effect::Execution))
        {
            h.fail();
            return;
        }
        #[cfg(test)]
        tests::terminal_completed(&self.0.member);
        #[cfg(test)]
        if tests::retain_task_mutant() {
            let task = h.tasks.get_mut(&self.0.member.index).unwrap();
            task.operations.clear();
            task.stop = None;
            task.life = Life::Terminal;
            return;
        }
        h.tasks.remove(&self.0.member.index);
        if let Some(debt) = h.retirements.get_mut(&self.0.member.index) {
            debt.terminal = true;
            // A live initial EXIT that skipped child context restoration is
            // still an unsupported startup path, even with real native death.
            debt.physical = restored;
        }
        h.advance();
    }
}
impl Drop for TerminalOperation {
    fn drop(&mut self) {
        let _change = self.0.member.history.changing();
        let mut h = self.0.member.history.0.lock().unwrap();
        if h.tasks
            .get(&self.0.member.index)
            .is_some_and(|task| task.operations.contains_key(&self.0.number))
        {
            // Dropping the original final-owner observer is not the earlier
            // ExitFuture handoff. An abandoned acknowledgment closes history.
            h.fail();
        }
    }
}
#[cfg(test)]
#[path = "source_cohort_tests.rs"]
pub(crate) mod tests;

#[cfg(test)]
#[path = "source_startup_tests.rs"]
pub(crate) mod startup_tests;

#[cfg(all(test, cohort_final_test))]
#[path = "source_final_tests.rs"]
pub(crate) mod final_tests;

#[cfg(test)]
#[path = "source_hold_tests.rs"]
pub(super) mod hold_tests;

#[cfg(all(test, cohort_final_test, target_arch = "x86_64"))]
#[path = "source_retirement_tests.rs"]
pub(super) mod retirement_tests;

#[cfg(all(test, cohort_final_test, target_arch = "x86_64"))]
#[path = "source_clone3_tests.rs"]
pub(super) mod clone3_tests;

#[cfg(all(test, cohort_final_test, target_arch = "x86_64"))]
#[path = "source_peer_tests.rs"]
pub(crate) mod peer_tests;

#[cfg(all(test, cohort_final_test, target_arch = "x86_64"))]
#[path = "followed_store_tests.rs"]
mod store_tests;

#[cfg(all(test, target_arch = "x86_64"))]
#[path = "source_restoration_tests.rs"]
mod restoration_tests;

#[cfg(all(test, cohort_final_test, target_arch = "x86_64"))]
#[path = "restored_receive_tests.rs"]
mod restored_receive_tests;

// A closed role inside the existing native operation. Observer cells have no
// Drop and own no physical stop, timer, wait or cleanup capability.
#[cfg(target_arch = "x86_64")]
struct ReceiveTimerObservation {
    member: u64,
    operation: u64,
    identity: Arc<TaskIdentity>,
    original: (Sysno, SyscallArgs),
    entered: safeptrace::Regs,
    scratch: Arc<crate::tracer::ReceiveTimerScratch>,
    progress: Mutex<ReceiveTimerProgress>,
    changed: Arc<Notify>,
}
#[cfg(target_arch = "x86_64")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ReceiveTimerProgress {
    PendingNative,
    PendingPublication { control_revision: u64 },
    Restored { control_revision: u64 },
    Failed,
}
#[cfg(target_arch = "x86_64")]
impl ReceiveTimerObservation {
    fn fail(&self, history: &CohortHistory) {
        let _change = ChangeNotice(vec![Arc::clone(&history.1), Arc::clone(&self.changed)]);
        let _h = history.0.lock().unwrap();
        *self.progress.lock().unwrap() = ReceiveTimerProgress::Failed;
    }
}
#[cfg(target_arch = "x86_64")]
impl Member {
    /// The borrowed origin can only be constructed inside the dedicated real
    /// receive timer path; a generic equal-shaped Ppoll never acquires this role.
    pub(super) fn native_receive_timer(
        &self,
        stopped: &Stopped,
        origin: super::followed_receive::TimerOrigin<'_>,
        timer: (Sysno, SyscallArgs),
        entered: &safeptrace::Regs,
    ) -> Result<NativeOperation, TraceError> {
        use safeptrace::Errno;
        origin.validate(stopped)?;
        super::original_context::check_entry(
            stopped,
            timer.0,
            timer.1,
            entered.rip,
            entered.rsp,
            false,
        )?;
        if timer.0 != Sysno::ppoll || !ControlStop::registers_equal(&stopped.getregs()?, entered) {
            return Err(Errno::EPROTO.into());
        }
        let identity = stopped.terminal_cleanup().task_identity()?;
        let mut native = self.native(timer.0, timer.1).ok_or(Errno::ESTALE)?;
        let _change = self.history.changing();
        let mut h = self.history.0.lock().unwrap();
        if !h.read_open() || h.hold.is_some() {
            return Err(Errno::ESTALE.into());
        }
        let task = h.tasks.get_mut(&self.index).ok_or(Errno::ESTALE)?;
        if !task.identity.same_generation(&identity)
            || task.life != Life::Stopped
            || task.invocation != Some(native.number)
            || task.operations.len() != 1
            || native.number.checked_add(1) != Some(task.next_operation)
            || task.receive_timer.as_ref().is_some_and(|old| {
                !matches!(
                    *old.progress.lock().unwrap(),
                    ReceiveTimerProgress::Restored { .. }
                )
            })
        {
            return Err(Errno::ESTALE.into());
        }
        let operation = task
            .operations
            .get_mut(&native.number)
            .ok_or(Errno::ESTALE)?;
        if operation.effect != Effect::Native
            || operation.outcome != Outcome::Waiting
            || operation.indirect_birth.is_some()
            || operation.peers.is_some()
            || operation.receive_timer.is_some()
        {
            return Err(Errno::ESTALE.into());
        }
        let observation = Arc::new(ReceiveTimerObservation {
            member: self.index,
            operation: native.number,
            identity: Arc::clone(&task.identity),
            original: origin.original(),
            entered: *entered,
            scratch: origin.scratch(),
            progress: Mutex::new(ReceiveTimerProgress::PendingNative),
            changed: Arc::new(Notify::new()),
        });
        operation.receive_timer = Some(Arc::clone(&observation));
        task.receive_timer = Some(Arc::clone(&observation));
        native.receive_timer = Some(observation);
        Ok(native)
    }

    pub(super) fn snapshot_receive_timers(&self) -> Result<ReceiveTimerJoin, safeptrace::Errno> {
        use safeptrace::Errno;
        let h = self.history.0.lock().unwrap();
        if !h.read_open() || h.hold.is_some() || !h.tasks.contains_key(&self.index) {
            return Err(Errno::ESTALE);
        }
        let mut members = BTreeMap::new();
        for (&index, task) in &h.tasks {
            let state = if task.quiescent() {
                let stop = task.stop.as_ref().ok_or(Errno::ESTALE)?;
                stop.validate_current()?;
                if let Some(timer) = &task.receive_timer {
                    let progress = *timer.progress.lock().unwrap();
                    if progress
                        != (ReceiveTimerProgress::Restored {
                            control_revision: stop.control_revision(),
                        })
                        || timer.operation.checked_add(1) != Some(task.next_operation)
                    {
                        return Err(Errno::ESTALE);
                    }
                }
                JoinMemberState::Stopped(stop.control_revision())
            } else {
                if index == self.index {
                    return Err(Errno::EBUSY);
                }
                let timer = task.receive_timer.as_ref().ok_or(Errno::EBUSY)?;
                if *timer.progress.lock().unwrap() != ReceiveTimerProgress::PendingNative {
                    return Err(Errno::EBUSY);
                }
                timer.validate_native(task)?;
                JoinMemberState::Timer(Arc::clone(timer))
            };
            members.insert(
                index,
                JoinMember {
                    identity: Arc::clone(&task.identity),
                    origin: task.origin,
                    next_operation: task.next_operation,
                    state,
                },
            );
        }
        Ok(ReceiveTimerJoin {
            member: self.clone(),
            revision: h.revision,
            next_task: h.next_task,
            members,
        })
    }
}

#[cfg(target_arch = "x86_64")]
impl ReceiveTimerObservation {
    fn validate_native(&self, task: &Task) -> Result<(), safeptrace::Errno> {
        use safeptrace::Errno;
        let operation = task.operations.get(&self.operation).ok_or(Errno::ESTALE)?;
        if !self.identity.same_generation(&task.identity)
            || self.operation.checked_add(1) != Some(task.next_operation)
            || task.invocation != Some(self.operation)
            || task.operations.len() != 1
            || operation.effect != Effect::Native
            || operation.indirect_birth.is_some()
            || operation.peers.is_some()
            || !operation
                .receive_timer
                .as_ref()
                .is_some_and(|t| std::ptr::eq(&**t, self))
            || !matches!(
                task.origin,
                Origin::Command
                    | Origin::Child {
                        custody: ChildCustody::Restored,
                        ..
                    }
            )
        {
            return Err(Errno::ESTALE);
        }
        match (task.life, operation.outcome) {
            (Life::Executing, Outcome::Waiting) if task.stop.is_none() => Ok(()),
            (Life::Stopped, Outcome::Stopped) => {
                // Checked SET/GET temporarily consumes Task.stop. There is no
                // await there; the real producer owns it until reattachment.
                if let Some(stop) = &task.stop {
                    stop.validate_current()?;
                }
                Ok(())
            }
            _ => Err(Errno::EBUSY),
        }
    }
}
#[cfg(target_arch = "x86_64")]
struct JoinMember {
    identity: Arc<TaskIdentity>,
    origin: Origin,
    next_operation: u64,
    state: JoinMemberState,
}
#[cfg(target_arch = "x86_64")]
enum JoinMemberState {
    Stopped(u64),
    Timer(Arc<ReceiveTimerObservation>),
}
#[cfg(target_arch = "x86_64")]
pub(super) struct ReceiveTimerJoin {
    member: Member,
    revision: u64,
    next_task: u64,
    members: BTreeMap<u64, JoinMember>,
}
#[cfg(target_arch = "x86_64")]
impl ReceiveTimerJoin {
    /// true means every originally captured timer published the checked
    /// positive boundary. Removed operations alone never satisfy this check.
    pub(super) fn status(&self) -> Result<bool, safeptrace::Errno> {
        use safeptrace::Errno;
        let h = self.member.history.0.lock().unwrap();
        if !h.read_open()
            || h.hold.is_some()
            || h.next_task != self.next_task
            || !h.tasks.keys().eq(self.members.keys())
        {
            return Err(Errno::ESTALE);
        }
        let mut restores = 0u64;
        let mut ready = true;
        for (&index, saved) in &self.members {
            let task = &h.tasks[&index];
            if !task.identity.same_generation(&saved.identity)
                || task.origin != saved.origin
                || task.next_operation != saved.next_operation
            {
                return Err(Errno::ESTALE);
            }
            let revision = match &saved.state {
                JoinMemberState::Stopped(revision) => *revision,
                JoinMemberState::Timer(timer) => {
                    if timer.member != index
                        || !task
                            .receive_timer
                            .as_ref()
                            .is_some_and(|t| Arc::ptr_eq(t, timer))
                    {
                        return Err(Errno::ESTALE);
                    }
                    let progress = *timer.progress.lock().unwrap();
                    match progress {
                        ReceiveTimerProgress::PendingNative => {
                            timer.validate_native(task)?;
                            ready = false;
                            continue;
                        }
                        ReceiveTimerProgress::PendingPublication { control_revision } => {
                            restores = restores.checked_add(1).ok_or(Errno::EOVERFLOW)?;
                            ready = false;
                            control_revision
                        }
                        ReceiveTimerProgress::Restored { control_revision } => {
                            restores = restores.checked_add(1).ok_or(Errno::EOVERFLOW)?;
                            control_revision
                        }
                        ReceiveTimerProgress::Failed => return Err(Errno::ESTALE),
                    }
                }
            };
            if !task.quiescent() {
                return Err(Errno::ESTALE);
            }
            let stop = task.stop.as_ref().ok_or(Errno::ESTALE)?;
            if stop.control_revision() != revision {
                return Err(Errno::ESTALE);
            }
            stop.validate_current()?;
        }
        if self.revision.checked_add(restores) != Some(h.revision) {
            return Err(Errno::ESTALE);
        }
        Ok(ready)
    }

    pub(super) async fn wait(&self) -> Result<(), safeptrace::Errno> {
        let mut notifications = vec![Arc::clone(&self.member.history.1)];
        for saved in self.members.values() {
            if let JoinMemberState::Timer(timer) = &saved.state {
                notifications.push(Arc::clone(&timer.changed));
            }
        }
        loop {
            // Enable before checking: a completion before polling the await
            // cannot be lost. No cohort/cell lock or ControlHold spans await.
            let mut waiting: Vec<_> = notifications
                .iter()
                .map(|n| Box::pin(n.notified()))
                .collect();
            for notified in &mut waiting {
                notified.as_mut().enable();
            }
            if self.status()? {
                return Ok(());
            }
            futures::future::select_all(waiting).await;
        }
    }
}

#[cfg(target_arch = "x86_64")]
impl RestoredNativeContext {
    pub(super) fn publish_receive_timer(
        &self,
        publication: super::followed_receive::TimerPublication<'_>,
    ) -> Result<(), safeptrace::Errno> {
        use safeptrace::Errno;
        publication.validate(self)?;
        let timer = self.timer_publication.as_ref().ok_or(Errno::ESTALE)?;
        let _change = ChangeNotice(vec![
            Arc::clone(&self.member.history.1),
            Arc::clone(&timer.changed),
        ]);
        let h = self.member.history.0.lock().unwrap();
        let task = h.tasks.get(&self.member.index).ok_or(Errno::ESTALE)?;
        if !h.read_open()
            || h.hold.is_some()
            || !task.quiescent()
            || self.operation != timer.operation
            || self.member.index != timer.member
            || timer.original != publication.original()
            || !Arc::ptr_eq(&timer.scratch, publication.scratch())
            || !task
                .receive_timer
                .as_ref()
                .is_some_and(|t| Arc::ptr_eq(t, timer))
            || *timer.progress.lock().unwrap()
                != (ReceiveTimerProgress::PendingPublication {
                    control_revision: self.control_revision,
                })
        {
            return Err(Errno::ESTALE);
        }
        *timer.progress.lock().unwrap() = ReceiveTimerProgress::Restored {
            control_revision: self.control_revision,
        };
        Ok(())
    }
}
#[cfg(target_arch = "x86_64")]
impl Drop for RestoredNativeContext {
    fn drop(&mut self) {
        if let Some(timer) = &self.timer_publication {
            let _change = ChangeNotice(vec![
                Arc::clone(&self.member.history.1),
                Arc::clone(&timer.changed),
            ]);
            let mut h = self.member.history.0.lock().unwrap();
            let mut progress = timer.progress.lock().unwrap();
            if !matches!(*progress, ReceiveTimerProgress::Restored { .. }) {
                *progress = ReceiveTimerProgress::Failed;
                h.fail();
            }
        }
    }
}

#[cfg(all(test, cohort_final_test, target_arch = "x86_64"))]
#[path = "receive_timer_join_tests.rs"]
pub(super) mod timer_join_tests;
