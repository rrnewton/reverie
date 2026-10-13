/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! A description-bound projection of actually registered process generations.
//!
//! The existing family transition decides visibility. This module never
//! chooses an exit status, changes wait ownership or creates a signal event.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::Weak;

use reverie::SignalProcessId;
use reverie::SignalTaskIdentity;

use super::DirectChildState;
use super::FileTableState;
use super::ImageRevision;
use super::LoadedStaticElf;
use super::ProcessBinding;
use super::ProcessFamilyExit;
use super::ProcessSignalRegistry;
use super::process_key;
use crate::elf::GuestFsContext;
use crate::elf::TASK_COMM_LEN;
use crate::elf::TaskLifecycleTable;
use crate::executor::CaptureMetadata;

/// Minted only from successful ProcessBinding registration or image rebind.
/// Directory descriptions retain the run/mount view; the task stamp is used
/// only to authenticate this executor's acquisition and metadata mutations.
#[derive(Clone)]
pub(crate) struct ProcNamespace {
    // An open mount view owns its membership ledger, not the executors or
    // their file tables. SCM transfer must not switch it to the receiver's run.
    registry: Arc<ProcessSignalRegistry>,
    identity: Arc<()>,
    task: SignalTaskIdentity,
    image: ImageRevision,
    pub(crate) mounts: Arc<crate::proc_mounts::ProcMountSnapshot>,
}

impl std::fmt::Debug for ProcNamespace {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_struct("ProcNamespace").field("task", &self.task).finish_non_exhaustive()
    }
}

/// Task endpoints belong to one admitted incarnation. CLONE_FS shares fs;
/// accepted process clones copy it. Exited tasks retain no files/fs endpoint.
#[derive(Clone, Debug)]
pub(crate) struct ProcTaskImage {
    pub(crate) tid: i32,
    pub(crate) generation: u64,
    pub(crate) name: Arc<Mutex<[u8; TASK_COMM_LEN]>>,
    pub(crate) files: Weak<Mutex<FileTableState>>,
    pub(crate) fs: Option<Arc<Mutex<GuestFsContext>>>,
    pub(crate) capture: Option<CaptureMetadata>,
    pub(crate) exited: bool,
}

#[derive(Clone, Debug)]
pub(super) struct ProcOwnerImage {
    identity: SignalProcessId,
    revision: ImageRevision,
    lifecycle: Arc<Mutex<TaskLifecycleTable>>,
    ppid: i32,
    orphan_reaper_pid: Arc<std::sync::atomic::AtomicI32>,
    pgid: i32,
    leader_name: Arc<Mutex<[u8; TASK_COMM_LEN]>>,
    cmdline: Vec<u8>,
    executable_file: Option<Weak<std::fs::File>>,
    tasks: BTreeMap<i32, ProcTaskImage>,
    leader_generation: u64,
    terminal_status: Option<reverie::ExitStatus>,
}

/// A family-lock observation. All endpoint locks are taken only after this
/// snapshot has left that critical section; it owns no host task or guest fd.
#[derive(Clone, Debug)]
pub(crate) struct ProcOwnerSnapshot {
    pub(crate) identity: SignalProcessId,
    pub(crate) lifecycle: Arc<Mutex<TaskLifecycleTable>>,
    pub(crate) ppid: i32,
    pub(crate) orphan_reaper_pid: Arc<std::sync::atomic::AtomicI32>,
    pub(crate) pgid: i32,
    pub(crate) leader_name: Arc<Mutex<[u8; TASK_COMM_LEN]>>,
    pub(crate) cmdline: Vec<u8>,
    pub(crate) executable_file: Option<Weak<std::fs::File>>,
    pub(crate) tasks: BTreeMap<i32, ProcTaskImage>,
    pub(crate) zombie: bool,
    pub(crate) terminal_status: Option<reverie::ExitStatus>,
}

impl ProcOwnerImage {
    fn registered(
        binding: &ProcessBinding,
        state: &LoadedStaticElf,
        files: &Arc<Mutex<FileTableState>>,
    ) -> Self {
        // Registration/rebind callers hold the existing process transaction.
        // Acquire lifecycle/image before family, never under its guard.
        let task = Self::task(state, files);
        let revision = binding.image.lock().unwrap_or_else(|p| p.into_inner()).revision.clone();
        Self {
            identity: binding.identity,
            revision,
            lifecycle: state.task_lifecycle.clone(),
            ppid: state.ppid,
            orphan_reaper_pid: state.orphan_reaper_pid.clone(),
            pgid: state.pgid,
            leader_name: state.thread_group_leader_name.clone(),
            cmdline: state.cmdline.clone(),
            executable_file: state.executable_file.as_ref().map(Arc::downgrade),
            leader_generation: task.generation,
            tasks: BTreeMap::from([(state.tid, task)]),
            terminal_status: None,
        }
    }

    fn task(state: &LoadedStaticElf, files: &Arc<Mutex<FileTableState>>) -> ProcTaskImage {
        let lifecycle = state.task_lifecycle.lock().unwrap_or_else(|p| p.into_inner());
        let task = lifecycle.get(state.tid).expect("registered proc task exists");
        ProcTaskImage {
            tid: state.tid,
            generation: task.generation,
            name: state.thread_name.clone(),
            files: Arc::downgrade(files),
            fs: state.shared_fs.clone(),
            capture: lifecycle.task_capture(state.tid),
            exited: false,
        }
    }

    pub(super) fn freeze_terminal(
        &mut self,
        name: [u8; TASK_COMM_LEN],
        status: reverie::ExitStatus,
    ) {
        self.leader_name = Arc::new(Mutex::new(name));
        self.terminal_status = Some(status);
    }

    fn snapshot(&self, zombie: bool) -> ProcOwnerSnapshot {
        let tasks = if zombie {
            BTreeMap::from([(self.identity.tgid.as_raw(), ProcTaskImage {
                tid: self.identity.tgid.as_raw(),
                generation: self.leader_generation,
                name: self.leader_name.clone(),
                files: Weak::new(),
                fs: None,
                capture: None,
                exited: true,
            })])
        } else { self.tasks.clone() };
        ProcOwnerSnapshot {
            identity: self.identity,
            lifecycle: self.lifecycle.clone(),
            ppid: self.ppid,
            orphan_reaper_pid: self.orphan_reaper_pid.clone(),
            pgid: self.pgid,
            leader_name: self.leader_name.clone(),
            cmdline: if zombie { Vec::new() } else { self.cmdline.clone() },
            executable_file: if zombie { None } else { self.executable_file.clone() },
            tasks,
            zombie,
            terminal_status: self.terminal_status,
        }
    }

    fn visible(&self, family: &super::ProcessFamilyState) -> Option<bool> {
        let key = process_key(self.identity);
        match family.terminal.get(&key) {
            None => self.tasks.values().any(|task| !task.exited).then_some(false),
            Some(ProcessFamilyExit::Child(snapshot)) if snapshot.completion.waitable
                && family.direct_children.get(&process_key(snapshot.completion.parent))
                    .is_some_and(|children| children.get(&key) == Some(&DirectChildState::WaitableZombie)) => Some(true),
            Some(_) => None,
        }
    }
}

impl ProcessSignalRegistry {
    fn namespace(self: &Arc<Self>, image: &ProcOwnerImage, state: &LoadedStaticElf) -> ProcNamespace {
        let task = image.tasks.get(&state.tid).expect("registered proc task exists");
        ProcNamespace {
            registry: self.clone(),
            identity: self.proc_namespace_identity.clone(),
            task: SignalTaskIdentity {
                process: image.identity,
                tid: reverie::Pid::from_raw(state.tid),
                task_generation: task.generation,
            },
            image: image.revision.clone(),
            mounts: state.proc_mounts.clone(),
        }
    }

    pub(in crate::executor) fn bind_proc_namespace(
        self: &Arc<Self>,
        binding: &Arc<ProcessBinding>,
        state: &LoadedStaticElf,
        files: &Arc<Mutex<FileTableState>>,
    ) -> ProcNamespace {
        let image = ProcOwnerImage::registered(binding, state, files);
        let namespace = self.namespace(&image, state);
        let retired = self.family.lock().unwrap_or_else(|p| p.into_inner())
            .proc_images.insert(process_key(binding.identity), image);
        drop(retired); // No endpoint destructor runs under family.
        namespace
    }

    pub(in crate::executor) fn register_proc_thread(
        self: &Arc<Self>,
        binding: &Arc<ProcessBinding>,
        parent: &ProcNamespace,
        state: &LoadedStaticElf,
        files: &Arc<Mutex<FileTableState>>,
    ) -> Option<ProcNamespace> {
        if binding.image.lock().unwrap_or_else(|p| p.into_inner()).revision != parent.image {
            return None;
        }
        let task = ProcOwnerImage::task(state, files);
        let mut family = self.family.lock().unwrap_or_else(|p| p.into_inner());
        let image = family.proc_images.get_mut(&process_key(binding.identity))?;
        if image.revision != parent.image { return None; }
        let retired = image.tasks.insert(state.tid, task);
        let namespace = self.namespace(image, state);
        drop(family);
        drop(retired);
        Some(namespace)
    }

    pub(in crate::executor) fn rebind_proc_image(
        self: &Arc<Self>,
        binding: &Arc<ProcessBinding>,
        state: &LoadedStaticElf,
        files: &Arc<Mutex<FileTableState>>,
    ) -> ProcNamespace {
        self.bind_proc_namespace(binding, state, files)
    }

    pub(super) fn proc_terminal_name(&self, identity: SignalProcessId) -> Option<[u8; TASK_COMM_LEN]> {
        let name = self.family.lock().unwrap_or_else(|p| p.into_inner())
            .proc_images.get(&process_key(identity))?.leader_name.clone();
        let value = *name.lock().unwrap_or_else(|p| p.into_inner());
        Some(value)
    }

    pub(in crate::executor) fn retire_proc_task(&self, identity: SignalTaskIdentity) {
        let mut family = self.family.lock().unwrap_or_else(|p| p.into_inner());
        let Some(image) = family.proc_images.get_mut(&process_key(identity.process)) else { return; };
        let tid = identity.tid.as_raw();
        let Some(task) = image.tasks.get_mut(&tid)
            .filter(|task| task.generation == identity.task_generation) else { return; };
        task.exited = true;
        task.files = Weak::new();
        task.capture = None;
        let retired_fs = task.fs.take();
        // Keep exact image/terminal evidence even after provisional Drop. The
        // existing family decision can still publish a later waitable zombie;
        // Drop itself neither publishes terminal status nor exposes a zombie.
        drop(family);
        drop(retired_fs);
    }
}

impl ProcNamespace {
    pub(crate) fn same_view(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.identity, &other.identity)
    }

    pub(crate) fn caller_identity(&self) -> SignalTaskIdentity { self.task }

    pub(crate) fn caller_in_view(&self, state: &LoadedStaticElf) -> Result<SignalTaskIdentity, i64> {
        let caller = state.proc_namespace.as_ref().ok_or(-(libc::ENOENT as i64))?;
        if !self.same_view(caller) || !caller.owns_current_task(state) {
            return Err(-(libc::ENOENT as i64));
        }
        Ok(caller.caller_identity())
    }

    pub(crate) fn owns_current_task(&self, state: &LoadedStaticElf) -> bool {
        if self.task.process.tgid.as_raw() != state.pid || self.task.tid.as_raw() != state.tid {
            return false;
        }
        let registry = &self.registry;
        let Some(binding) = registry.lookup(self.task.process) else { return false; };
        if binding.image.lock().unwrap_or_else(|p| p.into_inner()).revision != self.image {
            return false;
        }
        let current = state.task_lifecycle.lock().unwrap_or_else(|p| p.into_inner())
            .get(state.tid).is_some_and(|task| {
                task.generation == self.task.task_generation
                    && task.process_generation == self.task.process.generation
            });
        if !current { return false; }
        let family = registry.family.lock().unwrap_or_else(|p| p.into_inner());
        family.proc_images.get(&process_key(self.task.process)).is_some_and(|image| {
            image.revision == self.image
                && !family.terminal.contains_key(&process_key(self.task.process))
                && image.tasks.get(&state.tid).is_some_and(|task| {
                    !task.exited && task.generation == self.task.task_generation
                        && Weak::ptr_eq(&task.files, &state.fdinfo_table)
                })
        })
    }

    pub(crate) fn owners(&self) -> Result<Vec<ProcOwnerSnapshot>, i64> {
        let registry = &self.registry;
        let family = registry.family.lock().unwrap_or_else(|p| p.into_inner());
        Ok(family.proc_images.values().filter_map(|image| {
            image.visible(&family).map(|zombie| image.snapshot(zombie))
        }).collect())
    }

    pub(crate) fn owner(&self, identity: SignalProcessId) -> Result<ProcOwnerSnapshot, i64> {
        let registry = &self.registry;
        let family = registry.family.lock().unwrap_or_else(|p| p.into_inner());
        let image = family.proc_images.get(&process_key(identity)).ok_or(-(libc::ENOENT as i64))?;
        let zombie = image.visible(&family).ok_or(-(libc::ENOENT as i64))?;
        Ok(image.snapshot(zombie))
    }

    pub(crate) fn lookup(&self, tid: i32) -> Result<ProcOwnerSnapshot, i64> {
        let mut matches = self.owners()?.into_iter()
            .filter(|owner| owner.identity.tgid.as_raw() == tid || owner.tasks.get(&tid).is_some_and(|task| !task.exited));
        let owner = matches.next().ok_or(-(libc::ENOENT as i64))?;
        if matches.next().is_some() { return Err(-(libc::ENOSYS as i64)); }
        Ok(owner)
    }
}
