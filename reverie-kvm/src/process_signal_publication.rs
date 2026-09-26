/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! Run-scoped callback-independent process publication and recipient permits.
//!
//! Installation precedes guest startup. A permit authorizes one actual task
//! continuation, not a host wake or an unrelated borrowed Guest. Generation-
//! bound child completion is published through this same run-scoped control.
//!
//! Active publication uses only transaction -> lifecycle -> run failure ->
//! process signals and pinned private eventfd carriers. It never acquires or
//! pins the ordinary file table. The retained inactive alarm test endpoint
//! additionally takes file table first, preserving its negative controls.
//! Image validation briefly reacquires image below transaction;
//! child lookup briefly reacquires registry below lifecycle. Those guards are
//! released before failure/process acquisition. The run failure guard remains
//! held through readiness I/O after process/lifecycle guards are released,
//! serializing publications across
//! processes in this run. Existing executor process/thread locks remain below
//! the transaction. No registry or retirement mutex is held during host closes.

use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::Weak;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::AtomicI32;
use std::sync::atomic::Ordering;
use std::sync::mpsc::Sender;

use reverie::SignalEvent;
use reverie::SignalProcessId;
use reverie::syscalls::Errno;

use super::FileTableState;
use super::LoadedStaticElf;
use super::set_signalfd_ready;
use crate::elf::TaskLifecycleTable;
use crate::signal::ProcessSignalState;
use crate::vm::CONTAINER_INIT_PID;

type ProcessKey = (i32, u64);

fn process_key(process: SignalProcessId) -> ProcessKey {
    (process.tgid.as_raw(), process.generation)
}

fn process_identity((tgid, generation): ProcessKey) -> SignalProcessId {
    SignalProcessId {
        tgid: reverie::Pid::from_raw(tgid),
        generation,
    }
}

#[derive(Clone, Debug)]
struct ImageRevision(Arc<()>);

impl PartialEq for ImageRevision {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

impl Eq for ImageRevision {}

#[derive(Clone)]
struct CurrentImage {
    revision: ImageRevision,
    files: Weak<Mutex<FileTableState>>,
    signals: Weak<Mutex<ProcessSignalState>>,
}

/// Executors own this binding; the run registry never owns a process or image.
/// Threads share it, fork registers a fresh binding, and exec replaces `image`.
pub(super) struct ProcessBinding {
    identity: SignalProcessId,
    parent: Option<SignalProcessId>,
    transaction: Weak<Mutex<()>>,
    lifecycle: Weak<Mutex<TaskLifecycleTable>>,
    image: Mutex<CurrentImage>,
    // The process-wide guest-visible parent cell shared by every thread and
    // exec image; an exiting parent stores the reaper here when it orphans us.
    reparented_ppid: Arc<AtomicI32>,
}

impl ProcessBinding {
    pub(super) fn rebind(&self, state: &LoadedStaticElf, files: &Arc<Mutex<FileTableState>>) {
        // Caller holds the authoritative file table and process transaction.
        *self.image.lock().unwrap_or_else(|p| p.into_inner()) = CurrentImage {
            revision: ImageRevision(Arc::new(())),
            files: Arc::downgrade(files),
            signals: Arc::downgrade(&state.process_signals),
        };
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DirectChildState {
    Live,
    WaitableZombie,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ChildExitSnapshot {
    pub(crate) completion: reverie::ChildExitCompletion,
    disposition: PublicationDisposition,
    pending_generation: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ProcessFamilyExit {
    Root,
    Child(ChildExitSnapshot),
    Failed,
    /// No live guest process can reap this exit: its current parent was
    /// already terminal, so the namespace init outside the guest (or run
    /// teardown, once the traced root is gone) consumes it without a
    /// guest-visible SIGCHLD or wait.
    RunTeardownChild {
        status: reverie::ExitStatus,
    },
    /// The exiting process leaves an unreaped zombie child that Linux would
    /// hand to a live guest init (the traced root at PID 1) together with a
    /// second SIGCHLD. That re-notification is not implemented.
    ZombieAdoptionUnsupported {
        child: SignalProcessId,
        reaper: SignalProcessId,
    },
    ParentGenerationUnavailable {
        parent: SignalProcessId,
    },
    ParentChildRelationUnavailable {
        parent: SignalProcessId,
    },
    AncestryCycle {
        ancestor: SignalProcessId,
    },
    MultipleParents {
        child: SignalProcessId,
        first_parent: SignalProcessId,
        second_parent: SignalProcessId,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ProcessFamilyAncestryError {
    Cycle {
        ancestor: ProcessKey,
    },
    MultipleParents {
        child: ProcessKey,
        first_parent: ProcessKey,
        second_parent: ProcessKey,
    },
}

/// Collection state of an orphan adopted by a live guest init. Its host
/// thread and fork-time completion slot stay with the dead fork parent, so the
/// adopter's wait observes it here instead.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum AdoptedChild {
    Running,
    Waitable(reverie::ExitStatus),
    Failed,
}

/// What an adopting init's wait may collect from its adopted orphans.
#[derive(Debug, Default, Eq, PartialEq)]
pub(super) struct AdoptedChildPoll {
    /// Waitable exits, removed from the ledger and owned by the caller.
    pub(super) waitable: Vec<(i32, reverie::ExitStatus)>,
    /// A matching adopted orphan has not yet published its exit.
    pub(super) running: bool,
    /// A matching adopted orphan failed before publishing its exit.
    pub(super) failed: Option<i32>,
}

#[derive(Default)]
struct ProcessFamilyState {
    direct_children: BTreeMap<ProcessKey, BTreeMap<ProcessKey, DirectChildState>>,
    terminal: BTreeMap<ProcessKey, ProcessFamilyExit>,
    // The traced root; when it is PID 1 it is the PID-namespace init.
    root: Option<ProcessKey>,
    // Current parent of each orphan adopted by the guest init. Absent means
    // the immutable fork-time parent. Retained after exit so the Tool's later
    // publication validates against the adopter.
    adopted_parent: BTreeMap<ProcessKey, ProcessKey>,
    // Adopter -> adopted orphan -> collection state.
    adoptions: BTreeMap<ProcessKey, BTreeMap<ProcessKey, AdoptedChild>>,
    // Adopter -> wake channels of its executors blocked on a running adopted
    // orphan. An orphan's host thread belongs to its dead fork parent, so its
    // completion cannot use the adopter's fork-time notifier. Registered and
    // drained under this lock together with `adoptions`, so a wake is never
    // lost and the list holds only currently blocked waiters.
    adoption_waiters: BTreeMap<ProcessKey, Vec<Sender<i32>>>,
}

/// Wake every executor that was blocked on an adopted orphan. Each
/// registration is one-shot; a woken waiter re-polls and re-registers.
fn wake_adoption_waiters(waiters: Vec<Sender<i32>>, child_pid: i32) {
    for waiter in waiters {
        // A retired executor has dropped its receiver; nothing to wake.
        let _ = waiter.send(child_pid);
    }
}

impl ProcessFamilyState {
    fn take_adoption_waiters(&mut self, adopter: ProcessKey) -> Vec<Sender<i32>> {
        self.adoption_waiters.remove(&adopter).unwrap_or_default()
    }

    fn current_parent(
        &self,
        process: ProcessKey,
        fork_parent: Option<SignalProcessId>,
    ) -> Option<ProcessKey> {
        self.adopted_parent
            .get(&process)
            .copied()
            .or(fork_parent.map(process_key))
    }

    /// Linux hands orphans to the nearest child subreaper, else the
    /// PID-namespace init. KVM has no subreaper (PR_SET_CHILD_SUBREAPER is
    /// refused), so the reaper is namespace init: the traced root when it is
    /// PID 1 and still live. Otherwise init is outside the guest (Hermit's
    /// container init) and `None` is returned.
    fn guest_reaper(&self, exiting: ProcessKey) -> Option<ProcessKey> {
        self.root.filter(|root| {
            root.0 == CONTAINER_INIT_PID && *root != exiting && !self.terminal.contains_key(root)
        })
    }

    /// Retire every edge of an exiting parent at its logical exit. Returns the
    /// orphans still running, whose guest-visible parent becomes namespace
    /// init. A caller has already refused a zombie that a guest init would
    /// have to adopt; an external init reaps zombies without guest effect.
    fn reparent_orphans(&mut self, exiting: ProcessKey) -> Vec<ProcessKey> {
        let Some(children) = self.direct_children.remove(&exiting) else {
            return Vec::new();
        };
        let reaper = self.guest_reaper(exiting);
        let mut orphans = Vec::new();
        for (child, state) in children {
            match state {
                DirectChildState::Live => {
                    if let Some(reaper) = reaper {
                        self.direct_children
                            .entry(reaper)
                            .or_default()
                            .insert(child, DirectChildState::Live);
                        self.adopted_parent.insert(child, reaper);
                        self.adoptions
                            .entry(reaper)
                            .or_default()
                            .insert(child, AdoptedChild::Running);
                    }
                    orphans.push(child);
                }
                DirectChildState::WaitableZombie => {
                    debug_assert!(reaper.is_none(), "guest init zombie adoption was refused");
                }
            }
        }
        orphans
    }

    fn has_terminal_ancestor(
        &self,
        mut ancestor: ProcessKey,
    ) -> Result<bool, ProcessFamilyAncestryError> {
        let mut visited = BTreeSet::new();
        loop {
            if !visited.insert(ancestor) {
                return Err(ProcessFamilyAncestryError::Cycle { ancestor });
            }
            if self.terminal.contains_key(&ancestor) {
                return Ok(true);
            }
            let mut parents = self
                .direct_children
                .iter()
                .filter(|(_, children)| children.contains_key(&ancestor))
                .map(|(parent, _)| *parent);
            let Some(parent) = parents.next() else {
                return Ok(false);
            };
            if let Some(second_parent) = parents.next() {
                return Err(ProcessFamilyAncestryError::MultipleParents {
                    child: ancestor,
                    first_parent: parent,
                    second_parent,
                });
            }
            ancestor = parent;
        }
    }
}

#[derive(Default)]
pub(super) struct ProcessSignalRegistry {
    processes: Mutex<BTreeMap<(i32, u64), Weak<ProcessBinding>>>,
    // Logical process ancestry is independent of host join-handle placement.
    // It is retained by exact generation until a wait consumes a zombie or an
    // exit-time auto-reap decision removes it. An exiting parent hands its
    // running children to namespace init here (see `reparent_orphans`).
    family: Mutex<ProcessFamilyState>,
    // Run-scoped at-most-once admission. Standard-signal coalescing is not an
    // operation ledger: after dequeue, the same child could otherwise enqueue
    // a second SIGCHLD. Retain exact generations until the run ends.
    child_publications: Mutex<BTreeMap<(i32, u64), ChildPublicationRecord>>,
    // No callbacks or G references. An eventual owner must make its own run
    // terminal after saving FailedAfterCommit. This latch refuses further
    // publication; it is not a substitute for that owner transition.
    failure: Mutex<Option<PublicationFailure>>,
    controlled: AtomicBool,
    permits: Mutex<BTreeMap<(i32, u64, i32, u64), RegisteredPermit>>,
    completed_permits: Mutex<BTreeMap<(i32, u64, i32, u64), reverie::SignalDeliveryPermit>>,
    run_failure: Mutex<Weak<crate::failure::RunFailure>>,
    reported_failure: Mutex<Option<crate::Error>>,
    failure_forwarded: AtomicBool,
}

impl ProcessSignalRegistry {
    pub(super) fn register(
        &self,
        state: &LoadedStaticElf,
        files: &Arc<Mutex<FileTableState>>,
        generation: u64,
        parent: Option<SignalProcessId>,
    ) -> Result<Arc<ProcessBinding>, SignalProcessId> {
        let identity = SignalProcessId {
            tgid: reverie::Pid::from_raw(state.pid),
            generation,
        };
        // Successful exec and same-process executor reconstruction retain the
        // process generation; they are not a fork edge and cannot make a
        // process its own child.
        let parent = parent.filter(|candidate| *candidate != identity);
        let binding = Arc::new(ProcessBinding {
            identity,
            parent,
            transaction: Arc::downgrade(&state.signal_transaction),
            lifecycle: Arc::downgrade(&state.task_lifecycle),
            image: Mutex::new(CurrentImage {
                revision: ImageRevision(Arc::new(())),
                files: Arc::downgrade(files),
                signals: Arc::downgrade(&state.process_signals),
            }),
            reparented_ppid: state.reparented_ppid.clone(),
        });
        if parent.is_none() {
            let mut family = self.family.lock().unwrap_or_else(|p| p.into_inner());
            family.root.get_or_insert(process_key(identity));
        }
        if let Some(parent) = parent {
            let mut family = self.family.lock().unwrap_or_else(|p| p.into_inner());
            let parent_key = process_key(parent);
            if family.terminal.contains_key(&parent_key) {
                return Err(parent);
            }
            let previous = family
                .direct_children
                .entry(parent_key)
                .or_default()
                .insert(process_key(binding.identity), DirectChildState::Live);
            debug_assert!(previous.is_none(), "duplicate KVM child process generation");
        }
        let mut processes = self.processes.lock().unwrap_or_else(|p| p.into_inner());
        processes.retain(|_, process| process.strong_count() != 0);
        processes.insert((state.pid, generation), Arc::downgrade(&binding));
        Ok(binding)
    }

    fn lookup(&self, identity: SignalProcessId) -> Option<Arc<ProcessBinding>> {
        self.processes
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get(&(identity.tgid.as_raw(), identity.generation))?
            .upgrade()
    }

    /// Freeze the exact process generation at its first exact task failure.
    /// Descendants may still finish successfully while the runtime unwinds,
    /// but their completion is teardown rather than a new logical child-exit
    /// publication to the failed parent.
    pub(super) fn record_process_failure(&self, process: SignalProcessId) {
        let fork_parent = self.lookup(process).and_then(|binding| binding.parent);
        let mut family = self.family.lock().unwrap_or_else(|p| p.into_inner());
        let process_family_key = process_key(process);
        let replace_success = matches!(
            family.terminal.get(&process_family_key),
            None | Some(
                ProcessFamilyExit::Root
                    | ProcessFamilyExit::Child(_)
                    | ProcessFamilyExit::RunTeardownChild { .. }
            )
        );
        if !replace_success {
            // Preserve the first typed fatal family invariant instead of
            // replacing its actionable cause with a generic peer failure.
            return;
        }
        family
            .terminal
            .insert(process_family_key, ProcessFamilyExit::Failed);
        let mut waiters = Vec::new();
        if let Some(parent_key) = family.current_parent(process_family_key, fork_parent) {
            if let Some(children) = family.direct_children.get_mut(&parent_key) {
                children.remove(&process_family_key);
                if children.is_empty() {
                    family.direct_children.remove(&parent_key);
                }
            }
            // An adopting init may be blocked waiting for this orphan. Fail
            // that wait rather than leave it asleep on a status never coming.
            if let Some(state) = family
                .adoptions
                .get_mut(&parent_key)
                .and_then(|adopted| adopted.get_mut(&process_family_key))
            {
                *state = AdoptedChild::Failed;
                waiters = family.take_adoption_waiters(parent_key);
            }
        }
        drop(family);
        wake_adoption_waiters(waiters, process.tgid.as_raw());
    }

    /// Freeze one process's terminal parent/wait policy at the authoritative
    /// lifecycle transition. Host join completion and the later Tool callback
    /// only consume this snapshot; they cannot resample a parent's newer
    /// SIGCHLD disposition.
    pub(super) fn record_process_exit(
        &self,
        process: SignalProcessId,
        status: reverie::ExitStatus,
    ) -> ProcessFamilyExit {
        if let Some(exit) = self
            .family
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .terminal
            .get(&process_key(process))
            .copied()
        {
            return exit;
        }

        let binding = self.lookup(process);
        let fork_parent = binding.as_ref().and_then(|binding| binding.parent);
        let is_root = fork_parent.is_none();
        let key = process_key(process);
        let mut expected_parent = self
            .family
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .current_parent(key, fork_parent);
        loop {
            // The current parent's transaction orders this exit against that
            // parent's own exit. Adoption only moves an edge while the fork
            // parent holds its transaction, so after it is taken the current
            // parent is stable unless the adoption committed first; retry then
            // under the adopter's transaction.
            let parent_identity = expected_parent.map(process_identity);
            let parent_binding = parent_identity.and_then(|parent| self.lookup(parent));
            let parent_transaction = parent_binding
                .as_ref()
                .and_then(|binding| binding.transaction.upgrade());
            let parent_transaction_guard = parent_transaction
                .as_ref()
                .map(|transaction| transaction.lock().unwrap_or_else(|p| p.into_inner()));
            let parent_snapshot = parent_binding.as_ref().and_then(|parent_binding| {
                let parent = parent_identity?;
                let lifecycle = parent_binding.lifecycle.upgrade()?;
                let lifecycle = lifecycle.lock().unwrap_or_else(|p| p.into_inner());
                if !lifecycle.contains_process(parent.tgid.as_raw(), parent.generation) {
                    return None;
                }
                let image = parent_binding
                    .image
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .clone();
                let signals = image.signals.upgrade()?;
                let signals = signals.lock().unwrap_or_else(|p| p.into_inner());
                let action = signals
                    .dispositions
                    .get(&libc::SIGCHLD)
                    .copied()
                    .unwrap_or_default();
                let disposition = if action.is_ignored() {
                    PublicationDisposition::Ignored
                } else if action.handler == libc::SIG_DFL as u64 {
                    PublicationDisposition::Default
                } else {
                    PublicationDisposition::Caught
                };
                let waitable =
                    !action.is_ignored() && action.flags & libc::SA_NOCLDWAIT as u64 == 0;
                let pending_generation = signals.pending_generation(libc::SIGCHLD);
                Some((parent, disposition, waitable, pending_generation))
            });

            let mut family = self.family.lock().unwrap_or_else(|p| p.into_inner());
            if let Some(exit) = family.terminal.get(&key).copied() {
                return exit;
            }
            let current_parent = family.current_parent(key, fork_parent);
            if current_parent != expected_parent {
                expected_parent = current_parent;
                drop(family);
                drop(parent_transaction_guard);
                continue;
            }
            let (exit, orphans) = self.decide_process_exit(
                &mut family,
                process,
                status,
                is_root,
                parent_identity,
                parent_snapshot,
            );
            family.terminal.insert(key, exit);
            drop(family);
            // Publish each running orphan's new guest-visible parent while this
            // process's transaction is still held by `take_exit`, before its
            // Tool exit callback can release the scheduler turn. A later
            // getppid in the orphan therefore observes the reaper.
            for orphan in orphans {
                if let Some(orphan) = self.lookup(process_identity(orphan)) {
                    orphan
                        .reparented_ppid
                        .store(CONTAINER_INIT_PID, Ordering::Release);
                }
            }
            return exit;
        }
    }

    // AUTONOMOUS-BOT-IMPLEMENTED
    // TODO-HUMAN-REVIEW(PR-653): Review KVM orphan reparenting
    // at the logical exit transition.
    /// Decide one process's terminal family transition under the family lock
    /// and its current parent's transaction. Returns the exit together with
    /// the running orphans whose guest-visible parent must become init.
    fn decide_process_exit(
        &self,
        family: &mut ProcessFamilyState,
        process: SignalProcessId,
        status: reverie::ExitStatus,
        is_root: bool,
        parent_identity: Option<SignalProcessId>,
        parent_snapshot: Option<(SignalProcessId, PublicationDisposition, bool, u64)>,
    ) -> (ProcessFamilyExit, Vec<ProcessKey>) {
        let key = process_key(process);
        let parent_is_terminal = match parent_identity {
            Some(parent) => match family.has_terminal_ancestor(process_key(parent)) {
                Ok(_) => family.terminal.contains_key(&process_key(parent)),
                Err(ProcessFamilyAncestryError::Cycle { ancestor }) => {
                    let exit = ProcessFamilyExit::AncestryCycle {
                        ancestor: process_identity(ancestor),
                    };
                    return (exit, Vec::new());
                }
                Err(ProcessFamilyAncestryError::MultipleParents {
                    child,
                    first_parent,
                    second_parent,
                }) => {
                    let exit = ProcessFamilyExit::MultipleParents {
                        child: process_identity(child),
                        first_parent: process_identity(first_parent),
                        second_parent: process_identity(second_parent),
                    };
                    return (exit, Vec::new());
                }
            },
            None => false,
        };
        // A live guest init would adopt an unreaped zombie together with a
        // second SIGCHLD; refuse that one unimplemented shape before any edge
        // moves. With init outside the guest the zombie is reaped invisibly.
        if let Some(reaper) = family.guest_reaper(key)
            && let Some(child) = family.direct_children.get(&key).and_then(|children| {
                children.iter().find_map(|(child, state)| {
                    (*state == DirectChildState::WaitableZombie).then_some(*child)
                })
            })
        {
            let exit = ProcessFamilyExit::ZombieAdoptionUnsupported {
                child: process_identity(child),
                reaper: process_identity(reaper),
            };
            return (exit, Vec::new());
        }
        let exit = if is_root {
            ProcessFamilyExit::Root
        } else if parent_is_terminal {
            // The direct parent's logical transition, rather than a transitive
            // ancestor's host teardown, decides whether this child still has a
            // live reaper. Both exits serialize through the direct parent's
            // transaction: child-first retains normal waitability, while
            // parent-first is consuming run teardown. A terminal root alone
            // cannot silently auto-reap a grandchild from its still-live parent.
            if let Some(parent) = parent_identity {
                let parent_key = process_key(parent);
                if let Some(children) = family.direct_children.get_mut(&parent_key) {
                    children.remove(&key);
                    if children.is_empty() {
                        family.direct_children.remove(&parent_key);
                    }
                }
            }
            ProcessFamilyExit::RunTeardownChild { status }
        } else if let Some((parent, disposition, waitable, pending_generation)) = parent_snapshot {
            let completion = reverie::ChildExitCompletion {
                parent,
                child: process,
                status,
                waitable,
                uid: 0,
                user_ticks: 0,
                system_ticks: 0,
            };
            let parent_key = process_key(parent);
            let relation_exists = family
                .direct_children
                .get(&parent_key)
                .is_some_and(|children| children.contains_key(&key));
            if !relation_exists {
                return (
                    ProcessFamilyExit::ParentChildRelationUnavailable { parent },
                    Vec::new(),
                );
            }
            if waitable {
                let relation = family
                    .direct_children
                    .get_mut(&parent_key)
                    .and_then(|children| children.get_mut(&key))
                    .expect("validated KVM direct-child relation remains present");
                *relation = DirectChildState::WaitableZombie;
            } else {
                let children = family
                    .direct_children
                    .get_mut(&parent_key)
                    .expect("validated KVM parent relation remains present");
                children.remove(&key);
                if children.is_empty() {
                    family.direct_children.remove(&parent_key);
                }
            }
            ProcessFamilyExit::Child(ChildExitSnapshot {
                completion,
                disposition,
                pending_generation,
            })
        } else {
            return (
                ProcessFamilyExit::ParentGenerationUnavailable {
                    parent: parent_identity
                        .expect("non-root KVM process retains a parent identity"),
                },
                Vec::new(),
            );
        };
        // Linux exit_notify: every child of the exiting process is handed to
        // its reaper at this same transition. No process that is itself
        // terminal can later reap them, so retire the exact edges here.
        let orphans = family.reparent_orphans(key);
        if is_root {
            // Nothing in the guest remains to collect orphans this root
            // adopted; they are consumed by run teardown.
            family.adoptions.remove(&key);
            family.adoption_waiters.remove(&key);
        }
        (exit, orphans)
    }

    pub(super) fn process_family_exit(
        &self,
        process: SignalProcessId,
    ) -> Option<ProcessFamilyExit> {
        self.family
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .terminal
            .get(&process_key(process))
            .copied()
    }

    /// Current logical parent of `process`: the adopting init for an adopted
    /// orphan, else the immutable fork-time parent.
    #[cfg(test)]
    pub(super) fn current_parent(&self, process: SignalProcessId) -> Option<SignalProcessId> {
        let fork_parent = self.lookup(process).and_then(|binding| binding.parent);
        self.family
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .current_parent(process_key(process), fork_parent)
            .map(process_identity)
    }

    /// Make an adopted orphan's authoritative completion collectable by its
    /// adopter, at the same post-fence point where a fork child's completion
    /// becomes collectable by its fork parent. `waitable == None` is an
    /// auto-reap under the adopter's SIG_IGN/SA_NOCLDWAIT policy. Returns
    /// false when `child` is not adopted by a live `adopter`.
    pub(super) fn publish_adopted_child_completion(
        &self,
        adopter: SignalProcessId,
        child: SignalProcessId,
        waitable: Option<reverie::ExitStatus>,
    ) -> bool {
        let mut family = self.family.lock().unwrap_or_else(|p| p.into_inner());
        let adopter_key = process_key(adopter);
        let child_key = process_key(child);
        let Some(adopted) = family.adoptions.get_mut(&adopter_key) else {
            return false;
        };
        if adopted.get(&child_key) != Some(&AdoptedChild::Running) {
            return false;
        }
        match waitable {
            Some(status) => {
                adopted.insert(child_key, AdoptedChild::Waitable(status));
            }
            None => {
                adopted.remove(&child_key);
                if adopted.is_empty() {
                    family.adoptions.remove(&adopter_key);
                }
            }
        }
        let waiters = family.take_adoption_waiters(adopter_key);
        drop(family);
        wake_adoption_waiters(waiters, child.tgid.as_raw());
        true
    }

    /// Collect the adopted orphans of `adopter` that match a wait's pid
    /// selector. Waitable exits are moved out to the caller, which records
    /// them exactly like joined fork children.
    /// A caller that will block on a running result passes its wake channel;
    /// it is registered atomically with the observation, so a completion
    /// published afterwards always wakes it.
    pub(super) fn poll_adopted_children(
        &self,
        adopter: SignalProcessId,
        matches: impl Fn(i32) -> bool,
        waiter: Option<&Sender<i32>>,
    ) -> AdoptedChildPoll {
        let mut family = self.family.lock().unwrap_or_else(|p| p.into_inner());
        let adopter_key = process_key(adopter);
        let mut poll = AdoptedChildPoll::default();
        let Some(adopted) = family.adoptions.get_mut(&adopter_key) else {
            return poll;
        };
        adopted.retain(|child, state| {
            if !matches(child.0) {
                return true;
            }
            match *state {
                AdoptedChild::Running => {
                    poll.running = true;
                    true
                }
                AdoptedChild::Waitable(status) => {
                    poll.waitable.push((child.0, status));
                    false
                }
                AdoptedChild::Failed => {
                    poll.failed.get_or_insert(child.0);
                    true
                }
            }
        });
        if adopted.is_empty() {
            family.adoptions.remove(&adopter_key);
        }
        if poll.running
            && poll.waitable.is_empty()
            && poll.failed.is_none()
            && let Some(waiter) = waiter
        {
            family
                .adoption_waiters
                .entry(adopter_key)
                .or_default()
                .push(waiter.clone());
        }
        poll
    }

    pub(super) fn consume_child_wait(&self, parent: SignalProcessId, child_pid: i32) -> bool {
        let mut family = self.family.lock().unwrap_or_else(|p| p.into_inner());
        let parent_key = process_key(parent);
        let Some(children) = family.direct_children.get_mut(&parent_key) else {
            return false;
        };
        let child = children.iter().find_map(|(key, state)| {
            (key.0 == child_pid && *state == DirectChildState::WaitableZombie).then_some(*key)
        });
        let Some(child) = child else {
            return false;
        };
        children.remove(&child);
        if children.is_empty() {
            family.direct_children.remove(&parent_key);
        }
        true
    }

    pub(super) fn control(self: &Arc<Self>) -> ProcessSignalControl {
        ProcessSignalControl(Arc::downgrade(self))
    }
}

/// Retaining a control does not retain executors, descriptors, or GlobalState.
pub(super) struct ProcessSignalControl(Weak<ProcessSignalRegistry>);

impl std::fmt::Debug for ProcessSignalControl {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProcessSignalControl")
            .finish_non_exhaustive()
    }
}

#[derive(Clone)]
struct RegisteredPermit {
    permit: reverie::SignalDeliveryPermit,
    image: ImageRevision,
}

fn task_key(task: reverie::SignalTaskIdentity) -> (i32, u64, i32, u64) {
    (
        task.process.tgid.as_raw(),
        task.process.generation,
        task.tid.as_raw(),
        task.task_generation,
    )
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum PublicationRejection {
    Closed,
    StaleProcess,
    ChangedImage,
    InvalidEvent,
    InvalidCompletion,
    Backend(Errno),
    Terminal,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum PublicationDisposition {
    Ignored,
    Default,
    Caught,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum PendingChange {
    Queued,
    Coalesced,
    Suppressed,
    Discarded,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct PublicationReceipt {
    process: SignalProcessId,
    image: ImageRevision,
    signal: i32,
    pending_generation: u64,
    change: PendingChange,
    disposition: PublicationDisposition,
    child_completion: Option<reverie::ChildExitCompletion>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct PublicationFailure {
    receipt: PublicationReceipt,
    errno: Errno,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum ProcessPublication {
    Rejected(PublicationRejection),
    Committed(PublicationReceipt),
    FailedAfterCommit(PublicationFailure),
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum ChildPublicationRecord {
    Committed(PublicationReceipt),
    Failed(PublicationFailure),
}

impl ChildPublicationRecord {
    fn completion(&self) -> reverie::ChildExitCompletion {
        match self {
            Self::Committed(receipt) => receipt,
            Self::Failed(failure) => &failure.receipt,
        }
        .child_completion
        .expect("child ledger contains a child completion")
    }

    fn replay(&self) -> ProcessPublication {
        match self {
            Self::Committed(receipt) => ProcessPublication::Committed(receipt.clone()),
            Self::Failed(failure) => ProcessPublication::FailedAfterCommit(failure.clone()),
        }
    }
}

impl ProcessSignalControl {
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "retained inactive private endpoint; active facade uses independent carriers"
        )
    )]
    pub(super) fn publish_alarm(
        &self,
        target: SignalProcessId,
        event: SignalEvent,
    ) -> ProcessPublication {
        let mut expected = [0; reverie::SIGNAL_INFO_SIZE];
        expected[..4].copy_from_slice(&libc::SIGALRM.to_ne_bytes());
        expected[8..12].copy_from_slice(&libc::SI_KERNEL.to_ne_bytes());
        if event.signal() != libc::SIGALRM
            || event.siginfo() != expected
            || event.target() != (reverie::SignalTarget::Process { pid: target.tgid })
        {
            return ProcessPublication::Rejected(PublicationRejection::InvalidEvent);
        }
        self.publish(target, event, None, false)
    }

    fn publish_active_alarm(
        &self,
        target: SignalProcessId,
        event: SignalEvent,
    ) -> ProcessPublication {
        let mut expected = [0; reverie::SIGNAL_INFO_SIZE];
        expected[..4].copy_from_slice(&libc::SIGALRM.to_ne_bytes());
        expected[8..12].copy_from_slice(&libc::SI_KERNEL.to_ne_bytes());
        if event.signal() != libc::SIGALRM
            || event.siginfo() != expected
            || event.target() != (reverie::SignalTarget::Process { pid: target.tgid })
        {
            return ProcessPublication::Rejected(PublicationRejection::InvalidEvent);
        }
        self.publish(target, event, None, true)
    }

    fn publish_child_completion(
        &self,
        completion: reverie::ChildExitCompletion,
    ) -> ProcessPublication {
        let event = match child_exit_signal_event(completion) {
            Ok(event) => event,
            Err(rejection) => return ProcessPublication::Rejected(rejection),
        };
        self.publish(completion.parent, event, Some(&completion), true)
    }

    fn publish(
        &self,
        target: SignalProcessId,
        event: SignalEvent,
        completion: Option<&reverie::ChildExitCompletion>,
        independent_carriers: bool,
    ) -> ProcessPublication {
        use ProcessPublication::Rejected;
        use PublicationRejection::*;
        let Some(registry) = self.0.upgrade() else {
            return Rejected(Closed);
        };
        // A committed result is a stable acknowledgement, not permission to
        // repeat the effect. Consult it before liveness validation so an exact
        // duplicate remains idempotent after the child executor retires.
        if let Some(completion) = completion
            && let Some(record) = registry
                .child_publications
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .get(&(completion.child.tgid.as_raw(), completion.child.generation))
                .cloned()
        {
            return if record.completion() == *completion {
                record.replay()
            } else {
                Rejected(InvalidCompletion)
            };
        }
        let Some(binding) = registry.lookup(target) else {
            return Rejected(StaleProcess);
        };
        let image = binding
            .image
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone();
        let (Some(transaction), Some(lifecycle), Some(signals)) = (
            binding.transaction.upgrade(),
            binding.lifecycle.upgrade(),
            image.signals.upgrade(),
        ) else {
            return Rejected(StaleProcess);
        };
        // The active path neither acquires nor pins an ordinary file table:
        // even dropping its final Arc could close an unrelated blocking socket
        // while the Tool holds its scheduler mutex. Only the inactive private
        // endpoint retains its historical table preflight and corruption checks.
        let files_owner = if independent_carriers {
            None
        } else {
            let Some(files) = image.files.upgrade() else {
                return Rejected(StaleProcess);
            };
            Some(files)
        };
        let files = match files_owner.as_ref() {
            Some(files) => match files.lock() {
                Ok(files) => Some(files),
                Err(_) => return Rejected(Backend(Errno::EIO)),
            },
            None => None,
        };
        let _transaction = transaction.lock().unwrap_or_else(|p| p.into_inner());
        if binding
            .image
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .revision
            != image.revision
        {
            return Rejected(ChangedImage);
        }
        let lifecycle = lifecycle.lock().unwrap_or_else(|p| p.into_inner());
        if binding.identity != target
            || !lifecycle.contains_process(target.tgid.as_raw(), target.generation)
        {
            return Rejected(StaleProcess);
        }
        let child_snapshot = match completion {
            Some(completion) => match registry.process_family_exit(completion.child) {
                Some(ProcessFamilyExit::Child(snapshot)) if snapshot.completion == *completion => {
                    Some(snapshot)
                }
                _ => return Rejected(InvalidCompletion),
            },
            None => None,
        };
        if let Some(completion) = completion {
            let Some(child) = registry.lookup(completion.child) else {
                return Rejected(InvalidCompletion);
            };
            // An adopted orphan's SIGCHLD belongs to its adopter, frozen at
            // its exit; the fork-time parent is already terminal.
            let current_parent = registry
                .family
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .current_parent(process_key(completion.child), child.parent);
            if current_parent != Some(process_key(target))
                || lifecycle.process_exit_status(
                    completion.child.tgid.as_raw(),
                    completion.child.generation,
                ) != Some(completion.status)
            {
                return Rejected(InvalidCompletion);
            }
        }
        // Serialize the run-local terminal latch through the entire commit.
        // This also prevents a different process from publishing after the
        // first failed readiness transaction. No Tool call can occur here.
        let mut failure = registry.failure.lock().unwrap_or_else(|p| p.into_inner());
        if failure.is_some() {
            return Rejected(Terminal);
        }
        // Keep this guard through the complete mutation and readiness phase.
        // A concurrent duplicate cannot pass the check before the first
        // operation records its irreversible commit.
        let mut child_publications = completion.map(|_| {
            registry
                .child_publications
                .lock()
                .unwrap_or_else(|p| p.into_inner())
        });
        if let (Some(completion), Some(publications)) = (completion, child_publications.as_ref())
            && let Some(previous) =
                publications.get(&(completion.child.tgid.as_raw(), completion.child.generation))
        {
            return if previous.completion() == *completion {
                previous.replay()
            } else {
                Rejected(InvalidCompletion)
            };
        }
        let mut process = signals.lock().unwrap_or_else(|p| p.into_inner());
        let signal = event.signal();
        let action = process
            .dispositions
            .get(&signal)
            .copied()
            .unwrap_or_default();
        let disposition = child_snapshot.map_or_else(
            || {
                if action.is_ignored() {
                    PublicationDisposition::Ignored
                } else if action.handler == libc::SIG_DFL as u64 {
                    PublicationDisposition::Default
                } else {
                    PublicationDisposition::Caught
                }
            },
            |snapshot| snapshot.disposition,
        );
        let pending_generation = child_snapshot.map_or_else(
            || process.pending_generation(signal),
            |snapshot| snapshot.pending_generation,
        );
        let mut receipt = PublicationReceipt {
            process: target,
            image: image.revision,
            signal,
            pending_generation,
            change: PendingChange::Suppressed,
            disposition,
            child_completion: completion.copied(),
        };
        if signal == libc::SIGCHLD && disposition == PublicationDisposition::Ignored {
            if let (Some(completion), Some(publications)) =
                (completion, child_publications.as_mut())
            {
                let previous = publications.insert(
                    (completion.child.tgid.as_raw(), completion.child.generation),
                    ChildPublicationRecord::Committed(receipt.clone()),
                );
                debug_assert!(previous.is_none());
            }
            return ProcessPublication::Committed(receipt);
        }
        if let Some(snapshot) = child_snapshot
            && process.pending_generation(signal) != snapshot.pending_generation
        {
            receipt.change = PendingChange::Discarded;
            if let (Some(completion), Some(publications)) =
                (completion, child_publications.as_mut())
            {
                let previous = publications.insert(
                    (completion.child.tgid.as_raw(), completion.child.generation),
                    ChildPublicationRecord::Committed(receipt.clone()),
                );
                debug_assert!(previous.is_none());
            }
            return ProcessPublication::Committed(receipt);
        }
        let matching = process
            .signalfd_masks
            .iter()
            .filter_map(|(&fd, mask)| mask.contains(signal).then_some(fd))
            .collect::<Vec<_>>();
        let carriers = if independent_carriers {
            let Some(carriers) = matching
                .iter()
                .map(|fd| process.signalfd_carriers.get(fd).cloned())
                .collect::<Option<Vec<_>>>()
            else {
                return Rejected(Backend(Errno::EBADF));
            };
            carriers
        } else {
            if matching.iter().any(|fd| {
                !files
                    .as_ref()
                    .expect("private endpoint owns table")
                    .files
                    .contains_key(fd)
            }) {
                return Rejected(Backend(Errno::EBADF));
            }
            Vec::new()
        };
        receipt.change = match process
            .shared_pending
            .enqueue(event, receipt.pending_generation)
        {
            Ok(true) => PendingChange::Queued,
            Ok(false) => PendingChange::Coalesced,
            Err(errno) => return Rejected(Backend(errno)),
        };
        // Release queue/lifecycle locks, retaining descriptor+transaction
        // ownership across only bounded, nonblocking eventfd readiness I/O.
        drop(process);
        drop(lifecycle);
        for (index, fd) in matching.into_iter().enumerate() {
            let file = if independent_carriers {
                carriers[index].file()
            } else {
                &files.as_ref().expect("private endpoint owns table").files[&fd]
            };
            if let Err(raw) = set_signalfd_ready(file, true) {
                let committed = PublicationFailure {
                    receipt,
                    errno: Errno::new(i32::try_from(-raw).unwrap_or(libc::EIO)),
                };
                if let (Some(completion), Some(publications)) =
                    (completion, child_publications.as_mut())
                {
                    let previous = publications.insert(
                        (completion.child.tgid.as_raw(), completion.child.generation),
                        ChildPublicationRecord::Failed(committed.clone()),
                    );
                    debug_assert!(previous.is_none());
                }
                *failure = Some(committed.clone());
                return ProcessPublication::FailedAfterCommit(committed);
            }
        }
        if let (Some(completion), Some(publications)) = (completion, child_publications.as_mut()) {
            let previous = publications.insert(
                (completion.child.tgid.as_raw(), completion.child.generation),
                ChildPublicationRecord::Committed(receipt.clone()),
            );
            debug_assert!(previous.is_none());
        }
        ProcessPublication::Committed(receipt)
    }
}

fn child_exit_signal_event(
    completion: reverie::ChildExitCompletion,
) -> Result<SignalEvent, PublicationRejection> {
    if completion.user_ticks < 0 || completion.system_ticks < 0 {
        return Err(PublicationRejection::InvalidCompletion);
    }
    let (code, status) = match completion.status {
        reverie::ExitStatus::Exited(status) if (0..=i32::from(u8::MAX)).contains(&status) => {
            (libc::CLD_EXITED, status)
        }
        reverie::ExitStatus::Exited(_) => {
            return Err(PublicationRejection::InvalidCompletion);
        }
        reverie::ExitStatus::Signaled(signal, true) => (libc::CLD_DUMPED, signal as libc::c_int),
        reverie::ExitStatus::Signaled(signal, false) => (libc::CLD_KILLED, signal as libc::c_int),
    };
    let mut info = [0; reverie::SIGNAL_INFO_SIZE];
    info[..4].copy_from_slice(&libc::SIGCHLD.to_ne_bytes());
    info[8..12].copy_from_slice(&code.to_ne_bytes());
    info[16..20].copy_from_slice(&completion.child.tgid.as_raw().to_ne_bytes());
    info[20..24].copy_from_slice(&completion.uid.to_ne_bytes());
    info[24..28].copy_from_slice(&status.to_ne_bytes());
    info[32..40].copy_from_slice(&completion.user_ticks.to_ne_bytes());
    info[40..48].copy_from_slice(&completion.system_ticks.to_ne_bytes());
    let event = SignalEvent::new(
        libc::SIGCHLD,
        info,
        reverie::SignalTarget::Process {
            pid: completion.parent.tgid,
        },
    );
    event.map_err(|_| PublicationRejection::InvalidCompletion)
}

impl ProcessSignalRegistry {
    pub(super) fn install(
        &self,
        mode: reverie::BackendSignalControlMode,
        failure: &Arc<crate::failure::RunFailure>,
    ) {
        *self.run_failure.lock().unwrap_or_else(|p| p.into_inner()) = Arc::downgrade(failure);
        self.controlled.store(
            mode == reverie::BackendSignalControlMode::ToolControlled,
            Ordering::Release,
        );
    }

    pub(super) fn controlled(&self) -> bool {
        self.controlled.load(Ordering::Acquire)
    }

    pub(super) fn permit(
        &self,
        task: reverie::SignalTaskIdentity,
    ) -> Option<reverie::SignalDeliveryPermit> {
        let binding = self.lookup(task.process)?;
        let image = binding
            .image
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .revision
            .clone();
        self.permits
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get(&task_key(task))
            .filter(|registered| registered.image == image)
            .map(|registered| registered.permit)
    }

    pub(super) fn take_reported_failure(&self) -> Option<crate::Error> {
        self.reported_failure
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .take()
    }

    pub(super) fn owned_permit(
        &self,
        task: reverie::SignalTaskIdentity,
    ) -> Option<reverie::SignalDeliveryPermit> {
        self.permits
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get(&task_key(task))
            .map(|p| p.permit)
    }

    pub(super) fn retire_task(&self, task: reverie::SignalTaskIdentity) {
        self.permits
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .remove(&task_key(task));
        self.completed_permits
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .remove(&task_key(task));
    }
}

fn public_receipt(receipt: &PublicationReceipt) -> reverie::ProcessSignalPublication {
    debug_assert!(receipt.child_completion.is_none());
    reverie::ProcessSignalPublication {
        process: receipt.process,
        pending_generation: receipt.pending_generation,
        coalesced: receipt.change == PendingChange::Coalesced,
        disposition: match receipt.disposition {
            PublicationDisposition::Ignored => reverie::ProcessAlarmSignalDisposition::Ignored,
            PublicationDisposition::Caught => reverie::ProcessAlarmSignalDisposition::Caught,
            PublicationDisposition::Default => reverie::ProcessAlarmSignalDisposition::DefaultFatal,
        },
    }
}

fn public_child_receipt(receipt: &PublicationReceipt) -> reverie::ChildExitPublication {
    reverie::ChildExitPublication {
        completion: receipt
            .child_completion
            .expect("child publication retained its completion"),
        pending_generation: receipt.pending_generation,
        effect: match receipt.change {
            PendingChange::Queued => reverie::ChildExitPublicationEffect::Queued,
            PendingChange::Coalesced => reverie::ChildExitPublicationEffect::Coalesced,
            PendingChange::Suppressed => {
                reverie::ChildExitPublicationEffect::SuppressedExplicitIgnore
            }
            PendingChange::Discarded => {
                reverie::ChildExitPublicationEffect::DiscardedByDispositionChange
            }
        },
    }
}

fn report_publication_failure(
    registry: &ProcessSignalRegistry,
    process: SignalProcessId,
    phase: &'static str,
    error: crate::Error,
) -> Result<(), Errno> {
    let run = registry
        .run_failure
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .upgrade()
        .ok_or(Errno::ESRCH)?;
    if registry.failure_forwarded.swap(true, Ordering::AcqRel) {
        return Ok(());
    }
    // No scheduler, registry, image or signal lock survives this call.
    let context = crate::failure::FailureContext::new(run, process.tgid, process.tgid);
    let published = context.publish(phase, error);
    // The first run cause may be a concurrent independent failure. Keep this
    // returned Error too, so root completion retains the committed publication
    // receipt as secondary cleanup rather than losing it.
    registry
        .reported_failure
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .get_or_insert(published);
    Ok(())
}

impl reverie::ProcessSignalControl for ProcessSignalControl {
    fn publish_alarm(
        &self,
        process: SignalProcessId,
        event: SignalEvent,
    ) -> reverie::ProcessSignalPublicationResult {
        use reverie::ProcessSignalPublicationResult as Outcome;
        match self.publish_active_alarm(process, event) {
            ProcessPublication::Committed(receipt) => Outcome::Committed(public_receipt(&receipt)),
            ProcessPublication::FailedAfterCommit(failure) => Outcome::FailedAfterCommit {
                receipt: public_receipt(&failure.receipt),
                errno: failure.errno,
            },
            ProcessPublication::Rejected(reason) => Outcome::RejectedBeforeCommit(match reason {
                PublicationRejection::Backend(errno) => errno,
                PublicationRejection::Closed | PublicationRejection::StaleProcess => Errno::ESRCH,
                PublicationRejection::ChangedImage => Errno::EAGAIN,
                PublicationRejection::InvalidEvent | PublicationRejection::InvalidCompletion => {
                    Errno::EINVAL
                }
                PublicationRejection::Terminal => Errno::EIO,
            }),
        }
    }

    fn publish_child_exit(
        &self,
        completion: reverie::ChildExitCompletion,
    ) -> reverie::ChildExitPublicationResult {
        use reverie::ChildExitPublicationResult as Outcome;
        match self.publish_child_completion(completion) {
            ProcessPublication::Committed(receipt) => {
                Outcome::Committed(public_child_receipt(&receipt))
            }
            ProcessPublication::FailedAfterCommit(failure) => Outcome::FailedAfterCommit {
                receipt: public_child_receipt(&failure.receipt),
                errno: failure.errno,
            },
            ProcessPublication::Rejected(reason) => Outcome::RejectedBeforeCommit(match reason {
                PublicationRejection::Backend(errno) => errno,
                PublicationRejection::Closed | PublicationRejection::StaleProcess => Errno::ESRCH,
                PublicationRejection::ChangedImage => Errno::EAGAIN,
                PublicationRejection::InvalidEvent | PublicationRejection::InvalidCompletion => {
                    Errno::EINVAL
                }
                PublicationRejection::Terminal => Errno::EIO,
            }),
        }
    }

    fn signal_recipients(
        &self,
        process: SignalProcessId,
        signal: i32,
    ) -> Result<Vec<reverie::SignalRecipient>, Errno> {
        if !(1..=64).contains(&signal) {
            return Err(Errno::EINVAL);
        }
        let registry = self.0.upgrade().ok_or(Errno::ESRCH)?;
        let binding = registry.lookup(process).ok_or(Errno::ESRCH)?;
        let image = binding
            .image
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone();
        let transaction = binding.transaction.upgrade().ok_or(Errno::ESRCH)?;
        let lifecycle = binding.lifecycle.upgrade().ok_or(Errno::ESRCH)?;
        let signals = image.signals.upgrade().ok_or(Errno::ESRCH)?;
        let _transaction = transaction.lock().unwrap_or_else(|p| p.into_inner());
        if binding
            .image
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .revision
            != image.revision
        {
            return Err(Errno::EAGAIN);
        }
        let lifecycle = lifecycle.lock().unwrap_or_else(|p| p.into_inner());
        let signals = signals.lock().unwrap_or_else(|p| p.into_inner());
        let mut signal_mask = crate::signal::KernelSigset::default();
        signal_mask.insert(signal);
        if !signals
            .shared_pending
            .any_matching(signal_mask, &signals.pending_generations)
        {
            return Ok(Vec::new());
        }
        let mut recipients = Vec::new();
        for task in lifecycle.signal_process_tasks(process) {
            let Some(thread) = lifecycle.signal_target(task.tid.as_raw()) else {
                continue;
            };
            if !thread.lock().blocked.contains(signal) {
                recipients.push(reverie::SignalRecipient { task });
            }
        }
        recipients.sort_by_key(|recipient| recipient.task.tid);
        Ok(recipients)
    }

    fn alarm_recipients(
        &self,
        process: SignalProcessId,
    ) -> Result<Vec<reverie::SignalRecipient>, Errno> {
        <Self as reverie::ProcessSignalControl>::signal_recipients(self, process, libc::SIGALRM)
    }

    fn reserve_delivery(&self, permit: reverie::SignalDeliveryPermit) -> Result<(), Errno> {
        let registry = self.0.upgrade().ok_or(Errno::ESRCH)?;
        let binding = registry.lookup(permit.task.process).ok_or(Errno::ESRCH)?;
        let transaction = binding.transaction.upgrade().ok_or(Errno::ESRCH)?;
        let lifecycle = binding.lifecycle.upgrade().ok_or(Errno::ESRCH)?;
        let _transaction = transaction.lock().unwrap_or_else(|p| p.into_inner());
        let lifecycle = lifecycle.lock().unwrap_or_else(|p| p.into_inner());
        let task = lifecycle
            .get(permit.task.tid.as_raw())
            .ok_or(Errno::ESRCH)?;
        if task.generation != permit.task.task_generation
            || task.process_generation != permit.task.process.generation
            || task.tgid != permit.task.process.tgid.as_raw()
            || permit.site.is_some_and(|site| {
                site.process != permit.task.process
                    || site.tid != permit.task.tid
                    || site.task_generation != permit.task.task_generation
            })
        {
            return Err(Errno::EINVAL);
        }
        let image = binding
            .image
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .revision
            .clone();
        let mut permits = registry.permits.lock().unwrap_or_else(|p| p.into_inner());
        if permits.contains_key(&task_key(permit.task)) {
            return Err(Errno::EBUSY);
        }
        permits.insert(task_key(permit.task), RegisteredPermit { permit, image });
        Ok(())
    }

    fn release_delivery(&self, permit: reverie::SignalDeliveryPermit) -> Result<(), Errno> {
        let registry = self.0.upgrade().ok_or(Errno::ESRCH)?;
        let mut permits = registry.permits.lock().unwrap_or_else(|p| p.into_inner());
        let mut completed = registry
            .completed_permits
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let key = task_key(permit.task);
        match permits.get(&key) {
            Some(current) if current.permit == permit => {
                permits.remove(&key);
                completed.insert(key, permit);
                Ok(())
            }
            None if completed.get(&key) == Some(&permit) => Ok(()),
            _ => Err(Errno::EINVAL),
        }
    }

    fn finish_publication_failure(&self, process: SignalProcessId) -> Result<(), Errno> {
        let registry = self.0.upgrade().ok_or(Errno::ESRCH)?;
        let failure = registry
            .failure
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
            .ok_or(Errno::EINVAL)?;
        if failure.receipt.process != process || failure.receipt.child_completion.is_some() {
            return Err(Errno::EINVAL);
        }
        report_publication_failure(
            &registry,
            process,
            "process signal publication",
            crate::Error::ProcessSignalPublication {
                receipt: public_receipt(&failure.receipt),
                errno: failure.errno,
            },
        )
    }

    fn finish_child_exit_publication_failure(
        &self,
        receipt: reverie::ChildExitPublication,
    ) -> Result<(), Errno> {
        let registry = self.0.upgrade().ok_or(Errno::ESRCH)?;
        let failure = registry
            .failure
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
            .ok_or(Errno::EINVAL)?;
        if failure.receipt.child_completion.is_none()
            || public_child_receipt(&failure.receipt) != receipt
        {
            return Err(Errno::EINVAL);
        }
        report_publication_failure(
            &registry,
            receipt.completion.parent,
            "child-exit signal publication",
            crate::Error::ChildExitPublication {
                receipt,
                errno: failure.errno,
            },
        )
    }
}

#[cfg(test)]
mod tests {
    use std::os::fd::AsRawFd;

    use super::super::ElfExecutor;
    use super::super::native_loaded_state;
    use super::*;
    use crate::GuestMemory;
    use crate::SyscallRequest;
    use crate::signal::KernelSigaction;
    use crate::signal::KernelSigset;

    fn executor() -> ElfExecutor {
        ElfExecutor::new(native_loaded_state(std::path::Path::new("/tmp")), false)
    }

    fn identity(executor: &ElfExecutor) -> SignalProcessId {
        executor.signal_task_identity().unwrap().process
    }

    /// A traced root with an arbitrary PID; Hermit runs its root as PID 3,
    /// leaving namespace init outside the guest.
    fn executor_with_root(pid: i32) -> ElfExecutor {
        let mut state = native_loaded_state(std::path::Path::new("/tmp"));
        state.pid = pid;
        state.pgid = pid;
        state.tid = pid;
        state.ppid = if pid == CONTAINER_INIT_PID {
            0
        } else {
            CONTAINER_INIT_PID
        };
        state.task_lifecycle = Arc::new(Mutex::new(TaskLifecycleTable::with_root(
            pid,
            pid,
            pid,
            state.dumpable,
        )));
        ElfExecutor::new(state, false)
    }

    fn getppid(executor: &mut ElfExecutor) -> i64 {
        let memory = GuestMemory::new(0, 4096).unwrap();
        executor.execute(
            &SyscallRequest::new(libc::SYS_getppid as u64, [0; 6]),
            &memory,
        )
    }

    fn family_edge(
        registry: &ProcessSignalRegistry,
        parent: SignalProcessId,
        child: SignalProcessId,
    ) -> Option<DirectChildState> {
        registry
            .family
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .direct_children
            .get(&process_key(parent))
            .and_then(|children| children.get(&process_key(child)).copied())
    }

    fn alarm(target: SignalProcessId) -> SignalEvent {
        let mut info = [0; reverie::SIGNAL_INFO_SIZE];
        info[..4].copy_from_slice(&libc::SIGALRM.to_ne_bytes());
        info[8..12].copy_from_slice(&libc::SI_KERNEL.to_ne_bytes());
        SignalEvent::new(
            libc::SIGALRM,
            info,
            reverie::SignalTarget::Process { pid: target.tgid },
        )
        .unwrap()
    }

    fn call(executor: &mut ElfExecutor, memory: &GuestMemory, number: i64, args: [u64; 6]) -> i64 {
        executor.execute(&SyscallRequest::new(number as u64, args), memory)
    }

    fn read_struct<T>(memory: &GuestMemory, address: u64) -> T {
        let mut value = std::mem::MaybeUninit::<T>::zeroed();
        // SAFETY: value is writable for exactly size_of::<T>() bytes.
        let bytes = unsafe {
            std::slice::from_raw_parts_mut(
                value.as_mut_ptr().cast::<u8>(),
                std::mem::size_of::<T>(),
            )
        };
        memory.read(address, bytes).unwrap();
        // SAFETY: zeroed storage was fully initialized by memory.read.
        unsafe { value.assume_init() }
    }

    fn signalfd(executor: &mut ElfExecutor, memory: &mut GuestMemory) -> i32 {
        signalfd_for(executor, memory, libc::SIGALRM)
    }

    fn signalfd_for(executor: &mut ElfExecutor, memory: &mut GuestMemory, signal: i32) -> i32 {
        let mut mask = KernelSigset::default();
        mask.insert(signal);
        memory.write(0x80, &mask.to_bytes()).unwrap();
        let fd = call(
            executor,
            memory,
            libc::SYS_signalfd4,
            [u64::MAX, 0x80, 8, libc::SFD_NONBLOCK as u64, 0, 0],
        );
        assert!(fd >= 3, "signalfd setup: {fd}");
        fd as i32
    }

    fn ready(executor: &ElfExecutor, fd: i32) -> bool {
        let files = executor.file_table.lock().unwrap();
        let mut poll = libc::pollfd {
            fd: files.files[&fd].as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        assert!(unsafe { libc::poll(&mut poll, 1, 0) } >= 0);
        poll.revents & libc::POLLIN != 0
    }

    fn receipt(outcome: ProcessPublication) -> PublicationReceipt {
        match outcome {
            ProcessPublication::Committed(receipt) => receipt,
            other => panic!("publication did not commit: {other:?}"),
        }
    }

    fn child_completion(
        parent: SignalProcessId,
        child: SignalProcessId,
        status: reverie::ExitStatus,
        waitable: bool,
    ) -> reverie::ChildExitCompletion {
        reverie::ChildExitCompletion {
            parent,
            child,
            status,
            waitable,
            uid: 0,
            user_ticks: 0,
            system_ticks: 0,
        }
    }

    fn child_receipt(
        outcome: reverie::ChildExitPublicationResult,
    ) -> reverie::ChildExitPublication {
        match outcome {
            reverie::ChildExitPublicationResult::Committed(receipt) => receipt,
            other => panic!("child publication did not commit: {other:?}"),
        }
    }

    #[test]
    fn active_publication_and_dequeue_complete_while_file_table_is_held() {
        let mut executor = executor();
        let process = identity(&executor);
        let mut memory = GuestMemory::new(0, 4096).unwrap();
        let fd = signalfd(&mut executor, &mut memory);
        let alias = call(
            &mut executor,
            &memory,
            libc::SYS_dup,
            [fd as u64, 0, 0, 0, 0, 0],
        );
        assert!(alias > i64::from(fd));
        executor
            .signal_registry
            .controlled
            .store(true, Ordering::Release);
        let control = executor.backend_signal_control().process;
        let permit = reverie::SignalDeliveryPermit {
            task: executor.signal_task_identity().unwrap(),
            sequence: 1,
            site: None,
        };
        control.reserve_delivery(permit).unwrap();
        let table = executor.file_table.clone();
        let held = table.lock().unwrap();
        // The predecessor deadlocks here. This is a structural native control,
        // not a claim that arbitrary guest blocking reads are interruptible.
        assert!(matches!(
            control.publish_alarm(process, alarm(process)),
            reverie::ProcessSignalPublicationResult::Committed(_)
        ));
        let ready = |fd: i32| {
            let mut poll = libc::pollfd {
                fd: held.files[&fd].as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            assert!(unsafe { libc::poll(&mut poll, 1, 0) } >= 0);
            poll.revents & libc::POLLIN != 0
        };
        assert!(ready(fd) && ready(alias as i32));
        assert_eq!(
            executor
                .take_pending_signal_for_delivery()
                .unwrap()
                .unwrap()
                .event,
            alarm(process)
        );
        assert!(!ready(fd) && !ready(alias as i32));
        assert!(
            table.try_lock().is_err(),
            "the real table guard still belongs to this control"
        );
        control.release_delivery(permit).unwrap();
        drop(held);
    }

    #[test]
    fn active_carrier_alias_exec_close_and_process_lifetimes_are_exact() {
        let mut parent = executor();
        let child = parent.fork_child(2, false, false).unwrap();
        let parent_id = identity(&parent);
        let child_id = identity(&child);
        let mut memory = GuestMemory::new(0, 4096).unwrap();
        let fd = signalfd(&mut parent, &mut memory);
        let alias = call(
            &mut parent,
            &memory,
            libc::SYS_dup,
            [fd as u64, 0, 0, 0, 0, 0],
        ) as i32;
        assert!(alias > fd);
        let keeper = {
            let signals = parent.state.process_signals.lock().unwrap();
            assert_eq!(
                signals.signalfd_carriers[&fd],
                signals.signalfd_carriers[&alias]
            );
            signals.signalfd_carriers[&fd].downgrade()
        };
        assert!(
            child
                .state
                .process_signals
                .lock()
                .unwrap()
                .signalfd_carriers
                .is_empty()
        );
        assert!(
            parent.fork_child(3, false, false).is_err(),
            "existing signalfd/fork limitation remains explicit"
        );
        assert_eq!(
            call(
                &mut parent,
                &memory,
                libc::SYS_fcntl,
                [
                    fd as u64,
                    libc::F_SETFD as u64,
                    libc::FD_CLOEXEC as u64,
                    0,
                    0,
                    0
                ]
            ),
            0
        );
        let control = parent.backend_signal_control().process;
        assert!(matches!(
            control.publish_alarm(child_id, alarm(child_id)),
            reverie::ProcessSignalPublicationResult::Committed(_)
        ));
        assert!(
            !ready(&parent, alias),
            "another process cannot publish to this carrier"
        );
        parent.replace_after_exec(native_loaded_state(std::path::Path::new("/tmp")));
        assert_eq!(identity(&parent), parent_id);
        assert_eq!(
            parent
                .state
                .process_signals
                .lock()
                .unwrap()
                .signalfd_carriers
                .keys()
                .copied()
                .collect::<Vec<_>>(),
            vec![alias]
        );
        assert!(
            keeper.upgrade().is_some(),
            "non-CLOEXEC alias retains the same description"
        );
        assert!(matches!(
            control.publish_alarm(parent_id, alarm(parent_id)),
            reverie::ProcessSignalPublicationResult::Committed(_)
        ));
        assert!(ready(&parent, alias));
        assert_eq!(
            call(
                &mut parent,
                &memory,
                libc::SYS_close,
                [alias as u64, 0, 0, 0, 0, 0]
            ),
            0
        );
        assert!(
            keeper.upgrade().is_none(),
            "last guest alias releases the sole private keeper"
        );
        assert!(
            parent
                .state
                .process_signals
                .lock()
                .unwrap()
                .signalfd_carriers
                .is_empty()
        );
        assert!(
            parent
                .state
                .process_signals
                .lock()
                .unwrap()
                .shared_pending
                .contains(libc::SIGALRM),
            "close is not signal consumption"
        );
        parent.retire_current_thread(reverie::ExitStatus::SUCCESS, false);
        assert_eq!(
            control.publish_alarm(parent_id, alarm(parent_id)),
            reverie::ProcessSignalPublicationResult::RejectedBeforeCommit(Errno::ESRCH)
        );
        assert!(
            child
                .state
                .process_signals
                .lock()
                .unwrap()
                .shared_pending
                .contains(libc::SIGALRM)
        );
    }

    #[test]
    fn active_carrier_preparation_emfile_has_no_guest_descriptor_effect() {
        use std::os::fd::FromRawFd;
        const TEST: &str = "executor::process_signal_publication::tests::active_carrier_preparation_emfile_has_no_guest_descriptor_effect";
        const ENV: &str = "REVERIE_SIGNALFD_KEEPER_EMFILE_CHILD";
        const COMPLETE: &str = "signalfd keeper EMFILE control completed";
        fn limit() -> libc::rlimit {
            let mut r = libc::rlimit {
                rlim_cur: 0,
                rlim_max: 0,
            };
            assert_eq!(unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut r) }, 0);
            r
        }
        if std::env::var(ENV).as_deref() != Ok(TEST) {
            assert!(std::env::var_os(ENV).is_none());
            let before = limit();
            let output = std::process::Command::new("/usr/bin/timeout")
                .args(["--kill-after=2s", "10s"])
                .arg(std::env::current_exe().unwrap())
                .args(["--exact", TEST, "--test-threads=1", "--nocapture"])
                .env(ENV, TEST)
                .output()
                .unwrap();
            let after = limit();
            assert_eq!(
                (after.rlim_cur, after.rlim_max),
                (before.rlim_cur, before.rlim_max)
            );
            eprintln!(
                "signalfd keeper child status={}\nstdout:\n{}\nstderr:\n{}",
                output.status,
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            assert!(output.status.success());
            assert_eq!(
                String::from_utf8_lossy(&output.stderr)
                    .lines()
                    .filter(|line| *line == COMPLETE)
                    .count(),
                1
            );
            return;
        }
        let mut executor = executor();
        let mut memory = GuestMemory::new(0, 4096).unwrap();
        let mut mask = KernelSigset::default();
        mask.insert(libc::SIGALRM);
        memory.write(0x80, &mask.to_bytes()).unwrap();
        let original = limit();
        let reduced = libc::rlimit {
            rlim_cur: original.rlim_cur.min(256),
            rlim_max: original.rlim_max,
        };
        let source = std::fs::File::open("/dev/null").unwrap();
        let keys = executor.state.files.keys().copied().collect::<Vec<_>>();
        let mut fillers = Vec::with_capacity(257);
        let mut exhausted = None;
        let mut observed = None;
        let lowered = unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &reduced) };
        if lowered == 0 {
            for _ in 0..=256 {
                let raw = unsafe { libc::fcntl(source.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 3) };
                if raw < 0 {
                    exhausted = std::io::Error::last_os_error().raw_os_error();
                    break;
                }
                fillers.push(unsafe { std::fs::File::from_raw_fd(raw) });
            }
            if exhausted == Some(libc::EMFILE) && !fillers.is_empty() {
                drop(fillers.pop());
                // Prove an actual eventfd fits but its simultaneous keeper does
                // not. Then restore the same one-slot boundary for execute().
                let probe = unsafe { libc::eventfd(0, libc::EFD_NONBLOCK | libc::EFD_CLOEXEC) };
                if probe >= 0 {
                    let probe = unsafe { std::fs::File::from_raw_fd(probe) };
                    let clone_error = probe.try_clone().err().and_then(|e| e.raw_os_error());
                    drop(probe);
                    let raw = call(
                        &mut executor,
                        &memory,
                        libc::SYS_signalfd4,
                        [u64::MAX, 0x80, 8, libc::SFD_NONBLOCK as u64, 0, 0],
                    );
                    observed = Some((clone_error, raw));
                }
            }
        }
        let restored = unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &original) };
        drop(fillers);
        assert_eq!(restored, 0);
        assert_eq!(lowered, 0);
        assert_eq!(exhausted, Some(libc::EMFILE));
        assert_eq!(
            observed,
            Some((Some(libc::EMFILE), -i64::from(libc::EMFILE)))
        );
        assert_eq!(
            executor.state.files.keys().copied().collect::<Vec<_>>(),
            keys
        );
        assert_eq!(
            executor
                .file_table
                .lock()
                .unwrap()
                .files
                .keys()
                .copied()
                .collect::<Vec<_>>(),
            keys
        );
        let signals = executor.state.process_signals.lock().unwrap();
        assert!(signals.signalfd_masks.is_empty() && signals.signalfd_carriers.is_empty());
        assert!(signals.shared_pending.is_empty());
        drop(signals);
        let fd = signalfd(&mut executor, &mut memory);
        assert!(fd >= 3, "unconstrained creation still works");
        eprintln!("{COMPLETE}");
    }

    #[test]
    fn active_publication_failure_retains_receipt_beside_prior_run_cause() {
        let mut executor = executor();
        let process = identity(&executor);
        let global = Arc::new(());
        let run = crate::failure::RunFailure::new(&global);
        executor
            .signal_registry
            .install(reverie::BackendSignalControlMode::ToolControlled, &run);
        let context = crate::failure::FailureContext::new(run.clone(), process.tgid, process.tgid);
        let _first = context.publish(
            "prior cause",
            crate::Error::GuestClock("prior clock cause".into()),
        );
        let mut memory = GuestMemory::new(0, 4096).unwrap();
        let fd = signalfd(&mut executor, &mut memory);
        // Real post-commit carrier failure, identical to the existing private
        // negative control: queue insertion succeeds, eventfd write gets EBADF.
        let carrier = executor
            .state
            .process_signals
            .lock()
            .unwrap()
            .signalfd_carriers
            .insert(
                fd,
                crate::signal::SignalFdCarrier::pin_eventfd(
                    &std::fs::File::open("/dev/null").unwrap(),
                )
                .unwrap(),
            )
            .unwrap();
        let control = executor.backend_signal_control().process;
        let reverie::ProcessSignalPublicationResult::FailedAfterCommit { receipt, errno } =
            control.publish_alarm(process, alarm(process))
        else {
            panic!("missing committed failure")
        };
        assert_eq!(errno, Errno::EBADF);
        control.finish_publication_failure(process).unwrap();
        let retained = executor
            .take_process_publication_failure()
            .expect("root-owned error receipt");
        assert!(
            matches!(retained.primary(), crate::Error::ProcessSignalPublication { receipt: actual, errno: Errno::EBADF } if *actual == receipt)
        );
        let completed = run.complete::<()>(Err(retained)).unwrap_err();
        assert!(
            matches!(completed.primary(), crate::Error::GuestClock(message) if message == "prior clock cause")
        );
        assert!(
            completed
                .to_string()
                .contains("process signal publication committed")
        );
        assert!(executor.take_process_publication_failure().is_none());
        executor
            .state
            .process_signals
            .lock()
            .unwrap()
            .signalfd_carriers
            .insert(fd, carrier);
    }

    #[test]
    fn controlled_pending_alarm_does_not_reject_or_preselect_a_new_thread() {
        let leader = executor();
        leader
            .signal_registry
            .controlled
            .store(true, Ordering::Release);
        let control = leader.backend_signal_control().process;
        let process = identity(&leader);
        assert!(matches!(
            control.publish_alarm(process, alarm(process)),
            reverie::ProcessSignalPublicationResult::Committed(_)
        ));
        let mut worker = leader.thread_child(2).unwrap();
        assert!(worker.take_pending_signal_for_delivery().unwrap().is_none());
        assert_eq!(worker.delivery_permit(), None);
        let permit = reverie::SignalDeliveryPermit {
            task: worker.signal_task_identity().unwrap(),
            sequence: 1,
            site: None,
        };
        control.reserve_delivery(permit).unwrap();
        assert_eq!(
            worker
                .take_pending_signal_for_delivery()
                .unwrap()
                .unwrap()
                .event,
            alarm(process)
        );
        control.release_delivery(permit).unwrap();
    }

    #[test]
    fn controlled_alarm_selects_unblocked_worker_and_requires_its_permit() {
        let mut leader = executor();
        let mut worker = leader.thread_child(2).unwrap();
        let process = identity(&leader);
        leader
            .signal_registry
            .controlled
            .store(true, Ordering::Release);
        leader
            .state
            .thread_signals
            .lock()
            .blocked
            .insert(libc::SIGALRM);
        let control = leader.backend_signal_control().process;
        assert!(matches!(
            control.publish_alarm(process, alarm(process)),
            reverie::ProcessSignalPublicationResult::Committed(_)
        ));
        assert_eq!(
            control.alarm_recipients(process).unwrap(),
            vec![reverie::SignalRecipient {
                task: worker.signal_task_identity().unwrap()
            }]
        );
        assert!(leader.take_pending_signal_for_delivery().unwrap().is_none());
        assert!(worker.take_pending_signal_for_delivery().unwrap().is_none());
        let permit = reverie::SignalDeliveryPermit {
            task: worker.signal_task_identity().unwrap(),
            sequence: 1,
            site: None,
        };
        control.reserve_delivery(permit).unwrap();
        assert_eq!(
            worker
                .take_pending_signal_for_delivery()
                .unwrap()
                .unwrap()
                .event,
            alarm(process)
        );
        control.release_delivery(permit).unwrap();
        control.release_delivery(permit).unwrap();
        assert!(
            control
                .release_delivery(reverie::SignalDeliveryPermit {
                    sequence: 2,
                    ..permit
                })
                .is_err()
        );
        assert!(control.alarm_recipients(process).unwrap().is_empty());
    }

    #[test]
    fn controlled_permit_is_bound_to_image_and_old_owner_can_settle_after_exec() {
        let mut executor = executor();
        let process = identity(&executor);
        executor
            .signal_registry
            .controlled
            .store(true, Ordering::Release);
        let control = executor.backend_signal_control().process;
        let permit = reverie::SignalDeliveryPermit {
            task: executor.signal_task_identity().unwrap(),
            sequence: 1,
            site: None,
        };
        control.reserve_delivery(permit).unwrap();
        assert_eq!(executor.delivery_permit(), Some(permit));
        executor.replace_after_exec(native_loaded_state(std::path::Path::new("/tmp")));
        assert_eq!(executor.delivery_permit(), None);
        assert_eq!(executor.owned_delivery_permit(), Some(permit));
        assert!(matches!(
            control.publish_alarm(process, alarm(process)),
            reverie::ProcessSignalPublicationResult::Committed(_)
        ));
        assert!(
            executor
                .take_pending_signal_for_delivery()
                .unwrap()
                .is_none()
        );
        control.release_delivery(permit).unwrap();
        let fresh = reverie::SignalDeliveryPermit {
            sequence: 2,
            ..permit
        };
        control.reserve_delivery(fresh).unwrap();
        assert_eq!(
            executor
                .take_pending_signal_for_delivery()
                .unwrap()
                .unwrap()
                .event,
            alarm(process)
        );
        control.release_delivery(fresh).unwrap();
    }

    #[test]
    fn controlled_parked_lease_does_not_borrow_another_permit_or_callback() {
        let mut executor = executor();
        executor.enable_signal_dequeues();
        executor
            .signal_registry
            .controlled
            .store(true, Ordering::Release);
        let site = executor.begin_signal_callback().unwrap();
        let control = executor.backend_signal_control().process;
        let permit = reverie::SignalDeliveryPermit {
            task: executor.signal_task_identity().unwrap(),
            sequence: 7,
            site: Some(site),
        };
        control.reserve_delivery(permit).unwrap();
        assert_eq!(
            executor.admit_signal_observation(site, reverie::ParkedObservationLease { nonce: 8 }),
            Err(Errno::EINVAL)
        );
        executor
            .admit_signal_observation(site, reverie::ParkedObservationLease { nonce: 7 })
            .unwrap();
        assert_eq!(
            executor.admit_signal_observation(site, reverie::ParkedObservationLease { nonce: 7 }),
            Err(Errno::EINVAL)
        );
        control.release_delivery(permit).unwrap();
    }

    #[test]
    fn inactive_publication_refuses_poisoned_file_table_without_effects() {
        let executor = executor();
        let id = identity(&executor);
        let control = executor.signal_registry.control();
        let files = executor.file_table.clone();
        assert!(
            std::thread::spawn(move || {
                let _guard = files.lock().unwrap();
                panic!("controlled authoritative-table poison");
            })
            .join()
            .is_err()
        );
        for _ in 0..2 {
            assert!(matches!(
                control.publish_alarm(id, alarm(id)),
                ProcessPublication::Rejected(PublicationRejection::Backend(errno))
                    if errno == Errno::EIO
            ));
        }
        let process = executor.state.process_signals.lock().unwrap();
        assert_eq!(
            process
                .shared_pending
                .pending_mask(&process.pending_generations)
                .to_bytes(),
            KernelSigset::default().to_bytes()
        );
        assert!(executor.signal_registry.failure.lock().unwrap().is_none());
        assert!(executor.file_table.is_poisoned());
    }

    #[test]
    fn inactive_publication_alarm_preserves_masks_dispositions_and_coalesces() {
        for handler in [libc::SIG_DFL as u64, libc::SIG_IGN as u64, 0x1234] {
            for blocked in [false, true] {
                let executor = executor();
                let id = identity(&executor);
                let control = executor.signal_registry.control();
                executor
                    .state
                    .process_signals
                    .lock()
                    .unwrap()
                    .dispositions
                    .insert(
                        libc::SIGALRM,
                        KernelSigaction {
                            handler,
                            ..Default::default()
                        },
                    );
                if blocked {
                    executor
                        .state
                        .thread_signals
                        .lock()
                        .blocked
                        .insert(libc::SIGALRM);
                }
                let before_thread = executor.state.thread_signals.lock().clone();
                let first = receipt(control.publish_alarm(id, alarm(id)));
                assert_eq!(first.change, PendingChange::Queued);
                let second = receipt(control.publish_alarm(id, alarm(id)));
                assert_eq!(second.change, PendingChange::Coalesced);
                assert_eq!(first.image, second.image);
                assert_eq!(first.pending_generation, 0);
                assert_eq!(
                    first.disposition,
                    match handler {
                        0 => PublicationDisposition::Default,
                        1 => PublicationDisposition::Ignored,
                        _ => PublicationDisposition::Caught,
                    }
                );
                assert_eq!(*executor.state.thread_signals.lock(), before_thread);
                assert_eq!(executor.state.logical_clock_ns, 0);
                assert_eq!(
                    executor
                        .state
                        .process_signals
                        .lock()
                        .unwrap()
                        .shared_pending
                        .take_matching(
                            {
                                let mut mask = KernelSigset::default();
                                mask.insert(libc::SIGALRM);
                                mask
                            },
                            &[0; 65]
                        ),
                    Some(alarm(id))
                );
            }
        }
    }

    #[test]
    fn inactive_publication_rejects_bad_event_reused_process_and_closed_run() {
        let mut executor = executor();
        let id = identity(&executor);
        let control = executor.signal_registry.control();
        let mut info = alarm(id).siginfo();
        info[127] = 1;
        let invalid = SignalEvent::new(
            libc::SIGALRM,
            info,
            reverie::SignalTarget::Process { pid: id.tgid },
        )
        .unwrap();
        assert_eq!(
            control.publish_alarm(id, invalid),
            ProcessPublication::Rejected(PublicationRejection::InvalidEvent)
        );
        assert!(
            executor
                .state
                .process_signals
                .lock()
                .unwrap()
                .shared_pending
                .is_empty()
        );
        executor.retire_current_thread(reverie::ExitStatus::SUCCESS, false);
        // Numeric reuse in the same lifecycle cannot revive the old generation.
        executor
            .state
            .task_lifecycle
            .lock()
            .unwrap()
            .register(1, 1, 1, true);
        assert_eq!(
            control.publish_alarm(id, alarm(id)),
            ProcessPublication::Rejected(PublicationRejection::StaleProcess)
        );
        let signals = Arc::downgrade(&executor.state.process_signals);
        let files = Arc::downgrade(&executor.file_table);
        drop(executor);
        assert!(signals.upgrade().is_none());
        assert!(files.upgrade().is_none());
        assert_eq!(
            control.publish_alarm(id, alarm(id)),
            ProcessPublication::Rejected(PublicationRejection::Closed)
        );
    }

    #[test]
    fn inactive_publication_resolves_current_exec_image_and_surviving_thread() {
        let mut leader = executor();
        let mut worker = leader.thread_child(2).unwrap();
        let id = identity(&leader);
        let control = leader.signal_registry.control();
        leader.retire_current_thread(reverie::ExitStatus::SUCCESS, false);
        leader.release_files_on_exit();
        drop(leader);
        receipt(control.publish_alarm(id, alarm(id)));
        assert_eq!(
            worker
                .take_pending_signal_for_delivery()
                .unwrap()
                .unwrap()
                .event,
            alarm(id)
        );
        worker.retire_current_thread(reverie::ExitStatus::SUCCESS, false);
        assert_eq!(
            control.publish_alarm(id, alarm(id)),
            ProcessPublication::Rejected(PublicationRejection::StaleProcess)
        );
        drop(worker);

        let mut executor = executor();
        let control = executor.signal_registry.control();
        let id = identity(&executor);
        let first = receipt(control.publish_alarm(id, alarm(id)));
        let old = executor.state.process_signals.clone();
        executor.replace_after_exec(native_loaded_state(std::path::Path::new("/tmp")));
        // Exec preserves pending SIGALRM but resets the image binding.
        let second = receipt(control.publish_alarm(id, alarm(id)));
        assert_ne!(first.image, second.image);
        assert_eq!(second.change, PendingChange::Coalesced);
        assert_eq!(identity(&executor), id);
        assert!(!Arc::ptr_eq(&old, &executor.state.process_signals));
        assert_eq!(
            executor
                .take_pending_signal_for_delivery()
                .unwrap()
                .unwrap()
                .event,
            alarm(id)
        );
        receipt(control.publish_alarm(id, alarm(id)));
        assert!(old.lock().unwrap().shared_pending.contains(libc::SIGALRM));
        assert_eq!(
            executor
                .take_pending_signal_for_delivery()
                .unwrap()
                .unwrap()
                .event,
            alarm(id)
        );
    }

    #[test]
    fn inactive_publication_aliases_all_consumers_and_ignore_update_readiness() {
        let mut executor = executor();
        let id = identity(&executor);
        let control = executor.signal_registry.control();
        let mut memory = GuestMemory::new(0, 4096).unwrap();
        let original = signalfd(&mut executor, &mut memory);
        let duplicate = call(
            &mut executor,
            &memory,
            libc::SYS_dup,
            [original as u64, 0, 0, 0, 0, 0],
        ) as i32;
        assert!(duplicate > original);
        assert_eq!(
            call(
                &mut executor,
                &memory,
                libc::SYS_close,
                [original as u64, 0, 0, 0, 0, 0]
            ),
            0
        );
        for consumer in 0..3 {
            receipt(control.publish_alarm(id, alarm(id)));
            assert!(ready(&executor, duplicate));
            match consumer {
                0 => assert_eq!(
                    executor
                        .take_pending_signal_for_delivery()
                        .unwrap()
                        .unwrap()
                        .event,
                    alarm(id)
                ),
                1 => {
                    assert_eq!(
                        call(
                            &mut executor,
                            &memory,
                            libc::SYS_read,
                            [duplicate as u64, 0x100, 128, 0, 0, 0]
                        ),
                        128
                    );
                    let mut signo = [0; 4];
                    memory.read(0x100, &mut signo).unwrap();
                    assert_eq!(i32::from_ne_bytes(signo), libc::SIGALRM);
                }
                _ => assert_eq!(
                    call(
                        &mut executor,
                        &memory,
                        libc::SYS_rt_sigtimedwait,
                        [0x80, 0x100, 0, 8, 0, 0]
                    ),
                    i64::from(libc::SIGALRM)
                ),
            }
            assert!(!ready(&executor, duplicate));
        }
        receipt(control.publish_alarm(id, alarm(id)));
        let ignored = KernelSigaction {
            handler: libc::SIG_IGN as u64,
            ..Default::default()
        };
        memory.write(0x200, &ignored.encode()).unwrap();
        assert_eq!(
            call(
                &mut executor,
                &memory,
                libc::SYS_rt_sigaction,
                [libc::SIGALRM as u64, 0x200, 0, 8, 0, 0]
            ),
            0
        );
        assert!(!ready(&executor, duplicate));
        assert!(
            executor
                .state
                .process_signals
                .lock()
                .unwrap()
                .shared_pending
                .is_empty()
        );
        let later = receipt(control.publish_alarm(id, alarm(id)));
        assert_eq!(later.pending_generation, 1);
        assert!(ready(&executor, duplicate));
        // Closing an alias must not cause the private publisher to use its old
        // local descriptor snapshot or to write into a subsequently reused fd.
        assert_eq!(
            call(
                &mut executor,
                &memory,
                libc::SYS_close,
                [duplicate as u64, 0, 0, 0, 0, 0]
            ),
            0
        );
        receipt(control.publish_alarm(id, alarm(id)));
        assert!(executor.file_table.lock().unwrap().files.is_empty());
    }

    #[test]
    fn inactive_publication_preflight_and_postcommit_failure_are_distinct() {
        let mut executor = executor();
        let id = identity(&executor);
        let control = executor.signal_registry.control();
        let mut memory = GuestMemory::new(0, 4096).unwrap();
        let fd = signalfd(&mut executor, &mut memory);
        let carrier = executor
            .file_table
            .lock()
            .unwrap()
            .files
            .remove(&fd)
            .unwrap();
        assert_eq!(
            control.publish_alarm(id, alarm(id)),
            ProcessPublication::Rejected(PublicationRejection::Backend(Errno::EBADF))
        );
        assert!(
            executor
                .state
                .process_signals
                .lock()
                .unwrap()
                .shared_pending
                .is_empty()
        );
        // Deliberate backing-carrier corruption: real readiness write fails
        // EBADF after queue insertion. No production mutator installs this.
        executor
            .file_table
            .lock()
            .unwrap()
            .files
            .insert(fd, std::fs::File::open("/dev/null").unwrap());
        let ProcessPublication::FailedAfterCommit(failure) = control.publish_alarm(id, alarm(id))
        else {
            panic!("missing postcommit failure")
        };
        assert_eq!(failure.errno, Errno::EBADF);
        assert_eq!(failure.receipt.change, PendingChange::Queued);
        assert!(
            executor
                .state
                .process_signals
                .lock()
                .unwrap()
                .shared_pending
                .contains(libc::SIGALRM)
        );
        assert_eq!(
            *executor.signal_registry.failure.lock().unwrap(),
            Some(failure)
        );
        executor
            .file_table
            .lock()
            .unwrap()
            .files
            .insert(fd, carrier);
        assert_eq!(
            control.publish_alarm(id, alarm(id)),
            ProcessPublication::Rejected(PublicationRejection::Terminal)
        );
    }

    #[test]
    fn inactive_publication_concurrent_dequeue_keeps_readiness_equal_to_pending() {
        let mut executor = executor();
        let id = identity(&executor);
        let control = executor.signal_registry.control();
        let mut memory = GuestMemory::new(0, 4096).unwrap();
        let fd = signalfd(&mut executor, &mut memory);
        for _ in 0..64 {
            receipt(control.publish_alarm(id, alarm(id)));
            let start = std::sync::Barrier::new(2);
            let (published, removed) = std::thread::scope(|scope| {
                let publisher = scope.spawn(|| {
                    start.wait();
                    receipt(control.publish_alarm(id, alarm(id)))
                });
                let consumer = scope.spawn(|| {
                    start.wait();
                    executor
                        .take_pending_signal_for_delivery()
                        .unwrap()
                        .unwrap()
                });
                (publisher.join().unwrap(), consumer.join().unwrap())
            });
            assert_eq!(removed.event, alarm(id));
            let queued_after_remove = published.change == PendingChange::Queued;
            assert_eq!(
                executor
                    .state
                    .process_signals
                    .lock()
                    .unwrap()
                    .shared_pending
                    .contains(libc::SIGALRM),
                queued_after_remove
            );
            assert_eq!(ready(&executor, fd), queued_after_remove);
            if queued_after_remove {
                assert_eq!(
                    executor
                        .take_pending_signal_for_delivery()
                        .unwrap()
                        .unwrap()
                        .event,
                    alarm(id)
                );
            }
            assert!(!ready(&executor, fd));
        }
    }

    #[test]
    fn child_publication_requires_exact_generation_parent_status_and_waitability() {
        use reverie::ChildExitPublicationResult::RejectedBeforeCommit;

        let mut parent = executor();
        let parent_id = identity(&parent);
        let mut child = parent.fork_child(2, false, false).unwrap();
        let child_id = identity(&child);
        let wrong_parent = parent.fork_child(3, false, false).unwrap();
        let wrong_parent_id = identity(&wrong_parent);
        let control = parent.backend_signal_control().process;
        let completion =
            child_completion(parent_id, child_id, reverie::ExitStatus::Exited(23), true);

        assert_eq!(
            control.publish_child_exit(completion),
            RejectedBeforeCommit(Errno::EINVAL),
            "a live child has no committed terminal status"
        );
        child.retire_current_thread(reverie::ExitStatus::Exited(23), false);

        for (wrong, expected) in [
            (
                reverie::ChildExitCompletion {
                    child: SignalProcessId {
                        generation: child_id.generation + 1,
                        ..child_id
                    },
                    ..completion
                },
                Errno::EINVAL,
            ),
            (
                reverie::ChildExitCompletion {
                    parent: SignalProcessId {
                        generation: parent_id.generation + 1,
                        ..parent_id
                    },
                    ..completion
                },
                Errno::ESRCH,
            ),
            (
                reverie::ChildExitCompletion {
                    parent: wrong_parent_id,
                    ..completion
                },
                Errno::EINVAL,
            ),
            (
                reverie::ChildExitCompletion {
                    status: reverie::ExitStatus::Exited(24),
                    ..completion
                },
                Errno::EINVAL,
            ),
            (
                reverie::ChildExitCompletion {
                    waitable: false,
                    ..completion
                },
                Errno::EINVAL,
            ),
            (
                reverie::ChildExitCompletion {
                    status: reverie::ExitStatus::Exited(256),
                    ..completion
                },
                Errno::EINVAL,
            ),
            (
                reverie::ChildExitCompletion {
                    user_ticks: -1,
                    ..completion
                },
                Errno::EINVAL,
            ),
            (
                reverie::ChildExitCompletion {
                    system_ticks: -1,
                    ..completion
                },
                Errno::EINVAL,
            ),
        ] {
            assert_eq!(
                control.publish_child_exit(wrong),
                RejectedBeforeCommit(expected)
            );
        }

        let receipt = child_receipt(control.publish_child_exit(completion));
        assert_eq!(receipt.completion, completion);
        assert_eq!(receipt.effect, reverie::ChildExitPublicationEffect::Queued);
        let event = parent
            .take_pending_signal_for_delivery()
            .unwrap()
            .unwrap()
            .event;
        let info = event.siginfo();
        assert_eq!(event.signal(), libc::SIGCHLD);
        assert_eq!(i32::from_ne_bytes(info[16..20].try_into().unwrap()), 2);
        assert_eq!(u32::from_ne_bytes(info[20..24].try_into().unwrap()), 0);
        assert_eq!(i64::from_ne_bytes(info[32..40].try_into().unwrap()), 0);
        assert_eq!(i64::from_ne_bytes(info[40..48].try_into().unwrap()), 0);

        assert_eq!(
            child_receipt(control.publish_child_exit(completion)),
            receipt,
            "an exact duplicate returns the retained acknowledgement"
        );
        assert!(
            parent.take_pending_signal_for_delivery().unwrap().is_none(),
            "an acknowledged duplicate must not enqueue a second SIGCHLD"
        );
        assert_eq!(
            control.publish_child_exit(reverie::ChildExitCompletion {
                uid: completion.uid + 1,
                ..completion
            }),
            RejectedBeforeCommit(Errno::EINVAL),
            "conflicting data for one child generation must fail before mutation"
        );

        drop(child);
        drop(wrong_parent);
        let replacement = parent.fork_child(2, false, false).unwrap();
        assert_ne!(identity(&replacement).generation, child_id.generation);
        assert_eq!(
            child_receipt(control.publish_child_exit(completion)),
            receipt,
            "the exact acknowledgement survives executor retirement and numeric PID reuse"
        );
        assert!(parent.state.children.is_empty());
    }

    #[test]
    fn process_registry_keeps_reused_numeric_pids_generation_distinct() {
        let parent = executor();
        let mut old = parent.fork_child(2, false, false).unwrap();
        let old_id = identity(&old);
        old.retire_current_thread(reverie::ExitStatus::Exited(23), false);
        let replacement = parent.fork_child(2, false, false).unwrap();
        let replacement_id = identity(&replacement);
        assert_ne!(old_id.generation, replacement_id.generation);
        assert_eq!(
            parent.signal_registry.lookup(old_id).unwrap().identity,
            old_id
        );
        assert_eq!(
            parent
                .signal_registry
                .lookup(replacement_id)
                .unwrap()
                .identity,
            replacement_id
        );
        drop(old);
        assert!(parent.signal_registry.lookup(old_id).is_none());
        assert!(parent.signal_registry.lookup(replacement_id).is_some());
    }

    #[test]
    fn child_publication_encodes_exited_killed_and_core_statuses() {
        for (status, expected_code, expected_status) in [
            (reverie::ExitStatus::Exited(37), libc::CLD_EXITED, 37),
            (
                reverie::ExitStatus::Signaled(reverie::Signal::SIGTERM, false),
                libc::CLD_KILLED,
                libc::SIGTERM,
            ),
            (
                reverie::ExitStatus::Signaled(reverie::Signal::SIGABRT, true),
                libc::CLD_DUMPED,
                libc::SIGABRT,
            ),
        ] {
            let mut parent = executor();
            let mut child = parent.fork_child(2, false, false).unwrap();
            let completion = child_completion(identity(&parent), identity(&child), status, true);
            child.retire_current_thread(status, false);
            let receipt = child_receipt(
                parent
                    .backend_signal_control()
                    .process
                    .publish_child_exit(completion),
            );
            assert_eq!(receipt.completion.status, status);
            let event = parent
                .take_pending_signal_for_delivery()
                .unwrap()
                .unwrap()
                .event;
            let info = event.siginfo();
            assert_eq!(
                i32::from_ne_bytes(info[8..12].try_into().unwrap()),
                expected_code
            );
            assert_eq!(
                i32::from_ne_bytes(info[24..28].try_into().unwrap()),
                expected_status
            );
        }
    }

    #[test]
    fn child_publication_distinguishes_explicit_ignore_from_no_cldwait() {
        for (action, expected_effect, pending) in [
            (
                KernelSigaction {
                    handler: libc::SIG_IGN as u64,
                    ..Default::default()
                },
                reverie::ChildExitPublicationEffect::SuppressedExplicitIgnore,
                false,
            ),
            (
                KernelSigaction {
                    handler: libc::SIG_DFL as u64,
                    flags: libc::SA_NOCLDWAIT as u64,
                    ..Default::default()
                },
                reverie::ChildExitPublicationEffect::Queued,
                true,
            ),
        ] {
            let parent = executor();
            parent
                .state
                .process_signals
                .lock()
                .unwrap()
                .dispositions
                .insert(libc::SIGCHLD, action);
            let mut child = parent.fork_child(2, false, false).unwrap();
            let completion = child_completion(
                identity(&parent),
                identity(&child),
                reverie::ExitStatus::Exited(7),
                false,
            );
            child.retire_current_thread(completion.status, false);
            let control = parent.backend_signal_control().process;
            let receipt = child_receipt(control.publish_child_exit(completion));
            assert_eq!(receipt.effect, expected_effect);
            assert_eq!(
                child_receipt(control.publish_child_exit(completion)),
                receipt,
                "an exact duplicate must retain the original suppression/queue receipt"
            );
            assert_eq!(
                parent
                    .state
                    .process_signals
                    .lock()
                    .unwrap()
                    .shared_pending
                    .contains(libc::SIGCHLD),
                pending
            );
        }
    }

    #[test]
    fn child_publication_uses_exit_time_disposition_after_parent_policy_changes() {
        let install = |parent: &mut ElfExecutor, action: KernelSigaction| {
            let mut memory = GuestMemory::new(0, 4096).unwrap();
            memory.write(0x100, &action.encode()).unwrap();
            assert_eq!(
                call(
                    parent,
                    &memory,
                    libc::SYS_rt_sigaction,
                    [libc::SIGCHLD as u64, 0x100, 0, 8, 0, 0],
                ),
                0,
            );
        };

        let mut parent = executor();
        let mut child = parent.fork_child(2, false, false).unwrap();
        let completion = child_completion(
            identity(&parent),
            identity(&child),
            reverie::ExitStatus::Exited(7),
            true,
        );
        child.retire_current_thread(completion.status, false);
        let exit_generation = match child
            .signal_registry
            .process_family_exit(completion.child)
            .unwrap()
        {
            ProcessFamilyExit::Child(snapshot) => snapshot.pending_generation,
            other => panic!("unexpected child family exit: {other:?}"),
        };
        install(
            &mut parent,
            KernelSigaction {
                handler: libc::SIG_IGN as u64,
                ..Default::default()
            },
        );
        let receipt = child_receipt(
            parent
                .backend_signal_control()
                .process
                .publish_child_exit(completion),
        );
        assert_eq!(receipt.completion, completion);
        assert_eq!(receipt.pending_generation, exit_generation);
        assert_eq!(
            receipt.effect,
            reverie::ChildExitPublicationEffect::DiscardedByDispositionChange
        );
        assert!(
            !parent
                .state
                .process_signals
                .lock()
                .unwrap()
                .shared_pending
                .contains(libc::SIGCHLD),
            "a delayed child event cannot resurrect the discarded generation",
        );

        for (action, expected_effect) in [
            (
                KernelSigaction {
                    handler: libc::SIG_IGN as u64,
                    ..Default::default()
                },
                reverie::ChildExitPublicationEffect::SuppressedExplicitIgnore,
            ),
            (
                KernelSigaction {
                    handler: libc::SIG_DFL as u64,
                    flags: libc::SA_NOCLDWAIT as u64,
                    ..Default::default()
                },
                reverie::ChildExitPublicationEffect::DiscardedByDispositionChange,
            ),
        ] {
            let mut parent = executor();
            install(&mut parent, action);
            let mut child = parent.fork_child(2, false, false).unwrap();
            let completion = child_completion(
                identity(&parent),
                identity(&child),
                reverie::ExitStatus::Exited(9),
                false,
            );
            child.retire_current_thread(completion.status, false);
            install(&mut parent, KernelSigaction::default());
            let receipt = child_receipt(
                parent
                    .backend_signal_control()
                    .process
                    .publish_child_exit(completion),
            );
            assert_eq!(receipt.completion, completion);
            assert_eq!(receipt.effect, expected_effect);
        }
    }

    #[test]
    fn live_orphan_is_adopted_by_guest_init_and_reaped_with_its_status() {
        const STATUS: u64 = 0x200;

        let mut root = executor_with_root(1);
        let root_id = identity(&root);
        let mut owner = root.fork_child(2, false, false).unwrap();
        let mut orphan = owner.fork_child(3, false, false).unwrap();
        let owner_id = identity(&owner);
        let orphan_id = identity(&orphan);
        assert_eq!(getppid(&mut orphan), 2);

        owner.retire_current_thread(reverie::ExitStatus::Exited(7), false);
        assert!(matches!(
            owner.signal_registry.process_family_exit(owner_id),
            Some(ProcessFamilyExit::Child(snapshot)) if snapshot.completion.parent == root_id
        ));
        // Linux exit_notify: the running child now belongs to namespace init.
        assert_eq!(getppid(&mut orphan), 1);
        assert_eq!(
            root.signal_registry.current_parent(orphan_id),
            Some(root_id)
        );
        assert_eq!(
            family_edge(&root.signal_registry, root_id, orphan_id),
            Some(DirectChildState::Live)
        );
        assert_eq!(
            family_edge(&root.signal_registry, owner_id, orphan_id),
            None
        );

        // Collect the exiting owner first so only the orphan remains.
        root.record_child_completion(
            2,
            crate::executor::ChildCompletion::Waitable(reverie::ExitStatus::Exited(7)),
        )
        .unwrap();
        let memory = GuestMemory::new(0, 4096).unwrap();
        assert_eq!(
            root.execute(
                &SyscallRequest::new(libc::SYS_wait4 as u64, [2, 0, 0, 0, 0, 0]),
                &memory,
            ),
            2
        );
        // The adopted orphan is still running: a non-blocking wait reports
        // "none ready" rather than ECHILD.
        assert_eq!(
            root.execute(
                &SyscallRequest::new(
                    libc::SYS_wait4 as u64,
                    [u64::MAX, STATUS, libc::WNOHANG as u64, 0, 0, 0],
                ),
                &memory,
            ),
            0
        );

        orphan.retire_current_thread(reverie::ExitStatus::Exited(9), false);
        let completion = match orphan.signal_registry.process_family_exit(orphan_id) {
            Some(ProcessFamilyExit::Child(snapshot)) => snapshot.completion,
            other => panic!("adopted orphan lost its waitable exit: {other:?}"),
        };
        assert_eq!(completion.parent, root_id);
        assert_eq!(completion.child, orphan_id);
        assert_eq!(completion.status, reverie::ExitStatus::Exited(9));
        assert!(completion.waitable);
        // Not collectable until the runtime's post-fence delivery.
        assert_eq!(
            root.execute(
                &SyscallRequest::new(
                    libc::SYS_wait4 as u64,
                    [u64::MAX, STATUS, libc::WNOHANG as u64, 0, 0, 0],
                ),
                &memory,
            ),
            0
        );
        assert!(orphan.publish_adopted_child_completion(
            root_id,
            crate::executor::ChildCompletion::Waitable(completion.status),
        ));
        assert!(
            !orphan.publish_adopted_child_completion(
                root_id,
                crate::executor::ChildCompletion::Waitable(completion.status),
            ),
            "an adopted completion is delivered at most once",
        );
        assert_eq!(
            root.execute(
                &SyscallRequest::new(libc::SYS_wait4 as u64, [u64::MAX, STATUS, 0, 0, 0, 0]),
                &memory,
            ),
            3
        );
        let mut status = [0; std::mem::size_of::<libc::c_int>()];
        memory.read(STATUS, &mut status).unwrap();
        assert_eq!(libc::c_int::from_le_bytes(status), 9 << 8);
        assert_eq!(family_edge(&root.signal_registry, root_id, orphan_id), None);
        assert_eq!(
            root.execute(
                &SyscallRequest::new(libc::SYS_wait4 as u64, [u64::MAX, 0, 0, 0, 0, 0]),
                &memory,
            ),
            -i64::from(libc::ECHILD)
        );
    }

    #[test]
    fn blocked_adopter_wait_wakes_on_adopted_orphan_completion() {
        const STATUS: u64 = 0x200;

        let root = executor_with_root(1);
        let root_id = identity(&root);
        let mut owner = root.fork_child(2, false, false).unwrap();
        let mut orphan = owner.fork_child(3, false, false).unwrap();
        owner.retire_current_thread(reverie::ExitStatus::Exited(7), false);
        let waiter = std::thread::spawn(move || {
            let mut root = root;
            let memory = GuestMemory::new(0, 4096).unwrap();
            // Only the orphan: the owner's own completion is never joined
            // into this executor, so the pid-specific wait blocks on pid 3.
            let reaped = root.execute(
                &SyscallRequest::new(libc::SYS_wait4 as u64, [3, STATUS, 0, 0, 0, 0]),
                &memory,
            );
            let mut status = [0; std::mem::size_of::<libc::c_int>()];
            memory.read(STATUS, &mut status).unwrap();
            (reaped, libc::c_int::from_le_bytes(status))
        });
        orphan.retire_current_thread(reverie::ExitStatus::Exited(9), false);
        assert!(orphan.publish_adopted_child_completion(
            root_id,
            crate::executor::ChildCompletion::Waitable(reverie::ExitStatus::Exited(9)),
        ));
        assert_eq!(waiter.join().unwrap(), (3, 9 << 8));
    }

    #[test]
    fn adopted_orphan_sigchld_is_published_to_its_adopter() {
        let root = executor_with_root(1);
        let root_id = identity(&root);
        let mut owner = root.fork_child(2, false, false).unwrap();
        let mut orphan = owner.fork_child(3, false, false).unwrap();
        let owner_id = identity(&owner);
        let orphan_id = identity(&orphan);
        owner.retire_current_thread(reverie::ExitStatus::Exited(7), false);
        orphan.retire_current_thread(reverie::ExitStatus::Exited(9), false);
        let completion = child_completion(root_id, orphan_id, reverie::ExitStatus::Exited(9), true);
        assert!(matches!(
            orphan.signal_registry.process_family_exit(orphan_id),
            Some(ProcessFamilyExit::Child(snapshot)) if snapshot.completion == completion
        ));
        // A publication naming the dead fork parent is not this child's.
        let stale = child_completion(owner_id, orphan_id, completion.status, true);
        assert!(!matches!(
            owner
                .backend_signal_control()
                .process
                .publish_child_exit(stale),
            reverie::ChildExitPublicationResult::Committed(_)
        ));
        let receipt = child_receipt(
            root.backend_signal_control()
                .process
                .publish_child_exit(completion),
        );
        assert_eq!(receipt.completion, completion);
        assert!(
            root.state
                .process_signals
                .lock()
                .unwrap()
                .shared_pending
                .contains(libc::SIGCHLD),
            "the adopting init receives the orphan's SIGCHLD",
        );
    }

    #[test]
    fn adopted_orphan_auto_reaps_under_adopter_sigchld_policy() {
        for action in [
            KernelSigaction {
                handler: libc::SIG_IGN as u64,
                ..Default::default()
            },
            KernelSigaction {
                handler: libc::SIG_DFL as u64,
                flags: libc::SA_NOCLDWAIT as u64,
                ..Default::default()
            },
        ] {
            let mut root = executor_with_root(1);
            let root_id = identity(&root);
            root.state
                .process_signals
                .lock()
                .unwrap()
                .dispositions
                .insert(libc::SIGCHLD, action);
            let mut owner = root.fork_child(2, false, false).unwrap();
            let mut orphan = owner.fork_child(3, false, false).unwrap();
            let orphan_id = identity(&orphan);
            owner.retire_current_thread(reverie::ExitStatus::Exited(7), false);
            orphan.retire_current_thread(reverie::ExitStatus::Exited(9), false);
            assert!(matches!(
                orphan.signal_registry.process_family_exit(orphan_id),
                Some(ProcessFamilyExit::Child(snapshot))
                    if snapshot.completion.parent == root_id && !snapshot.completion.waitable
            ));
            assert_eq!(family_edge(&root.signal_registry, root_id, orphan_id), None);
            assert!(orphan.publish_adopted_child_completion(
                root_id,
                crate::executor::ChildCompletion::AutoReaped(reverie::ExitStatus::Exited(9)),
            ));
            let memory = GuestMemory::new(0, 4096).unwrap();
            // The owner itself was auto-reaped too; nothing is left to wait for.
            assert_eq!(
                root.execute(
                    &SyscallRequest::new(libc::SYS_wait4 as u64, [u64::MAX, 0, 0, 0, 0, 0]),
                    &memory,
                ),
                -i64::from(libc::ECHILD)
            );
        }
    }

    #[test]
    fn orphans_go_to_init_outside_the_guest_when_root_is_not_pid_1() {
        let root = executor_with_root(3);
        let root_id = identity(&root);
        let mut owner = root.fork_child(4, false, false).unwrap();
        let mut orphan = owner.fork_child(5, false, false).unwrap();
        let mut zombie = owner.fork_child(6, false, false).unwrap();
        let owner_id = identity(&owner);
        let orphan_id = identity(&orphan);
        let zombie_id = identity(&zombie);
        zombie.retire_current_thread(reverie::ExitStatus::Exited(8), false);
        assert_eq!(
            family_edge(&root.signal_registry, owner_id, zombie_id),
            Some(DirectChildState::WaitableZombie)
        );
        assert_eq!(getppid(&mut orphan), 4);

        owner.retire_current_thread(reverie::ExitStatus::Exited(7), false);
        assert!(matches!(
            owner.signal_registry.process_family_exit(owner_id),
            Some(ProcessFamilyExit::Child(snapshot)) if snapshot.completion.parent == root_id
        ));
        assert_eq!(getppid(&mut orphan), 1);
        assert!(
            !root
                .signal_registry
                .family
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .direct_children
                .contains_key(&process_key(owner_id)),
            "init outside the guest reaps the zombie and takes the running orphan",
        );
        orphan.retire_current_thread(reverie::ExitStatus::Exited(9), false);
        assert_eq!(
            orphan.signal_registry.process_family_exit(orphan_id),
            Some(ProcessFamilyExit::RunTeardownChild {
                status: reverie::ExitStatus::Exited(9),
            }),
        );
    }

    #[test]
    fn zombie_adoption_by_guest_init_is_an_explicit_refusal() {
        let root = executor_with_root(1);
        let root_id = identity(&root);
        let mut zombie_owner = root.fork_child(2, false, false).unwrap();
        let mut zombie = zombie_owner.fork_child(3, false, false).unwrap();
        let zombie_owner_id = identity(&zombie_owner);
        let zombie_id = identity(&zombie);
        zombie.retire_current_thread(reverie::ExitStatus::Exited(9), false);
        zombie_owner.retire_current_thread(reverie::ExitStatus::Exited(7), false);
        assert_eq!(
            zombie_owner
                .signal_registry
                .process_family_exit(zombie_owner_id),
            Some(ProcessFamilyExit::ZombieAdoptionUnsupported {
                child: zombie_id,
                reaper: root_id,
            })
        );
        assert!(matches!(
            zombie_owner.process_family_exit(),
            Err(crate::Error::ZombieAdoptionUnsupported { process, child, reaper })
                if process == zombie_owner_id && child == zombie_id && reaper == root_id
        ));
        assert_eq!(
            family_edge(&root.signal_registry, zombie_owner_id, zombie_id),
            Some(DirectChildState::WaitableZombie),
            "the refused transition moves no edge",
        );
    }

    #[test]
    fn process_family_retires_reaped_and_auto_reaped_children() {
        let parent = executor();
        let mut reaping_owner = parent.fork_child(2, false, false).unwrap();
        let mut reaped = reaping_owner.fork_child(3, false, false).unwrap();
        let reaping_owner_id = identity(&reaping_owner);
        reaped.retire_current_thread(reverie::ExitStatus::Exited(9), false);
        assert!(
            reaping_owner
                .signal_registry
                .consume_child_wait(reaping_owner_id, 3)
        );
        reaping_owner.retire_current_thread(reverie::ExitStatus::Exited(7), false);
        assert!(matches!(
            reaping_owner
                .signal_registry
                .process_family_exit(reaping_owner_id),
            Some(ProcessFamilyExit::Child(_))
        ));

        for action in [
            KernelSigaction {
                handler: libc::SIG_IGN as u64,
                ..Default::default()
            },
            KernelSigaction {
                handler: libc::SIG_DFL as u64,
                flags: libc::SA_NOCLDWAIT as u64,
                ..Default::default()
            },
        ] {
            let parent = executor();
            let mut owner = parent.fork_child(2, false, false).unwrap();
            owner
                .state
                .process_signals
                .lock()
                .unwrap()
                .dispositions
                .insert(libc::SIGCHLD, action);
            let mut auto_reaped = owner.fork_child(3, false, false).unwrap();
            let owner_id = identity(&owner);
            auto_reaped.retire_current_thread(reverie::ExitStatus::Exited(9), false);
            owner.retire_current_thread(reverie::ExitStatus::Exited(7), false);
            assert!(matches!(
                owner.signal_registry.process_family_exit(owner_id),
                Some(ProcessFamilyExit::Child(_))
            ));
        }
    }

    #[test]
    fn terminal_root_preserves_a_live_parent_wait_before_direct_parent_teardown() {
        const INFO: u64 = 0x100;
        const STATUS: u64 = 0x200;

        let mut root = executor();
        let mut child = root.fork_child(2, false, false).unwrap();
        let mut grandchild = child.fork_child(3, false, false).unwrap();
        let root_id = identity(&root);
        let child_id = identity(&child);
        let grandchild_id = identity(&grandchild);

        root.retire_current_thread(reverie::ExitStatus::Exited(1), false);
        assert_eq!(
            root.signal_registry.process_family_exit(root_id),
            Some(ProcessFamilyExit::Root),
        );
        grandchild.retire_current_thread(reverie::ExitStatus::Exited(3), false);
        let completion = match grandchild
            .signal_registry
            .process_family_exit(grandchild_id)
            .unwrap()
        {
            ProcessFamilyExit::Child(snapshot) => snapshot.completion,
            other => panic!("live direct parent lost its waitable child: {other:?}"),
        };
        assert_eq!(completion.parent, child_id);
        assert_eq!(completion.child, grandchild_id);
        assert_eq!(completion.status, reverie::ExitStatus::Exited(3));
        assert!(completion.waitable);
        child
            .record_child_completion(
                3,
                crate::executor::ChildCompletion::from_waitability(
                    completion.status,
                    completion.waitable,
                ),
            )
            .unwrap();

        let memory = GuestMemory::new(0, 4096).unwrap();
        assert_eq!(
            child.execute(
                &SyscallRequest::new(
                    libc::SYS_waitid as u64,
                    [
                        libc::P_PID as u64,
                        3,
                        INFO,
                        (libc::WEXITED | libc::WNOWAIT | libc::WNOHANG) as u64,
                        0,
                        0,
                    ],
                ),
                &memory,
            ),
            0,
        );
        let info: libc::siginfo_t = read_struct(&memory, INFO);
        // SAFETY: waitid writes the SIGCHLD variant of siginfo_t.
        unsafe {
            assert_eq!(info.si_pid(), 3);
            assert_eq!(info.si_status(), 3);
        }
        assert!(
            child
                .signal_registry
                .family
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .direct_children
                .get(&process_key(child_id))
                .is_some_and(|children| children.get(&process_key(grandchild_id))
                    == Some(&DirectChildState::WaitableZombie)),
            "WNOWAIT must preserve the exact waitable family edge",
        );
        assert_eq!(
            child.execute(
                &SyscallRequest::new(
                    libc::SYS_wait4 as u64,
                    [3, STATUS, libc::WNOHANG as u64, 0, 0, 0],
                ),
                &memory,
            ),
            3,
        );
        let mut status = [0; std::mem::size_of::<libc::c_int>()];
        memory.read(STATUS, &mut status).unwrap();
        assert_eq!(libc::c_int::from_le_bytes(status), 3 << 8);

        child.retire_current_thread(reverie::ExitStatus::Exited(2), false);
        assert_eq!(
            child.signal_registry.process_family_exit(child_id),
            Some(ProcessFamilyExit::RunTeardownChild {
                status: reverie::ExitStatus::Exited(2),
            }),
        );
        assert!(
            root.signal_registry
                .family
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .direct_children
                .is_empty(),
            "direct-parent teardown must retire every exact child edge",
        );
    }

    #[test]
    fn terminal_direct_parent_tears_down_a_later_child_without_publication() {
        let mut root = executor();
        let mut child = root.fork_child(2, false, false).unwrap();
        let mut grandchild = child.fork_child(3, false, false).unwrap();
        let child_id = identity(&child);
        let grandchild_id = identity(&grandchild);

        root.retire_current_thread(reverie::ExitStatus::Exited(1), false);
        child.retire_current_thread(reverie::ExitStatus::Exited(2), false);
        assert_eq!(
            child.signal_registry.process_family_exit(child_id),
            Some(ProcessFamilyExit::RunTeardownChild {
                status: reverie::ExitStatus::Exited(2),
            }),
        );
        grandchild.retire_current_thread(reverie::ExitStatus::Exited(3), false);
        assert_eq!(
            grandchild
                .signal_registry
                .process_family_exit(grandchild_id),
            Some(ProcessFamilyExit::RunTeardownChild {
                status: reverie::ExitStatus::Exited(3),
            }),
        );
        assert!(
            root.signal_registry
                .family
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .direct_children
                .is_empty(),
        );
    }

    #[test]
    fn live_root_adopts_but_terminal_root_consumes_the_same_unreaped_family_shape() {
        let root = executor();
        let root_id = identity(&root);
        let mut child = root.fork_child(2, false, false).unwrap();
        let grandchild = child.fork_child(3, false, false).unwrap();
        let child_id = identity(&child);
        let grandchild_id = identity(&grandchild);

        child.retire_current_thread(reverie::ExitStatus::Exited(2), false);
        assert!(
            matches!(
                child.signal_registry.process_family_exit(child_id),
                Some(ProcessFamilyExit::Child(snapshot)) if snapshot.completion.parent == root_id
            ),
            "a middle process that exits under a live PID 1 root hands its child to that root",
        );
        assert_eq!(
            family_edge(&root.signal_registry, root_id, grandchild_id),
            Some(DirectChildState::Live)
        );

        let mut root = executor();
        let mut child = root.fork_child(2, false, false).unwrap();
        let mut grandchild = child.fork_child(3, false, false).unwrap();
        let root_id = identity(&root);
        let child_id = identity(&child);
        let grandchild_id = identity(&grandchild);

        root.retire_current_thread(reverie::ExitStatus::Exited(1), false);
        assert_eq!(
            root.signal_registry.process_family_exit(root_id),
            Some(ProcessFamilyExit::Root),
        );
        child.retire_current_thread(reverie::ExitStatus::Exited(2), false);
        assert_eq!(
            child.signal_registry.process_family_exit(child_id),
            Some(ProcessFamilyExit::RunTeardownChild {
                status: reverie::ExitStatus::Exited(2),
            }),
            "after the root becomes terminal, the same middle exit belongs to whole-run teardown",
        );
        grandchild.retire_current_thread(reverie::ExitStatus::Exited(3), false);
        assert_eq!(
            grandchild
                .signal_registry
                .process_family_exit(grandchild_id),
            Some(ProcessFamilyExit::RunTeardownChild {
                status: reverie::ExitStatus::Exited(3),
            }),
        );
        assert!(
            root.signal_registry
                .family
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .direct_children
                .is_empty(),
            "terminal-root consumption must retire the same family's exact edges",
        );
    }

    #[test]
    fn child_exit_distinguishes_lost_parent_generation_from_lost_family_edge() {
        let parent = executor();
        let parent_id = identity(&parent);
        let mut child = parent.fork_child(2, false, false).unwrap();
        let child_id = identity(&child);
        drop(parent);

        child.retire_current_thread(reverie::ExitStatus::Exited(7), false);
        assert_eq!(
            child.signal_registry.process_family_exit(child_id),
            Some(ProcessFamilyExit::ParentGenerationUnavailable { parent: parent_id }),
        );
        assert!(matches!(
            child.process_family_exit(),
            Err(crate::Error::ParentGenerationUnavailable { process, parent })
                if process == child_id && parent == parent_id
        ));
        child.signal_registry.record_process_failure(child_id);
        assert_eq!(
            child.signal_registry.process_family_exit(child_id),
            Some(ProcessFamilyExit::ParentGenerationUnavailable { parent: parent_id }),
            "a later generic failure must preserve the missing-generation cause",
        );

        let parent = executor();
        let parent_id = identity(&parent);
        let mut child = parent.fork_child(2, false, false).unwrap();
        let child_id = identity(&child);
        {
            let mut family = parent
                .signal_registry
                .family
                .lock()
                .unwrap_or_else(|p| p.into_inner());
            let children = family
                .direct_children
                .get_mut(&process_key(parent_id))
                .expect("fork registered an exact parent family edge");
            assert_eq!(
                children.remove(&process_key(child_id)),
                Some(DirectChildState::Live)
            );
            if children.is_empty() {
                family.direct_children.remove(&process_key(parent_id));
            }
        }

        child.retire_current_thread(reverie::ExitStatus::Exited(9), false);
        assert_eq!(
            child.signal_registry.process_family_exit(child_id),
            Some(ProcessFamilyExit::ParentChildRelationUnavailable { parent: parent_id }),
        );
        assert!(matches!(
            child.process_family_exit(),
            Err(crate::Error::ParentChildRelationUnavailable { process, parent })
                if process == child_id && parent == parent_id
        ));
        child.signal_registry.record_process_failure(child_id);
        assert_eq!(
            child.signal_registry.process_family_exit(child_id),
            Some(ProcessFamilyExit::ParentChildRelationUnavailable { parent: parent_id }),
            "a later generic failure must preserve the missing-relation cause",
        );
    }

    #[test]
    fn child_wait_consumes_the_exact_family_child_selected_by_the_backend() {
        const INFO: u64 = 0x100;

        for mode in 0..5 {
            let mut parent = executor();
            let parent_id = identity(&parent);
            let mut low = parent.fork_child(2, false, false).unwrap();
            let mut high = parent.fork_child(3, false, false).unwrap();
            let low_id = identity(&low);
            let high_id = identity(&high);
            low.retire_current_thread(reverie::ExitStatus::Exited(2), false);
            high.retire_current_thread(reverie::ExitStatus::Exited(3), false);
            parent
                .state
                .children
                .insert(2, reverie::ExitStatus::Exited(2));
            parent
                .state
                .children
                .insert(3, reverie::ExitStatus::Exited(3));

            let (id_type, id, expected, consume, use_wait4) = match mode {
                0 => (libc::P_ALL, 0, low_id, true, false),
                1 => (libc::P_PGID, parent.state.pgid, low_id, true, false),
                2 => (libc::P_PID, high_id.tgid.as_raw(), high_id, true, false),
                3 => (libc::P_ALL, 0, low_id, false, false),
                4 => (libc::P_ALL, 0, low_id, true, true),
                _ => unreachable!(),
            };
            let memory = GuestMemory::new(0, 4096).unwrap();
            let selected = if use_wait4 {
                let selected = call(
                    &mut parent,
                    &memory,
                    libc::SYS_wait4,
                    [u64::from(u32::MAX), INFO, libc::WNOHANG as u64, 0, 0, 0],
                );
                libc::pid_t::try_from(selected).expect("wait4 returned the selected child pid")
            } else {
                let flags = libc::WEXITED | libc::WNOHANG | if consume { 0 } else { libc::WNOWAIT };
                assert_eq!(
                    call(
                        &mut parent,
                        &memory,
                        libc::SYS_waitid,
                        [id_type as u64, id as u64, INFO, flags as u64, 0, 0],
                    ),
                    0,
                );
                let info: libc::siginfo_t = read_struct(&memory, INFO);
                // SAFETY: waitid wrote the SIGCHLD variant of siginfo_t.
                unsafe { info.si_pid() }
            };
            assert_eq!(selected, expected.tgid.as_raw());

            let family = parent
                .signal_registry
                .family
                .lock()
                .unwrap_or_else(|p| p.into_inner());
            let children = family
                .direct_children
                .get(&process_key(parent_id))
                .expect("at least one waitable child remains");
            for child in [low_id, high_id] {
                let remains = !consume || child != expected;
                assert_eq!(
                    children.get(&process_key(child)).copied(),
                    remains.then_some(DirectChildState::WaitableZombie),
                    "family-ledger removal must follow the pid actually returned by the backend wait",
                );
                assert_eq!(
                    parent.state.children.contains_key(&child.tgid.as_raw()),
                    remains,
                    "backend wait state and exact-generation family state must change together",
                );
            }
        }
    }

    #[test]
    fn controlled_child_wait_fails_closed_without_an_exact_family_zombie() {
        let mut parent = executor();
        let parent_id = identity(&parent);
        parent
            .signal_registry
            .controlled
            .store(true, Ordering::Release);
        parent
            .state
            .children
            .insert(2, reverie::ExitStatus::Exited(2));
        let memory = GuestMemory::new(0, 4096).unwrap();
        let error = parent
            .execute_checked(
                &SyscallRequest::new(
                    libc::SYS_wait4 as u64,
                    [u64::from(u32::MAX), 0, libc::WNOHANG as u64, 0, 0, 0],
                ),
                &memory,
            )
            .unwrap_err();
        assert!(matches!(
            error,
            crate::Error::FamilyWaitLedgerMismatch {
                parent,
                child_pid: 2,
            } if parent == parent_id
        ));
        assert!(
            !parent.state.children.contains_key(&2),
            "the typed failure must retain that the backend already reaped the numeric child",
        );
        assert!(parent.state.consumed_child_wait.is_none());
    }

    #[test]
    fn stale_child_wait_effect_refuses_before_another_syscall_dispatch() {
        let mut parent = executor();
        let parent_id = identity(&parent);
        parent.state.consumed_child_wait = Some(17);
        let memory = GuestMemory::new(0, 4096).unwrap();
        let error = parent
            .execute_checked(
                &SyscallRequest::new(libc::SYS_getpid as u64, [0; 6]),
                &memory,
            )
            .unwrap_err();
        assert!(matches!(
            error,
            crate::Error::ChildWaitLedgerEffectPending {
                parent,
                child_pid: 17,
            } if parent == parent_id
        ));
        assert_eq!(parent.state.consumed_child_wait, Some(17));
    }

    #[test]
    fn corrupt_family_ancestry_is_typed_instead_of_panicking_or_choosing_a_parent() {
        let root = executor();
        let parent = root.fork_child(2, false, false).unwrap();
        let mut child = parent.fork_child(3, false, false).unwrap();
        let root_id = identity(&root);
        let parent_id = identity(&parent);
        let child_id = identity(&child);
        parent
            .signal_registry
            .family
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .direct_children
            .get_mut(&process_key(parent_id))
            .expect("parent owns its real child edge")
            .insert(process_key(root_id), DirectChildState::Live);

        child.retire_current_thread(reverie::ExitStatus::Exited(3), false);
        assert_eq!(
            child.signal_registry.process_family_exit(child_id),
            Some(ProcessFamilyExit::AncestryCycle {
                ancestor: parent_id,
            }),
        );
        assert!(matches!(
            child.process_family_exit(),
            Err(crate::Error::ProcessFamilyAncestryCycle { process, ancestor })
                if process == child_id && ancestor == parent_id
        ));

        let root = executor();
        let parent = root.fork_child(2, false, false).unwrap();
        let mut child = parent.fork_child(3, false, false).unwrap();
        let root_id = identity(&root);
        let parent_id = identity(&parent);
        let child_id = identity(&child);
        let alternate_parent = SignalProcessId {
            tgid: reverie::Pid::from_raw(99),
            generation: 99,
        };
        parent
            .signal_registry
            .family
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .direct_children
            .entry(process_key(alternate_parent))
            .or_default()
            .insert(process_key(parent_id), DirectChildState::Live);

        child.retire_current_thread(reverie::ExitStatus::Exited(3), false);
        assert_eq!(
            child.signal_registry.process_family_exit(child_id),
            Some(ProcessFamilyExit::MultipleParents {
                child: parent_id,
                first_parent: root_id,
                second_parent: alternate_parent,
            }),
        );
        assert!(matches!(
            child.process_family_exit(),
            Err(crate::Error::ProcessFamilyMultipleParents {
                process,
                child,
                first_parent,
                second_parent,
            }) if process == child_id
                && child == parent_id
                && first_parent == root_id
                && second_parent == alternate_parent
        ));
    }

    #[test]
    fn group_exit_records_family_order_before_peer_host_retirement() {
        let actions = [
            KernelSigaction {
                handler: libc::SIG_IGN as u64,
                ..Default::default()
            },
            KernelSigaction {
                handler: libc::SIG_DFL as u64,
                flags: libc::SA_NOCLDWAIT as u64,
                ..Default::default()
            },
        ];

        for action in actions {
            for owner_finishes_first in [false, true] {
                let parent = executor();
                let parent_id = identity(&parent);
                let mut owner = parent.fork_child(2, false, false).unwrap();
                owner
                    .state
                    .process_signals
                    .lock()
                    .unwrap()
                    .dispositions
                    .insert(libc::SIGCHLD, action);
                let mut owner_peer = owner.thread_child(4).unwrap();
                let mut descendant = owner.fork_child(3, false, false).unwrap();
                let owner_id = identity(&owner);
                let descendant_id = identity(&descendant);

                owner_peer.retire_current_thread(reverie::ExitStatus::Exited(7), true);
                let owner_exit = owner.signal_registry.process_family_exit(owner_id);
                assert!(
                    matches!(
                        owner_exit,
                        Some(ProcessFamilyExit::Child(snapshot))
                            if snapshot.completion.parent == parent_id
                    ),
                    "the ordered exit_group transition must not wait for peer host cancellation",
                );
                assert_eq!(
                    getppid(&mut descendant),
                    1,
                    "the orphan is reparented at the ordered exit_group transition",
                );
                assert!(
                    owner.fork_child(6, false, false).is_err(),
                    "a peer cannot admit a new process after the family exit is frozen",
                );
                assert!(
                    !owner
                        .state
                        .task_lifecycle
                        .lock()
                        .unwrap()
                        .processes()
                        .any(|(tgid, _)| tgid == 6),
                    "refused late registration must retire its provisional lifecycle entry",
                );

                if owner_finishes_first {
                    owner.retire_current_thread(reverie::ExitStatus::SUCCESS, false);
                    descendant.retire_current_thread(reverie::ExitStatus::Exited(9), false);
                } else {
                    descendant.retire_current_thread(reverie::ExitStatus::Exited(9), false);
                    owner.retire_current_thread(reverie::ExitStatus::SUCCESS, false);
                }
                // The owner's own SIGCHLD policy no longer governs the
                // orphan: its adopter's default disposition keeps it waitable.
                assert!(matches!(
                    descendant
                        .signal_registry
                        .process_family_exit(descendant_id),
                    Some(ProcessFamilyExit::Child(snapshot))
                        if snapshot.completion.parent == parent_id
                            && snapshot.completion.waitable
                            && snapshot.completion.status == reverie::ExitStatus::Exited(9)
                ));
                assert_eq!(
                    owner.signal_registry.process_family_exit(owner_id),
                    owner_exit,
                    "physical peer order cannot replace the ordered family outcome",
                );
            }

            for owner_finishes_first in [false, true] {
                let parent = executor();
                let mut owner = parent.fork_child(2, false, false).unwrap();
                owner
                    .state
                    .process_signals
                    .lock()
                    .unwrap()
                    .dispositions
                    .insert(libc::SIGCHLD, action);
                let mut owner_peer = owner.thread_child(4).unwrap();
                let mut descendant = owner.fork_child(3, false, false).unwrap();
                let mut descendant_peer = descendant.thread_child(5).unwrap();
                let owner_id = identity(&owner);
                let descendant_id = identity(&descendant);

                descendant_peer.retire_current_thread(reverie::ExitStatus::Exited(9), true);
                assert!(matches!(
                    descendant
                        .signal_registry
                        .process_family_exit(descendant_id),
                    Some(ProcessFamilyExit::Child(ChildExitSnapshot {
                        completion: reverie::ChildExitCompletion {
                            waitable: false,
                            ..
                        },
                        ..
                    }))
                ));

                owner_peer.retire_current_thread(reverie::ExitStatus::Exited(7), true);
                assert!(matches!(
                    owner.signal_registry.process_family_exit(owner_id),
                    Some(ProcessFamilyExit::Child(_))
                ));

                if owner_finishes_first {
                    owner.retire_current_thread(reverie::ExitStatus::SUCCESS, false);
                    descendant.retire_current_thread(reverie::ExitStatus::SUCCESS, false);
                } else {
                    descendant.retire_current_thread(reverie::ExitStatus::SUCCESS, false);
                    owner.retire_current_thread(reverie::ExitStatus::SUCCESS, false);
                }
                assert!(matches!(
                    descendant
                        .signal_registry
                        .process_family_exit(descendant_id),
                    Some(ProcessFamilyExit::Child(_))
                ));
                assert!(matches!(
                    owner.signal_registry.process_family_exit(owner_id),
                    Some(ProcessFamilyExit::Child(_))
                ));
            }
        }
    }

    #[test]
    fn failed_process_family_is_monotonic_across_peer_retirement_orders() {
        for leader_first in [false, true] {
            for leader_group_exit in [false, true] {
                let parent = executor();
                let mut owner = parent.fork_child(2, false, false).unwrap();
                let mut worker = owner.thread_child(4).unwrap();
                let mut descendant = owner.fork_child(3, false, false).unwrap();
                let owner_id = identity(&owner);
                let descendant_id = identity(&descendant);

                if leader_first {
                    owner.retire_current_thread(reverie::ExitStatus::Exited(7), false);
                    assert_eq!(
                        owner.signal_registry.process_family_exit(owner_id),
                        None,
                        "a surviving peer keeps the process logically live",
                    );
                    worker.retire_failed_thread();
                } else {
                    worker.retire_failed_thread();
                    owner.retire_current_thread(reverie::ExitStatus::Exited(7), leader_group_exit);
                }

                assert_eq!(
                    owner.signal_registry.process_family_exit(owner_id),
                    Some(ProcessFamilyExit::Failed),
                    "later success or exit_group cannot replace exact failure",
                );
                assert!(
                    owner.fork_child(6, false, false).is_err(),
                    "a failed process cannot admit another child",
                );
                descendant.retire_current_thread(reverie::ExitStatus::Exited(9), false);
                assert_eq!(
                    descendant
                        .signal_registry
                        .process_family_exit(descendant_id),
                    Some(ProcessFamilyExit::RunTeardownChild {
                        status: reverie::ExitStatus::Exited(9),
                    }),
                );
            }
        }

        let parent = executor();
        let mut owner = parent.fork_child(2, false, false).unwrap();
        let mut peer = owner.thread_child(4).unwrap();
        let owner_id = identity(&owner);
        peer.retire_current_thread(reverie::ExitStatus::Exited(7), true);
        assert!(matches!(
            owner.signal_registry.process_family_exit(owner_id),
            Some(ProcessFamilyExit::Child(_)),
        ));
        owner.retire_failed_thread();
        assert_eq!(
            owner.signal_registry.process_family_exit(owner_id),
            Some(ProcessFamilyExit::Failed),
            "a peer failure must override an optimistic exit_group family success",
        );
        assert!(
            parent
                .signal_registry
                .family
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .direct_children
                .get(&process_key(identity(&parent)))
                .is_none_or(|children| !children.contains_key(&process_key(owner_id))),
            "the failed process cannot remain as a waitable successful child",
        );

        let mut root = executor();
        let mut root_peer = root.thread_child(4).unwrap();
        let root_id = identity(&root);
        root_peer.retire_current_thread(reverie::ExitStatus::Exited(7), true);
        assert_eq!(
            root.signal_registry.process_family_exit(root_id),
            Some(ProcessFamilyExit::Root),
        );
        root.retire_failed_thread();
        assert_eq!(
            root.signal_registry.process_family_exit(root_id),
            Some(ProcessFamilyExit::Failed),
            "a peer failure must override an optimistic root exit_group success",
        );

        let mut root = executor();
        let mut child = root.fork_child(2, false, false).unwrap();
        let mut child_peer = child.thread_child(4).unwrap();
        let child_id = identity(&child);
        root.retire_current_thread(reverie::ExitStatus::Exited(1), false);
        child_peer.retire_current_thread(reverie::ExitStatus::Exited(7), true);
        assert_eq!(
            child.signal_registry.process_family_exit(child_id),
            Some(ProcessFamilyExit::RunTeardownChild {
                status: reverie::ExitStatus::Exited(7),
            }),
        );
        child.retire_failed_thread();
        assert_eq!(
            child.signal_registry.process_family_exit(child_id),
            Some(ProcessFamilyExit::Failed),
            "a peer failure must override an optimistic run-teardown exit_group success",
        );

        let parent = executor();
        let parent_id = identity(&parent);
        let mut owner = parent.fork_child(2, false, false).unwrap();
        let mut peer = owner.thread_child(4).unwrap();
        let mut descendant = owner.fork_child(3, false, false).unwrap();
        let owner_id = identity(&owner);
        let descendant_id = identity(&descendant);
        descendant.retire_current_thread(reverie::ExitStatus::Exited(9), false);
        peer.retire_current_thread(reverie::ExitStatus::Exited(7), true);
        let refusal = Some(ProcessFamilyExit::ZombieAdoptionUnsupported {
            child: descendant_id,
            reaper: parent_id,
        });
        assert_eq!(owner.signal_registry.process_family_exit(owner_id), refusal);
        owner.retire_failed_thread();
        assert_eq!(
            owner.signal_registry.process_family_exit(owner_id),
            refusal,
            "a generic peer failure must not erase an earlier typed fatal invariant",
        );
    }

    #[test]
    fn child_siginfo_encoder_and_coalescing_retain_first_nonzero_metadata() {
        let mut parent = executor();
        let parent_id = identity(&parent);
        let first = reverie::ChildExitCompletion {
            parent: parent_id,
            child: SignalProcessId {
                tgid: reverie::Pid::from_raw(2),
                generation: 41,
            },
            status: reverie::ExitStatus::Exited(7),
            waitable: true,
            uid: 1001,
            user_ticks: 17,
            system_ticks: 23,
        };
        let second = reverie::ChildExitCompletion {
            child: SignalProcessId {
                tgid: reverie::Pid::from_raw(3),
                generation: 42,
            },
            status: reverie::ExitStatus::Exited(9),
            uid: 1002,
            user_ticks: 29,
            system_ticks: 31,
            ..first
        };
        let first_event = child_exit_signal_event(first).unwrap();
        let second_event = child_exit_signal_event(second).unwrap();
        for (event, completion) in [(first_event, first), (second_event, second)] {
            let info = event.siginfo();
            assert_eq!(
                i32::from_ne_bytes(info[16..20].try_into().unwrap()),
                completion.child.tgid.as_raw()
            );
            assert_eq!(
                u32::from_ne_bytes(info[20..24].try_into().unwrap()),
                completion.uid
            );
            assert_eq!(
                i64::from_ne_bytes(info[32..40].try_into().unwrap()),
                completion.user_ticks
            );
            assert_eq!(
                i64::from_ne_bytes(info[40..48].try_into().unwrap()),
                completion.system_ticks
            );
        }

        let control = parent.signal_registry.control();
        assert_eq!(
            receipt(control.publish(parent_id, first_event, None, true)).change,
            PendingChange::Queued
        );
        assert_eq!(
            receipt(control.publish(parent_id, second_event, None, true)).change,
            PendingChange::Coalesced
        );
        assert_eq!(
            parent
                .take_pending_signal_for_delivery()
                .unwrap()
                .unwrap()
                .event,
            first_event,
            "standard-signal coalescing must retain the first complete siginfo",
        );
    }

    #[test]
    fn child_publication_coalesces_and_retains_first_complete_siginfo() {
        let mut parent = executor();
        let parent_id = identity(&parent);
        let mut first = parent.fork_child(2, false, false).unwrap();
        let mut second = parent.fork_child(3, false, false).unwrap();
        let first_completion = child_completion(
            parent_id,
            identity(&first),
            reverie::ExitStatus::Exited(7),
            true,
        );
        let second_completion = child_completion(
            parent_id,
            identity(&second),
            reverie::ExitStatus::Exited(9),
            true,
        );
        first.retire_current_thread(first_completion.status, false);
        second.retire_current_thread(second_completion.status, false);
        let control = parent.backend_signal_control().process;
        assert_eq!(
            child_receipt(control.publish_child_exit(first_completion)).effect,
            reverie::ChildExitPublicationEffect::Queued
        );
        let second_receipt = child_receipt(control.publish_child_exit(second_completion));
        assert_eq!(
            second_receipt.effect,
            reverie::ChildExitPublicationEffect::Coalesced
        );
        assert_eq!(
            child_receipt(control.publish_child_exit(second_completion)),
            second_receipt,
            "an exact duplicate must retain the original coalesced receipt"
        );
        let info = parent
            .take_pending_signal_for_delivery()
            .unwrap()
            .unwrap()
            .event
            .siginfo();
        assert_eq!(i32::from_ne_bytes(info[16..20].try_into().unwrap()), 2);
        assert_eq!(i32::from_ne_bytes(info[24..28].try_into().unwrap()), 7);
        assert_eq!(u32::from_ne_bytes(info[20..24].try_into().unwrap()), 0);
        assert_eq!(i64::from_ne_bytes(info[32..40].try_into().unwrap()), 0);
        assert_eq!(i64::from_ne_bytes(info[40..48].try_into().unwrap()), 0);
    }

    #[test]
    fn concurrent_exact_child_publications_commit_one_effect() {
        let mut parent = executor();
        let mut child = parent.fork_child(2, false, false).unwrap();
        let completion = child_completion(
            identity(&parent),
            identity(&child),
            reverie::ExitStatus::Exited(7),
            true,
        );
        child.retire_current_thread(completion.status, false);
        let control = parent.backend_signal_control().process;
        let barrier = Arc::new(std::sync::Barrier::new(3));
        let mut workers = Vec::new();
        for _ in 0..2 {
            let control = control.clone();
            let barrier = barrier.clone();
            workers.push(std::thread::spawn(move || {
                barrier.wait();
                control.publish_child_exit(completion)
            }));
        }
        barrier.wait();
        let first = workers.remove(0).join().unwrap();
        let second = workers.remove(0).join().unwrap();
        assert_eq!(first, second);
        assert!(matches!(
            first,
            reverie::ChildExitPublicationResult::Committed(_)
        ));
        assert!(parent.take_pending_signal_for_delivery().unwrap().is_some());
        assert!(parent.take_pending_signal_for_delivery().unwrap().is_none());
    }

    #[test]
    fn generic_recipients_are_sorted_live_and_mask_aware() {
        let parent = executor();
        let high = parent.thread_child(9).unwrap();
        let low = parent.thread_child(3).unwrap();
        parent
            .state
            .thread_signals
            .lock()
            .blocked
            .insert(libc::SIGCHLD);
        let mut child = parent.fork_child(2, false, false).unwrap();
        let completion = child_completion(
            identity(&parent),
            identity(&child),
            reverie::ExitStatus::Exited(7),
            true,
        );
        child.retire_current_thread(completion.status, false);
        let control = parent.backend_signal_control().process;
        child_receipt(control.publish_child_exit(completion));
        assert_eq!(
            control
                .signal_recipients(completion.parent, libc::SIGCHLD)
                .unwrap()
                .into_iter()
                .map(|recipient| recipient.task.tid.as_raw())
                .collect::<Vec<_>>(),
            vec![3, 9]
        );
        high.state
            .thread_signals
            .lock()
            .blocked
            .insert(libc::SIGCHLD);
        assert_eq!(
            control
                .signal_recipients(completion.parent, libc::SIGCHLD)
                .unwrap()
                .into_iter()
                .map(|recipient| recipient.task.tid.as_raw())
                .collect::<Vec<_>>(),
            vec![3]
        );
        assert_eq!(
            control.signal_recipients(completion.parent, 0),
            Err(Errno::EINVAL)
        );
        drop(low);
        assert!(
            control
                .signal_recipients(completion.parent, libc::SIGCHLD)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn child_publication_uses_independent_carriers_and_refuses_a_missing_one() {
        let mut parent = executor();
        let mut child = parent.fork_child(2, false, false).unwrap();
        let mut memory = GuestMemory::new(0, 4096).unwrap();
        let fd = signalfd_for(&mut parent, &mut memory, libc::SIGCHLD);
        let alias = call(
            &mut parent,
            &memory,
            libc::SYS_dup,
            [fd as u64, 0, 0, 0, 0, 0],
        ) as i32;
        assert!(alias > fd);
        let completion = child_completion(
            identity(&parent),
            identity(&child),
            reverie::ExitStatus::Exited(7),
            true,
        );
        child.retire_current_thread(completion.status, false);
        let control = parent.backend_signal_control().process;
        let table = parent.file_table.clone();
        let held = table.lock().unwrap();
        assert_eq!(
            child_receipt(control.publish_child_exit(completion)).effect,
            reverie::ChildExitPublicationEffect::Queued,
            "publication must not acquire the ordinary file table"
        );
        drop(held);
        assert!(ready(&parent, fd));

        let mut other_parent = executor();
        let mut other_child = other_parent.fork_child(2, false, false).unwrap();
        let mut memory = GuestMemory::new(0, 4096).unwrap();
        let fd = signalfd_for(&mut other_parent, &mut memory, libc::SIGCHLD);
        let other_completion = child_completion(
            identity(&other_parent),
            identity(&other_child),
            reverie::ExitStatus::Exited(8),
            true,
        );
        other_child.retire_current_thread(other_completion.status, false);
        let carrier = other_parent
            .state
            .process_signals
            .lock()
            .unwrap()
            .signalfd_carriers
            .remove(&fd)
            .unwrap();
        assert_eq!(
            other_parent
                .backend_signal_control()
                .process
                .publish_child_exit(other_completion),
            reverie::ChildExitPublicationResult::RejectedBeforeCommit(Errno::EBADF)
        );
        assert!(
            other_parent
                .state
                .process_signals
                .lock()
                .unwrap()
                .shared_pending
                .is_empty()
        );
        drop(carrier);
    }

    #[test]
    fn child_signalfd_failure_retains_exact_receipt_and_is_not_retryable() {
        let mut parent = executor();
        let parent_id = identity(&parent);
        let global = Arc::new(());
        let run = crate::failure::RunFailure::new(&global);
        parent
            .signal_registry
            .install(reverie::BackendSignalControlMode::ToolControlled, &run);
        let mut child = parent.fork_child(2, false, false).unwrap();
        let mut memory = GuestMemory::new(0, 4096).unwrap();
        let fd = signalfd_for(&mut parent, &mut memory, libc::SIGCHLD);
        let alias = call(
            &mut parent,
            &memory,
            libc::SYS_dup,
            [fd as u64, 0, 0, 0, 0, 0],
        ) as i32;
        assert!(alias > fd);
        let completion = child_completion(
            parent_id,
            identity(&child),
            reverie::ExitStatus::Signaled(reverie::Signal::SIGABRT, true),
            true,
        );
        child.retire_current_thread(completion.status, false);
        parent
            .state
            .process_signals
            .lock()
            .unwrap()
            .signalfd_carriers
            .insert(
                alias,
                crate::signal::SignalFdCarrier::pin_eventfd(
                    &std::fs::File::open("/dev/null").unwrap(),
                )
                .unwrap(),
            );
        let committed_carrier = parent
            .state
            .process_signals
            .lock()
            .unwrap()
            .signalfd_carriers[&fd]
            .clone();
        let control = parent.backend_signal_control().process;
        let reverie::ChildExitPublicationResult::FailedAfterCommit { receipt, errno } =
            control.publish_child_exit(completion)
        else {
            panic!("missing child post-commit failure")
        };
        assert_eq!(errno, Errno::EBADF);
        assert_eq!(receipt.completion, completion);
        assert_eq!(receipt.effect, reverie::ChildExitPublicationEffect::Queued);
        assert!(
            ready(&parent, fd) && ready(&parent, alias),
            "the lower-fd carrier must commit readiness before the higher-fd carrier fails"
        );
        set_signalfd_ready(committed_carrier.file(), false).unwrap();
        assert!(!ready(&parent, fd) && !ready(&parent, alias));
        assert_eq!(
            control.publish_child_exit(completion),
            reverie::ChildExitPublicationResult::FailedAfterCommit { receipt, errno },
            "an exact duplicate returns the retained failure without retrying the effect"
        );
        assert!(
            !ready(&parent, fd) && !ready(&parent, alias),
            "replaying a retained failure must not reapply the already committed lower carrier"
        );
        assert_eq!(
            control.finish_publication_failure(parent_id),
            Err(Errno::EINVAL),
            "the alarm-shaped finisher must not acknowledge a child receipt"
        );
        assert_eq!(
            control.finish_child_exit_publication_failure(reverie::ChildExitPublication {
                pending_generation: receipt.pending_generation + 1,
                ..receipt
            }),
            Err(Errno::EINVAL),
            "failure forwarding is bound to the exact retained receipt"
        );
        control
            .finish_child_exit_publication_failure(receipt)
            .unwrap();
        control
            .finish_child_exit_publication_failure(receipt)
            .expect("exact failure acknowledgement is idempotent");
        let retained = parent
            .take_process_publication_failure()
            .expect("root-owned child publication failure");
        assert!(
            matches!(retained.primary(), crate::Error::ChildExitPublication { receipt: actual, errno: Errno::EBADF } if *actual == receipt)
        );
    }

    #[test]
    fn process_binding_guard_retains_exact_generation_until_ack_scope_ends() {
        let parent = executor();
        let process = identity(&parent);
        let registry = parent.signal_registry.clone();
        let guard = parent.retain_signal_process_binding();
        drop(parent);
        assert!(
            registry.lookup(process).is_some(),
            "the callback guard must retain the exact weak registry binding"
        );
        drop(guard);
        assert!(registry.lookup(process).is_none());
    }
}
