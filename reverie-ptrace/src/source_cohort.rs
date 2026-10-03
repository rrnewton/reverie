/* Copyright (c) Meta Platforms, Inc. and affiliates. All rights reserved.
 * Licensed under the BSD-style license in the root LICENSE file. */

//! Command membership and an inactive, already-stopped source-read boundary.
//! Original owners retain every physical effect. Failed observer metadata can
//! be discarded only irreversibly: this history can never enroll or issue again.
//! Locks cover metadata only, never ptrace, memory, callbacks or an await.
use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::Mutex;

use reverie::syscalls::SyscallArgs;
use reverie::syscalls::Sysno;
use safeptrace::ControlStop;
use safeptrace::Event;
use safeptrace::Stopped;
use safeptrace::TaskIdentity;
use safeptrace::TerminalCleanup;
use safeptrace::Wait;

use super::PreparedNewborn;
use super::TraceError;
use crate::regs::RegAccess;

#[derive(Default)]
pub(super) struct CohortHistory(Mutex<History>);
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
    identity: TaskIdentity,
    stop: Option<ControlStop>,
    origin: Origin,
    life: Life,
    next_operation: u64,
    operations: BTreeMap<u64, Operation>,
    invocation: Option<u64>,
}
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
}
pub(super) struct NativeReturn {
    owner: NativeOperation,
    raw: i64,
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
                identity,
                stop: None,
                origin,
                life: Life::Initializing,
                next_operation: 0,
                operations: BTreeMap::new(),
                invocation: None,
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
        task.next_operation = next;
        task.operations.insert(
            n,
            Operation {
                effect,
                outcome: Outcome::Waiting,
                indirect_birth: None,
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
        self.0.lock().unwrap().fail();
    }
    pub(super) fn initial_command(self: &Arc<Self>, stopped: &Stopped) -> Option<Member> {
        let identity = stopped.terminal_cleanup().task_identity().ok()?;
        let stop = stopped.control_stop().ok()?;
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
}

impl Member {
    /// Only the original ordinary child spawn installs this single observer.
    pub(super) fn ordinary_child(&self, terminal: Arc<TerminalCleanup>) -> Option<ChildRetirement> {
        let identity = terminal.task_identity().ok()?;
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
        let mut h = self.history.0.lock().unwrap();
        if !h.read_open() || !h.tasks.contains_key(&self.index) {
            return Err(Errno::ESTALE);
        }
        if h.hold.is_some() {
            return Err(Errno::EBUSY);
        }
        let mut controls = BTreeMap::new();
        for (&id, task) in &h.tasks {
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
        })
    }
}

impl Task {
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
        // Release physical gates before clearing the run-level signal fence.
        self.controls.clear();
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
        let (mut effect, exposure) = match stopped.pending_syscall_entry() {
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
        h.advance();
        let invocation = h.tasks.get(&self.index)?.invocation;
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
    }
}
impl NativeReturn {
    pub(super) fn restored(mut self) {
        #[cfg(test)]
        if tests::abandon_completion() {
            return;
        }
        // Restart pseudo-errors leave continuation semantics unresolved.
        // Short counts, zero and final errno are genuine completed results.
        if matches!(self.raw, -512 | -513 | -514 | -516) {
            return;
        }
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
