/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! Name lookup for the guest-owned procfs surface. Components are visited in
//! order: neither a missing entry nor a regular file can be erased by `..`.

use super::*;

#[derive(Clone, Debug)]
pub(super) struct ProcTaskTarget {
    pub(super) tid: i32,
    tgid: i32,
    process_generation: u64,
    generation: u64,
    process_directory: bool,
    process_view: ProcessProcView,
    table: std::sync::Weak<Mutex<FileTableState>>,
    lifecycle: Arc<Mutex<crate::elf::TaskLifecycleTable>>,
    capture: Option<CaptureMetadata>,
    namespace: Option<ProcNamespace>,
}

impl ProcTaskTarget {
    fn in_namespace(namespace: &ProcNamespace, tid: i32, process: bool) -> Result<Arc<Self>, i64> {
        let owner = namespace.lookup(tid)?;
        Self::owned(namespace, &owner, tid, process)
    }

    fn owned(
        namespace: &ProcNamespace,
        owner: &ProcOwnerSnapshot,
        tid: i32,
        process: bool,
    ) -> Result<Arc<Self>, i64> {
        let tid = if process { owner.identity.tgid.as_raw() } else { tid };
        let task = owner.tasks.get(&tid);
        if !process && task.is_none_or(|task| task.exited && tid != owner.identity.tgid.as_raw()) {
            return Err(negative_errno(libc::ENOENT));
        }
        let view = ProcessProcView::owned(namespace, owner, tid, !process)?;
        Ok(Arc::new(Self {
            tid,
            tgid: owner.identity.tgid.as_raw(),
            process_generation: owner.identity.generation,
            generation: task.map_or(0, |task| task.generation),
            process_directory: process,
            process_view: view,
            table: task.map_or_else(std::sync::Weak::new, |task| task.files.clone()),
            lifecycle: owner.lifecycle.clone(),
            capture: task.and_then(|task| task.capture),
            namespace: Some(namespace.clone()),
        }))
    }

    pub(super) fn process_identity(&self) -> reverie::SignalProcessId {
        reverie::SignalProcessId {
            tgid: reverie::Pid::from_raw(self.tgid),
            generation: self.process_generation,
        }
    }

    fn new(state: &LoadedStaticElf, tid: i32) -> Result<Arc<Self>, i64> {
        let lifecycle = state
            .task_lifecycle
            .lock()
            .expect("KVM lifecycle lock poisoned");
        let task = lifecycle
            .get(tid)
            .filter(|task| task.tgid == state.pid)
            .ok_or_else(|| negative_errno(libc::ENOENT))?;
        Ok(Arc::new(Self {
            tid,
            tgid: task.tgid,
            process_generation: task.process_generation,
            generation: task.generation,
            process_directory: false,
            process_view: ProcessProcView::of(state),
            table: if tid == state.tid {
                state.fdinfo_table.clone()
            } else {
                lifecycle
                    .task_files(tid)
                    .ok_or_else(|| negative_errno(libc::ENOENT))?
            },
            lifecycle: state.task_lifecycle.clone(),
            capture: lifecycle.task_capture(tid),
            namespace: None,
        }))
    }

    fn new_process(state: &LoadedStaticElf) -> Result<Arc<Self>, i64> {
        Self::new(state, state.tid)?.process()
    }

    fn process(&self) -> Result<Arc<Self>, i64> {
        if let Some(namespace) = &self.namespace {
            return Self::owned(namespace, &namespace.owner(self.process_identity())?, self.tgid, true);
        }
        let lifecycle = self.lifecycle.lock().expect("KVM lifecycle lock poisoned");
        if !lifecycle.contains_process(self.tgid, self.process_generation) {
            return Err(negative_errno(libc::ENOENT));
        }
        // A process directory survives SYS_exit in its leader while a member
        // remains. Its fd/fdinfo children still select that exact leader, and
        // must not rebind to a reused numeric TID.
        let leader = lifecycle
            .get(self.tgid)
            .filter(|task| task.process_generation == self.process_generation);
        Ok(Arc::new(Self {
            tid: self.tgid,
            generation: self.process_generation,
            process_directory: true,
            table: if leader.is_some() {
                lifecycle.task_files(self.tgid).unwrap_or_default()
            } else {
                std::sync::Weak::new()
            },
            capture: if leader.is_some() {
                lifecycle.task_capture(self.tgid)
            } else {
                None
            },
            ..self.clone()
        }))
    }

    fn select(&self, tid: i32) -> Result<Arc<Self>, i64> {
        if let Some(namespace) = &self.namespace {
            return Self::owned(namespace, &namespace.owner(self.process_identity())?, tid, false);
        }
        let lifecycle = self.lifecycle.lock().expect("KVM lifecycle lock poisoned");
        let task = lifecycle
            .get(tid)
            .filter(|task| {
                task.tgid == self.tgid && task.process_generation == self.process_generation
            })
            .ok_or_else(|| negative_errno(libc::ENOENT))?;
        Ok(Arc::new(Self {
            tid,
            tgid: self.tgid,
            process_generation: self.process_generation,
            generation: task.generation,
            process_directory: false,
            process_view: self.process_view.clone(),
            table: lifecycle
                .task_files(tid)
                .ok_or_else(|| negative_errno(libc::ENOENT))?,
            lifecycle: self.lifecycle.clone(),
            capture: lifecycle.task_capture(tid),
            namespace: None,
        }))
    }

    pub(super) fn live(&self) -> Result<(), i64> {
        if let Some(namespace) = &self.namespace {
            let owner = namespace.owner(self.process_identity())?;
            return if self.process_directory || owner.tasks.get(&self.tid)
                .is_some_and(|task| task.generation == self.generation
                    && (!task.exited || self.tid == self.tgid)) {
                Ok(())
            } else { Err(negative_errno(libc::ENOENT)) };
        }
        if self.process_directory {
            return if self
                .lifecycle
                .lock()
                .expect("KVM lifecycle lock poisoned")
                .contains_process(self.tgid, self.process_generation)
            {
                Ok(())
            } else {
                Err(negative_errno(libc::ENOENT))
            };
        }
        self.live_task()
    }

    pub(super) fn live_task(&self) -> Result<(), i64> {
        if let Some(namespace) = &self.namespace {
            let owner = namespace.owner(self.process_identity())?;
            return if owner.tasks.get(&self.tid).is_some_and(|task| {
                !task.exited && (self.process_directory || task.generation == self.generation)
            }) { Ok(()) } else { Err(negative_errno(libc::ENOENT)) };
        }
        if self
            .lifecycle
            .lock()
            .expect("KVM lifecycle lock poisoned")
            .get(self.tid)
            .is_some_and(|task| {
                task.generation == self.generation
                    && task.process_generation == self.process_generation
            })
        {
            Ok(())
        } else {
            Err(negative_errno(libc::ENOENT))
        }
    }

    fn owned_task(&self) -> Result<Option<ProcTaskImage>, i64> {
        let Some(namespace) = &self.namespace else { return Ok(None); };
        let owner = namespace.owner(self.process_identity())?;
        let task = owner.tasks.get(&self.tid).filter(|task| {
            !task.exited && (self.process_directory || task.generation == self.generation)
        }).ok_or_else(|| negative_errno(libc::ENOENT))?;
        Ok(Some(task.clone()))
    }

    fn table_endpoint(&self) -> Result<std::sync::Weak<Mutex<FileTableState>>, i64> {
        match self.owned_task()? {
            Some(task) => Ok(task.files),
            None => { self.live_task()?; Ok(self.table.clone()) }
        }
    }

    pub(super) fn is_current_table(&self, state: &LoadedStaticElf) -> bool {
        self.table_endpoint().is_ok_and(|table| std::sync::Weak::ptr_eq(&table, &state.fdinfo_table))
    }

    fn with_table<T>(&self, f: impl FnOnce(&FileTableState) -> Result<T, i64>) -> Result<T, i64> {
        let table = self.table_endpoint()?.upgrade().ok_or_else(|| negative_errno(libc::ENOENT))?;
        let table = table.lock().expect("KVM file-table lock poisoned");
        self.live_task()?;
        f(&table)
    }

    pub(super) fn fd_numbers(&self) -> Result<(Vec<i32>, u64), i64> {
        self.live()?;
        let endpoint = match self.table_endpoint() {
            Ok(endpoint) => endpoint,
            Err(error) if error == negative_errno(libc::ENOENT) => return Ok((Vec::new(), 0)),
            Err(error) => return Err(error),
        };
        let Some(table) = endpoint.upgrade() else { return Ok((Vec::new(), 0)); };
        let table = table.lock().expect("KVM file-table lock poisoned");
        let mut fds: std::collections::BTreeSet<i32> = table.files.keys().copied().collect();
        for fd in 0..=2 {
            if table_host_fd(&table, fd).is_some() { fds.insert(fd); }
        }
        Ok((fds.into_iter().collect(), table.proc_capacity() as u64))
    }

    pub(super) fn owned_content(&self, name: &[u8]) -> Result<Option<Vec<u8>>, i64> {
        let Some(namespace) = &self.namespace else { return Ok(None); };
        let owner = namespace.owner(self.process_identity())?;
        match name {
            b"cmdline" => Ok(Some(if owner.tasks.get(&self.tid).is_some_and(|task|
                !task.exited && (self.process_directory || task.generation == self.generation)) {
                    owner.cmdline
                } else { Vec::new() })),
            b"mounts" => Ok(Some(namespace.mounts.mounts.clone())),
            b"mountinfo" => Ok(Some(namespace.mounts.mountinfo.clone())),
            _ => Ok(None),
        }
    }

    pub(super) fn owned_link(&self, name: &[u8]) -> Result<Option<Vec<u8>>, i64> {
        if !matches!(name, b"cwd" | b"root" | b"exe") { return Ok(None); }
        let Some(namespace) = &self.namespace else { return Ok(None); };
        let owner = namespace.owner(self.process_identity())?;
        let Some(task) = self.owned_task()? else { unreachable!("owned proc task has endpoints"); };
        match name {
            b"cwd" => {
                let fs = task.fs.ok_or_else(|| negative_errno(libc::ENOENT))?;
                let directory = fs.lock().unwrap_or_else(|p| p.into_inner()).cwd_fd.clone();
                Ok(Some(canonical_fd_path(directory.as_raw_fd())?.into_os_string().into_vec()))
            }
            b"root" => Ok(Some(b"/".to_vec())),
            b"exe" => {
                let file = owner.executable_file.and_then(|file| file.upgrade())
                    .ok_or_else(|| negative_errno(libc::ENOENT))?;
                Ok(Some(canonical_fd_path(file.as_raw_fd())?.into_os_string().into_vec()))
            }
            _ => Ok(None),
        }
    }

    pub(super) fn is_owned(&self) -> bool { self.namespace.is_some() }

    pub(super) fn namespace(&self) -> Option<&ProcNamespace> { self.namespace.as_ref() }

    pub(super) fn owner_path(&self, name: &[u8]) -> Vec<u8> {
        let mut path = if self.process_directory { format!("/proc/{}/", self.tgid).into_bytes() }
            else { format!("/proc/{}/task/{}/", self.tgid, self.tid).into_bytes() };
        path.extend_from_slice(name);
        path
    }

    pub(super) fn inode_for_path(&self, path: &[u8]) -> u64 {
        if self.namespace.is_none() { return synthetic_proc_inode(path); }
        let components: Vec<_> = path.split(|b| *b == b'/').filter(|c| !c.is_empty()).collect();
        if components.len() < 2 { return synthetic_proc_inode(path); }
        let (mut identity, suffix, generation) = match components.as_slice() {
            [_, _, b"task", tid, suffix @ ..] if parse_proc_id(tid) == Some(self.tid) => (
                format!("/proc/{}/task/{}", self.tgid, self.tid).into_bytes(), suffix, self.generation),
            [_, name, suffix @ ..] if *name == b"thread-self"
                || (self.tid != self.tgid && parse_proc_id(name) == Some(self.tid)) => (
                format!("/proc/{}/task/{}", self.tgid, self.tid).into_bytes(), suffix, self.generation),
            [_, _, suffix @ ..] => (format!("/proc/{}", self.tgid).into_bytes(), suffix, 0),
            _ => return synthetic_proc_inode(path),
        };
        for component in suffix { identity.push(b'/'); identity.extend_from_slice(component); }
        owned_proc_inode(&identity, self.process_generation, generation)
    }

    pub(super) fn inode_for_name(&self, name: &[u8]) -> u64 { self.inode_for_path(&self.owner_path(name)) }

    /// Resolve a followed magic link from the selected incarnation's held
    /// endpoint, never by interpreting a foreign path in the caller's state.
    pub(super) fn owned_link_file(
        &self,
        state: &LoadedStaticElf,
        name: &[u8],
    ) -> Result<crate::elf::StagedFile, i64> {
        let namespace = self.namespace.as_ref().ok_or_else(|| negative_errno(libc::ENOSYS))?;
        let owner = namespace.owner(self.process_identity())?;
        let task = self.owned_task()?.ok_or_else(|| negative_errno(libc::ENOENT))?;
        let held = match name {
            b"cwd" => {
                let fs = task.fs.ok_or_else(|| negative_errno(libc::ENOENT))?;
                let held = fs.lock().unwrap_or_else(|p| p.into_inner()).cwd_fd.clone();
                held
            }
            b"exe" => owner.executable_file.and_then(|file| file.upgrade())
                .ok_or_else(|| negative_errno(libc::ENOENT))?,
            b"root" => Arc::new(std::fs::File::open("/").map_err(io_error)?),
            _ => return Err(negative_errno(libc::ENOENT)),
        };
        state.file_retirement.stage_clone(&held).map_err(io_error)
    }

    pub(super) fn open_source(&self, state: &LoadedStaticElf, fd: i32) -> Result<GuestFdOpenSource, i64> {
        if self.is_current_table(state) { return GuestFdOpenSource::of(state, fd); }
        let capture = self.owned_task()?.and_then(|task| task.capture).or(self.capture);
        self.with_table(|table| {
            let host = table_host_fd(table, fd).ok_or_else(|| negative_errno(libc::ENOENT))?;
            let standard = (0..=2).contains(&fd) && !table.files.contains_key(&fd)
                && !table.closed_standard_fds.contains(&fd) && (fd != 0 || table.stdin.is_some());
            let alias = output_alias_from_sets(fd, &table.stdout_alias_fds, &table.stderr_alias_fds, standard);
            if capture.is_some() && alias.is_some() {
                // Captured writes belong to their actual output endpoint, not
                // to the receiving executor's stdout/stderr. Metadata/link
                // observations below remain possible; reopening is unmodeled.
                return Err(negative_errno(libc::ENOSYS));
            }
            Ok(GuestFdOpenSource {
                file: state.file_retirement.stage(pin_host_fd(host)?),
                description: table.fdinfo_files.get(&fd).cloned(),
                proc_inode: table.proc_files.get(&fd).copied(),
                is_random: table.random_device_fds.contains(&fd),
                is_signalfd: table.signalfd_fds.contains(&fd),
                alias,
                object_inode: table.fd_object_inodes.get(&fd).cloned().unwrap_or_else(|| Arc::new(GuestFileIdentity {
                    inode: synthetic_guest_fd_inode(fd, false), timestamps: IdentityTimestamps::Host,
                })),
            })
        })
    }

    fn metadata_source(&self, state: &LoadedStaticElf, fd: i32) -> Result<OwnedFdMetadata, i64> {
        let capture = self.owned_task()?.and_then(|task| task.capture).or(self.capture);
        self.with_table(|table| {
            let host = table_host_fd(table, fd).ok_or_else(|| negative_errno(libc::ENOENT))?;
            let standard = (0..=2).contains(&fd) && !table.files.contains_key(&fd)
                && !table.closed_standard_fds.contains(&fd) && (fd != 0 || table.stdin.is_some());
            let alias = output_alias_from_sets(fd, &table.stdout_alias_fds, &table.stderr_alias_fds, standard);
            Ok(OwnedFdMetadata {
                file: state.file_retirement.stage(pin_host_fd(host)?),
                inode: table.proc_files.get(&fd).copied(),
                description: table.fdinfo_files.get(&fd).cloned(),
                random_minor: table.random_device_descriptions.get(&fd).map(|random| random.minor),
                object: table.fd_object_inodes.get(&fd).cloned(),
                signalfd: table.signalfd_fds.contains(&fd),
                capture: capture.zip(alias).map(|(capture, alias)| capture.identity(alias)),
            })
        })
    }

    fn fd_entry(&self, state: &LoadedStaticElf, fd: i32) -> Result<ProcFdEntry, i64> {
        self.live_task()?;
        let pin = |host: RawFd, description: Option<&Arc<FdinfoDescription>>| {
            if let Some(description) = description
                && let SeqProcSource::Directory(directory) = &description.source
            {
                return Ok(ProcFdEntry::Directory(directory.clone()));
            }
            Ok(ProcFdEntry::File(pin_host_fd(host)?))
        };
        if self.is_current_table(state) {
            pin(
                host_fd(state, fd).ok_or_else(|| negative_errno(libc::ENOENT))?,
                state.fdinfo_files.get(&fd),
            )
        } else {
            self.with_table(|table| {
                pin(
                    table_host_fd(table, fd).ok_or_else(|| negative_errno(libc::ENOENT))?,
                    table.fdinfo_files.get(&fd),
                )
            })
        }
    }

    pub(super) fn link(
        &self,
        state: &LoadedStaticElf,
        fd: i32,
        capture: Option<CaptureMetadata>,
    ) -> Result<Vec<u8>, i64> {
        self.live_task()?;
        if self.is_current_table(state) {
            return guest_fd_link_target(state, fd, capture);
        }
        self.with_table(|table| {
            let standard = (0..=2).contains(&fd)
                && !table.closed_standard_fds.contains(&fd)
                && !table.files.contains_key(&fd)
                && (fd != 0 || table.stdin.is_some());
            if let (Some(capture), Some(alias)) = (
                self.capture,
                output_alias_from_sets(
                    fd,
                    &table.stdout_alias_fds,
                    &table.stderr_alias_fds,
                    standard,
                ),
            ) {
                return Ok(format!("pipe:[{}]", capture.identity(alias).inode).into_bytes());
            }
            let host = table_host_fd(table, fd).ok_or_else(|| negative_errno(libc::ENOENT))?;
            if let Some(description) = table.fdinfo_files.get(&fd) {
                return Ok(description.path.clone());
            }
            if table.signalfd_fds.contains(&fd) {
                return Ok(b"anon_inode:[signalfd]".to_vec());
            }
            if let Some(inode) = table.proc_files.get(&fd)
                && let Some(path) = synthetic_proc_path_for_inode(*inode)
            {
                return Ok(path.to_vec());
            }
            let target = canonical_fd_path(host)?.into_os_string().into_vec();
            for kind in ["pipe", "socket"] {
                if target.starts_with(format!("{kind}:[").as_bytes())
                    && target.ends_with(b"]")
                    && let Some(identity) = table.fd_object_inodes.get(&fd)
                {
                    return Ok(format!("{kind}:[{}]", identity.inode).into_bytes());
                }
            }
            Ok(target)
        })
    }

    pub(super) fn view(
        &self,
        state: &LoadedStaticElf,
        file: ProcessProcFile,
    ) -> Result<ProcessProcView, i64> {
        self.live()?;
        if let Some(namespace) = &self.namespace {
            return ProcessProcView::owned(namespace, &namespace.owner(self.process_identity())?,
                self.tid, !self.process_directory);
        }
        let lifecycle = self.lifecycle.lock().expect("KVM lifecycle lock poisoned");
        // The existing process view samples the guest umask at file open.
        // An anchor retains identity and live name/parent cells, not that
        // sample. A foreign process's filesystem context is not represented,
        // so refuse new status opens rather than misreport its umask. Stat
        // does not render umask and uses the retained owner's cells.
        let same_process = Arc::ptr_eq(&self.lifecycle, &state.task_lifecycle)
            && lifecycle.get(state.tid).is_some_and(|task| {
                task.tgid == self.tgid && task.process_generation == self.process_generation
            });
        if file == ProcessProcFile::Status && !same_process {
            return Err(negative_errno(libc::ENOSYS));
        }
        let mut view = self.process_view.clone();
        if same_process {
            view.umask = Some(state.umask);
        }
        if self.process_directory {
            return Ok(view);
        }
        if !lifecycle.get(self.tid).is_some_and(|task| {
            task.generation == self.generation && task.process_generation == self.process_generation
        }) {
            return Err(negative_errno(libc::ENOENT));
        }
        let name = lifecycle
            .task_name(self.tid)
            .ok_or_else(|| negative_errno(libc::ENOENT))?;
        Ok(ProcessProcView {
            tid: self.tid,
            leader_name: name,
            task: Some((self.generation, self.lifecycle.clone())),
            ..view
        })
    }

    pub(super) fn identity_path(&self, file: ProcessProcFile) -> Vec<u8> {
        if self.namespace.is_some() { return self.file_path(file); }
        if self.process_directory {
            format!("/proc/self/{}", file.name()).into_bytes()
        } else {
            format!("/proc/{}/task/{}/{}", self.tgid, self.tid, file.name()).into_bytes()
        }
    }

    pub(super) fn file_path(&self, file: ProcessProcFile) -> Vec<u8> {
        if self.process_directory {
            format!("/proc/{}/{}", self.tgid, file.name()).into_bytes()
        } else {
            format!("/proc/{}/task/{}/{}", self.tgid, self.tid, file.name()).into_bytes()
        }
    }

    fn fd_exists(&self, state: &LoadedStaticElf, fd: i32) -> Result<(), i64> {
        self.live_task()?;
        if self.is_current_table(state) {
            host_fd(state, fd).ok_or_else(|| negative_errno(libc::ENOENT))?;
            Ok(())
        } else {
            self.with_table(|table| {
                table_host_fd(table, fd).ok_or_else(|| negative_errno(libc::ENOENT))?;
                Ok(())
            })
        }
    }

    pub(super) fn fdinfo(
        &self,
        state: &LoadedStaticElf,
        fd: i32,
        flags: u64,
        capture_output: bool,
    ) -> Result<FdinfoTarget, i64> {
        self.live_task()?;
        let endpoint = self.table_endpoint()?;
        let owned = self.owned_task()?;
        if self.is_current_table(state) {
            host_fd(state, fd).ok_or_else(|| negative_errno(libc::ENOENT))?;
            validate_fdinfo_open_flags(state, flags)?;
            ensure_fdinfo_content_supported(state, fd, capture_output)?;
        } else {
            self.with_table(|table| {
                let host = table_host_fd(table, fd).ok_or_else(|| negative_errno(libc::ENOENT))?;
                validate_fdinfo_open_flags(state, flags)?;
                let directory = table.fdinfo_files.get(&fd).is_some_and(|description|
                    matches!(&description.source, SeqProcSource::Directory(directory) if directory.namespace.is_some()));
                if (table.proc_files.contains_key(&fd) && !directory)
                    || table.random_device_fds.contains(&fd)
                    || table.signalfd_fds.contains(&fd)
                    || (self.capture.is_some()
                        && (table.stdout_alias_fds.contains(&fd)
                            || table.stderr_alias_fds.contains(&fd)
                            || ((fd == 1 || fd == 2)
                                && !table.files.contains_key(&fd)
                                && !table.closed_standard_fds.contains(&fd))))
                {
                    return Err(negative_errno(libc::ENOSYS));
                }
                if directory { Ok(()) } else { ensure_fdinfo_object(host) }
            })?;
        }
        if endpoint.upgrade().is_none() {
            return Err(negative_errno(libc::ENOSYS));
        }
        Ok(FdinfoTarget {
            target_tid: self.tid,
            target_generation: owned.as_ref().map_or(self.generation, |task| task.generation),
            target_fd: fd,
            table: endpoint,
            lifecycle: self.lifecycle.clone(),
            capture_output: if self.is_current_table(state) {
                capture_output
            } else {
                self.capture.is_some()
            },
        })
    }
}

enum ProcFdEntry {
    Directory(ProcDirectory),
    File(std::fs::File),
}

fn table_host_fd(table: &FileTableState, fd: i32) -> Option<RawFd> {
    table.files.get(&fd).map(AsRawFd::as_raw_fd).or_else(|| {
        if table.closed_standard_fds.contains(&fd) {
            return None;
        }
        match fd {
            0 => table.stdin.as_ref().map(AsRawFd::as_raw_fd),
            1 | 2 => Some(fd),
            _ => None,
        }
    })
}

#[derive(Clone, Debug)]
pub(super) struct ProcDirectory {
    pub(super) components: Vec<Vec<u8>>,
    pub(super) target: Option<Arc<ProcTaskTarget>>,
    pub(super) namespace: Option<ProcNamespace>,
    pub(super) cookie: Arc<Mutex<proc_directory::DirectoryCursor>>,
    pub(super) status_flags: i32,
}

impl ProcDirectory {
    pub(super) fn path(&self) -> Vec<u8> {
        absolute(&self.components)
    }

    pub(super) fn inode(&self) -> u64 {
        let path = self.path();
        self.target.as_ref().map_or_else(|| synthetic_proc_inode(&path), |target| target.inode_for_path(&path))
    }
}

pub(super) fn owned_proc_inode(path: &[u8], process_generation: u64, task_generation: u64) -> u64 {
    let mut identity = path.to_vec();
    identity.push(0);
    identity.extend_from_slice(&process_generation.to_le_bytes());
    identity.extend_from_slice(&task_generation.to_le_bytes());
    synthetic_proc_inode(&identity)
}

pub(super) enum ProcPath {
    File {
        path: Vec<u8>,
        target: Option<Arc<ProcTaskTarget>>,
        namespace: Option<ProcNamespace>,
    },
    Directory(ProcDirectory),
    TaskLink {
        directory: ProcDirectory,
        link: Vec<u8>,
    },
    Fd {
        target: Arc<ProcTaskTarget>,
        fd: i32,
    },
    Owned {
        file: crate::elf::StagedFile,
        path: Vec<u8>,
    },
}

fn absolute(components: &[Vec<u8>]) -> Vec<u8> {
    let mut path = Vec::new();
    for component in components {
        path.push(b'/');
        path.extend_from_slice(component);
    }
    if path.is_empty() {
        path.push(b'/');
    }
    path
}

fn canonical_file(state: &LoadedStaticElf, components: &[Vec<u8>]) -> Vec<u8> {
    let path = absolute(components);
    let thread = format!("/proc/{}/task/{}/", state.pid, state.tid);
    if let Some(suffix) = path.strip_prefix(thread.as_bytes()) {
        return [b"/proc/thread-self/".as_slice(), suffix].concat();
    }
    // An explicitly selected peer task must stay recognizable to the file
    // dispatcher regardless of the reader's own identity.
    if components
        .get(2)
        .is_some_and(|component| component == b"task")
    {
        return path;
    }
    let process = format!("/proc/{}/", state.pid);
    if let Some(suffix) = path.strip_prefix(process.as_bytes()) {
        return [b"/proc/self/".as_slice(), suffix].concat();
    }
    path
}

pub(super) fn resolve(
    state: &LoadedStaticElf,
    dirfd: i32,
    path: &[u8],
) -> Result<Option<ProcPath>, i64> {
    // Empty-path operations select the descriptor itself and retain their
    // syscall-specific rules; there is no pathname component to resolve.
    if path.is_empty() {
        return Ok(None);
    }
    let mut components;
    let mut target;
    let mut namespace;
    let mut raw_proc_path = path.starts_with(b"/");
    let mut pending: std::collections::VecDeque<Vec<u8>> = path
        .split(|b| *b == b'/')
        .filter(|c| !c.is_empty())
        .map(<[u8]>::to_vec)
        .collect();
    if path.starts_with(b"/") {
        if pending.pop_front().as_deref() != Some(b"proc") {
            return Ok(None);
        }
        components = vec![b"proc".to_vec()];
        target = None;
        namespace = state.proc_namespace.clone();
    } else if let Some(description) = state.fdinfo_files.get(&dirfd)
        && let SeqProcSource::Directory(directory) = &description.source
    {
        components = directory.components.clone();
        target = directory.target.clone();
        namespace = directory.namespace.clone();
    } else if state.proc_files.get(&dirfd) == Some(&synthetic_proc_inode(b"/proc")) {
        components = vec![b"proc".to_vec()];
        target = None;
        namespace = None;
    } else {
        return Ok(None);
    }
    if let Some(target) = &target {
        target.live()?;
    }
    while let Some(component) = pending.pop_front() {
        if component == b"." {
            continue;
        }
        if component == b".." {
            components.pop();
            if components.is_empty() {
                return Ok(Some(ProcPath::File {
                    path: absolute(&pending.into_iter().collect::<Vec<_>>()),
                    target: None,
                    namespace: None,
                }));
            }
            if components.len() == 1 {
                target = None;
            } else if let Some(previous) = &target {
                target = if components.len() >= 4 && components[2] == b"task" {
                    let tid = parse_proc_id(&components[3])
                        .ok_or_else(|| negative_errno(libc::ENOENT))?;
                    Some(previous.select(tid)?)
                } else {
                    Some(previous.process()?)
                };
            }
            continue;
        }
        let parent = canonical_file(state, &components);
        if components.len() == 1 && (component == b"self" || component == b"thread-self") {
            raw_proc_path = false;
            let (pid, tid) = if let Some(view) = &namespace {
                let caller = view.caller_in_view(state)?;
                (caller.process.tgid.as_raw(), caller.tid.as_raw())
            } else { (state.pid, state.tid) };
            components.push(pid.to_string().into_bytes());
            if component == b"thread-self" {
                components.push(b"task".to_vec());
                components.push(tid.to_string().into_bytes());
            }
            target = Some(if let Some(view) = &namespace {
                ProcTaskTarget::in_namespace(view, if component == b"thread-self" { tid } else { pid },
                    component == b"self")?
            } else if component == b"thread-self" {
                ProcTaskTarget::new(state, tid)?
            } else { ProcTaskTarget::new_process(state)? });
            if pending.is_empty() && !path.ends_with(b"/") {
                let link = if component == b"thread-self" {
                    format!("{pid}/task/{tid}").into_bytes()
                } else {
                    pid.to_string().into_bytes()
                };
                return Ok(Some(ProcPath::TaskLink {
                    directory: ProcDirectory {
                        components,
                        target,
                        namespace: namespace.clone(),
                        cookie: Arc::default(),
                        status_flags: 0,
                    },
                    link,
                }));
            }
            continue;
        }
        if components.len() == 1
            && let Some(tid) = parse_proc_id(&component)
        {
            // Preserve the host-procfs isolation guard for a non-owned raw
            // absolute name. After guest lookup, the original pathname can
            // name supervisor tasks/fds, so it must never become a fallback.
            if let Some(view) = &namespace {
                let owner = view.lookup(tid)?;
                target = Some(ProcTaskTarget::owned(view, &owner, tid,
                    tid == owner.identity.tgid.as_raw())?);
            } else {
                if raw_proc_path && tid != state.pid && !live_thread_in_process(state, tid) {
                    return Ok(None);
                }
                target = Some(if tid == state.pid { ProcTaskTarget::new_process(state)? }
                    else { ProcTaskTarget::new(state, tid)? });
            }
            raw_proc_path = false;
            components.push(component);
            continue;
        }
        if components.last().is_some_and(|c| c == b"task") {
            let tid = parse_proc_id(&component).ok_or_else(|| negative_errno(libc::ENOENT))?;
            target = Some(
                target
                    .as_ref()
                    .ok_or_else(|| negative_errno(libc::ENOENT))?
                    .select(tid)?,
            );
            components.push(component);
            continue;
        }
        if components.last().is_some_and(|c| c == b"fd") {
            let selected = target
                .as_ref()
                .ok_or_else(|| negative_errno(libc::ENOENT))?;
            let fd = parse_proc_id(&component).ok_or_else(|| negative_errno(libc::ENOENT))?;
            if pending.is_empty() && !path.ends_with(b"/") {
                return Ok(Some(ProcPath::Fd {
                    target: selected.clone(),
                    fd,
                }));
            }
            let file = match selected.fd_entry(state, fd)? {
                ProcFdEntry::Directory(directory) => {
                    components = directory.components;
                    target = directory.target;
                    namespace = directory.namespace;
                    continue;
                }
                ProcFdEntry::File(file) => file,
            };
            if fd_mode(file.as_raw_fd())? & libc::S_IFMT != libc::S_IFDIR {
                return Err(negative_errno(libc::ENOTDIR));
            }
            let suffix = if pending.is_empty() {
                b".".to_vec()
            } else {
                let mut suffix = absolute(&pending.into_iter().collect::<Vec<_>>());
                suffix.remove(0);
                if path.ends_with(b"/") {
                    suffix.push(b'/');
                }
                suffix
            };
            return Ok(Some(ProcPath::Owned {
                file: state.file_retirement.stage(file),
                path: suffix,
            }));
        }
        let directory = match parent.as_slice() {
            b"/proc" => component == b"sys",
            b"/proc/sys" => component == b"kernel",
            b"/proc/sys/kernel" => component == b"pty",
            _ => {
                let task_directory = matches!(components.as_slice(), [root, pid]
                    if root == b"proc" && parse_proc_id(pid).is_some())
                    || matches!(components.as_slice(), [root, pid, task, tid]
                        if root == b"proc" && parse_proc_id(pid).is_some()
                            && task == b"task" && parse_proc_id(tid).is_some());
                task_directory
                    && ([b"fd".as_slice(), b"fdinfo"].contains(&component.as_slice())
                        || (components.len() == 2 && component == b"task"))
            }
        };
        components.push(component);
        if directory {
            if components
                .last()
                .is_some_and(|component| component == b"fd" || component == b"fdinfo")
            {
                let selected = target.as_ref().ok_or_else(|| negative_errno(libc::ENOENT))?;
                if selected.namespace.is_some() { selected.live()?; } else { selected.live_task()?; }
            }
            continue;
        }
        let canonical = canonical_file(state, &components);
        let process_file = target.is_some() && process_file_name(&canonical).is_some();
        let fdinfo_file = if let Some(fd) = fdinfo_file_name(&canonical) {
            target
                .as_ref()
                .ok_or_else(|| negative_errno(libc::ENOENT))?
                .fd_exists(state, fd)?;
            true
        } else {
            false
        };
        let owned_leaf = target.as_ref().filter(|target| target.is_owned()).is_some_and(|_| {
            matches!(owner_file_name(&canonical),
                Some(b"cmdline" | b"mounts" | b"mountinfo" | b"cwd" | b"root" | b"exe"))
        });
        let exists = process_file || fdinfo_file || owned_leaf
            || (target.as_ref().is_none_or(|target| !target.is_owned())
                && (synthetic_proc_content(state, &canonical).is_some()
                    || guest_proc_exe_path(state, &canonical).is_some()
                    || matches!(canonical.as_slice(), b"/proc/self/cwd" | b"/proc/self/root")));
        if !exists {
            return Err(negative_errno(libc::ENOENT));
        }
        if !pending.is_empty() || path.ends_with(b"/") {
            return Err(negative_errno(libc::ENOTDIR));
        }
        return Ok(Some(ProcPath::File {
            path: canonical,
            target,
            namespace,
        }));
    }
    Ok(Some(ProcPath::Directory(ProcDirectory {
        components,
        target,
        namespace,
        cookie: Arc::default(),
        status_flags: 0,
    })))
}

pub(super) fn open_directory(
    state: &mut LoadedStaticElf,
    mut directory: ProcDirectory,
    flags: u64,
) -> i64 {
    if let Some(target) = &directory.target
        && let Err(error) = target.live()
    {
        return error;
    }
    if directory.components.len() == 1 && directory.namespace.is_none() {
        return open_synthetic_proc_directory(state, flags, flags & libc::O_CLOEXEC as u64 != 0);
    }
    let path_only = flags & libc::O_PATH as u64 != 0;
    if directory.components.last().is_some_and(|c| c == b"fdinfo") {
        return negative_errno(if !path_only && flags & libc::O_CREAT as u64 != 0 {
            libc::EISDIR
        } else {
            libc::ENOENT
        });
    }
    if !path_only {
        if flags & (libc::O_CREAT | libc::O_EXCL) as u64 == (libc::O_CREAT | libc::O_EXCL) as u64 {
            return negative_errno(libc::EEXIST);
        }
        if flags & libc::O_ACCMODE as u64 != libc::O_RDONLY as u64
            || flags & libc::O_TRUNC as u64 != 0
        {
            return negative_errno(libc::EISDIR);
        }
        if flags & libc::O_CREAT as u64 != 0 {
            return negative_errno(libc::EISDIR);
        }
        if flags & libc::O_TMPFILE as u64 == libc::O_TMPFILE as u64 {
            return negative_errno(libc::EINVAL);
        }
        if flags & libc::O_DIRECT as u64 != 0 {
            return negative_errno(libc::EINVAL);
        }
    }
    // A new open/reopen starts a fresh OFD. Dup/fork/SCM keep the description.
    directory.cookie = Arc::default();
    // The sealed carrier is reopened through a private magic link. These
    // immutable logical open flags must remain visible in F_GETFL/fdinfo even
    // though the native carrier cannot carry O_DIRECTORY or O_NOFOLLOW.
    directory.status_flags = flags as i32 & (libc::O_DIRECTORY | libc::O_NOFOLLOW);
    let path = directory.path();
    let fd = open_synthetic_proc(
        state,
        &path,
        b"",
        flags & libc::O_CLOEXEC as u64 != 0,
        path_only,
        flags as i32,
    );
    if fd >= 0 {
        if directory.namespace.is_some() { state.proc_files.insert(fd as i32, directory.inode()); }
        state.fdinfo_files.insert(
            fd as i32,
            Arc::new(FdinfoDescription {
                source: SeqProcSource::Directory(directory),
                path,
                nofollow_status: false,
                path_only,
                sequence: Mutex::default(),
            }),
        );
    }
    fd
}

pub(super) fn open_link(
    state: &mut LoadedStaticElf,
    path: &[u8],
    link: Vec<u8>,
    flags: u64,
) -> i64 {
    let fd = open_synthetic_proc(
        state,
        path,
        b"",
        flags & libc::O_CLOEXEC as u64 != 0,
        true,
        flags as i32,
    );
    if fd >= 0 {
        state.fdinfo_files.insert(
            fd as i32,
            Arc::new(FdinfoDescription {
                source: SeqProcSource::Link(link),
                path: path.to_vec(),
                nofollow_status: true,
                path_only: true,
                sequence: Mutex::default(),
            }),
        );
    }
    fd
}

/// Only an already selected proc owner may supply this numeric leaf. Raw
/// pathname fallback continues to use the existing current-process parser.
pub(super) fn fdinfo_file_name(path: &[u8]) -> Option<i32> {
    let components: Vec<_> = path.strip_prefix(b"/proc/")?.split(|b| *b == b'/').collect();
    let name = match components.as_slice() {
        [_, b"fdinfo", name] | [_, b"task", _, b"fdinfo", name] => *name,
        _ => return None,
    };
    parse_proc_id(name)
}

pub(super) fn owner_file_name(path: &[u8]) -> Option<&[u8]> {
    let components: Vec<_> = path.strip_prefix(b"/proc/")?.split(|b| *b == b'/').collect();
    match components.as_slice() {
        [_, name] | [_, b"task", _, name] => Some(*name),
        _ => None,
    }
}

pub(super) fn open_owned_link(
    state: &mut LoadedStaticElf,
    target: Arc<ProcTaskTarget>,
    path: Vec<u8>,
    name: Vec<u8>,
    flags: u64,
) -> i64 {
    let fd = open_synthetic_proc(state, &path, b"", flags & libc::O_CLOEXEC as u64 != 0,
        true, flags as i32);
    if fd >= 0 {
        state.proc_files.insert(fd as i32, target.inode_for_name(&name));
        state.fdinfo_files.insert(fd as i32, Arc::new(FdinfoDescription {
            source: SeqProcSource::OwnedLink { target, name }, path,
            nofollow_status: true, path_only: true, sequence: Mutex::default(),
        }));
    }
    fd
}

pub(super) enum ProcMetadata {
    Path(Vec<u8>),
    Stat(libc::stat),
    OwnedStat { stat: libc::stat, mount_id: u64 },
    Fd(OwnedFdMetadata),
}

/// One table observation pins the actual object and its logical metadata.
/// Native metadata calls and endpoint destruction occur after releasing the
/// selected owner's table; a subsequent numeric reuse cannot retarget it.
pub(super) struct OwnedFdMetadata {
    file: crate::elf::StagedFile,
    inode: Option<u64>,
    description: Option<Arc<FdinfoDescription>>,
    random_minor: Option<u32>,
    object: Option<Arc<GuestFileIdentity>>,
    signalfd: bool,
    capture: Option<CaptureObjectIdentity>,
}

impl OwnedFdMetadata {
    fn held(file: crate::elf::StagedFile) -> Self {
        Self { file, inode: None, description: None, random_minor: None,
            object: None, signalfd: false, capture: None }
    }

    fn link_stat(&self, inode: u64) -> Result<libc::stat, i64> {
        let flags = fd_status_flags(self.file.as_file().as_raw_fd())?;
        let mut mode = libc::S_IFLNK;
        if flags & libc::O_PATH == 0 {
            // fs/proc/fd.c: tid_fd_update_inode derives these permissions from
            // the selected file's FMODE_READ/FMODE_WRITE, not the caller's FD.
            match flags & libc::O_ACCMODE {
                libc::O_RDONLY => mode |= 0o500,
                libc::O_WRONLY => mode |= 0o300,
                libc::O_RDWR => mode |= 0o700,
                _ => {}
            }
        }
        let mut stat = synthetic_proc_stat(inode, 64);
        stat.st_mode = mode; stat.st_blocks = 0;
        Ok(stat)
    }

    fn seconds(&self, state: &LoadedStaticElf) -> Option<libc::time_t> {
        if !state.host_metadata_timestamps { return Some(DETERMINISTIC_METADATA_SECONDS); }
        match self.object.as_ref().map(|object| object.timestamps) {
            Some(IdentityTimestamps::VirtualFile) => return Some(DETERMINISTIC_METADATA_SECONDS),
            Some(IdentityTimestamps::ProcSnapshot) => return Some(0),
            _ => {}
        }
        if self.signalfd { return Some(DETERMINISTIC_METADATA_SECONDS); }
        let host = self.file.as_file().as_raw_fd();
        if canonical_fd_path(host).is_ok_and(|path| path.as_os_str().as_bytes() == b"anon_inode:[pidfd]")
            && recorded_self_pidfd_signature().is_some_and(|signature| pidfd_matches_signature(host, signature)) {
            return Some(DETERMINISTIC_METADATA_SECONDS);
        }
        None
    }

    pub(super) fn stat(&self, state: &LoadedStaticElf) -> Result<libc::stat, i64> {
        if let Some(capture) = self.capture { return Ok(synthetic_captured_output_stat(capture)); }
        let mut stat = std::mem::MaybeUninit::<libc::stat>::zeroed();
        // SAFETY: the captured object remains pinned and stat is writable.
        if unsafe { libc::fstat(self.file.as_file().as_raw_fd(), stat.as_mut_ptr()) } != 0 {
            return Err(io_error(std::io::Error::last_os_error()));
        }
        // SAFETY: fstat initialized the complete result.
        let mut stat = unsafe { stat.assume_init() };
        if let Some(inode) = self.inode {
            sanitize_proc_stat(&mut stat, inode);
            if let Some(description) = &self.description {
                match &description.source {
                    SeqProcSource::Directory(_) => { stat.st_mode = libc::S_IFDIR | 0o555; stat.st_nlink = 2; stat.st_size = 0; stat.st_blocks = 0; }
                    SeqProcSource::Link(link) => { stat.st_mode = libc::S_IFLNK | 0o777; stat.st_size = link.len() as libc::off_t; }
                    SeqProcSource::OwnedLink { .. } => { stat.st_mode = libc::S_IFLNK | 0o777; stat.st_size = 0; stat.st_blocks = 0; }
                    _ => {}
                }
            }
        } else if let Some(minor) = self.random_minor {
            stat.st_mode = libc::S_IFCHR | (stat.st_mode & 0o7777);
            stat.st_rdev = libc::makedev(1, minor); stat.st_size = 0; stat.st_blocks = 0;
            stat.st_nlink = 1; stat.st_uid = 0; stat.st_gid = 0;
            stat.st_dev = synthetic_dev(SYNTHETIC_GUEST_FD_DEV_MINOR);
            stat.st_ino = self.object.as_ref().expect("virtual random file has an identity").inode;
            sanitize_stat_timestamps(&mut stat);
        } else if let Some(seconds) = self.seconds(state) { set_stat_timestamps(&mut stat, seconds); }
        Ok(stat)
    }

    pub(super) fn statx(&self, state: &LoadedStaticElf, flags: i32, mask: u32) -> Result<libc::statx, i64> {
        if let Some(capture) = self.capture { return Ok(synthetic_captured_output_statx(capture)); }
        let mut stat = std::mem::MaybeUninit::<libc::statx>::zeroed();
        // SAFETY: the empty path and output live through the call; the pinned
        // object supplies the selected owner's FD, never a supervisor number.
        if unsafe { libc::statx(self.file.as_file().as_raw_fd(), c"".as_ptr(),
            libc::AT_EMPTY_PATH | (flags & libc::AT_STATX_SYNC_TYPE), mask, stat.as_mut_ptr()) } != 0 {
            return Err(io_error(std::io::Error::last_os_error()));
        }
        // SAFETY: statx initialized the complete result.
        let mut stat = unsafe { stat.assume_init() };
        if let Some(inode) = self.inode {
            stat = synthetic_proc_statx(inode, stat.stx_size);
            if let Some(description) = &self.description {
                match &description.source {
                    SeqProcSource::Directory(directory) => {
                        stat.stx_mode = (libc::S_IFDIR | 0o555) as u16;
                        stat.stx_nlink = 2; stat.stx_size = 0; stat.stx_blocks = 0;
                        if let Some(namespace) = &directory.namespace {
                            stat.stx_mask |= libc::STATX_MNT_ID; stat.stx_mnt_id = namespace.mounts.proc_fs.mount_id;
                        }
                    }
                    SeqProcSource::Link(link) => { stat.stx_mode = (libc::S_IFLNK | 0o777) as u16; stat.stx_size = link.len() as u64; }
                    SeqProcSource::OwnedLink { .. } => { stat.stx_mode = (libc::S_IFLNK | 0o777) as u16; stat.stx_size = 0; stat.stx_blocks = 0; }
                    _ => {}
                }
                if let Some(namespace) = description.owned_namespace() {
                    stat.stx_mask |= libc::STATX_MNT_ID;
                    stat.stx_mnt_id = namespace.mounts.proc_fs.mount_id;
                }
            }
        } else if let Some(minor) = self.random_minor {
            stat.stx_mode = libc::S_IFCHR as u16 | (stat.stx_mode & 0o7777);
            stat.stx_rdev_major = 1; stat.stx_rdev_minor = minor;
            stat.stx_nlink = 1; stat.stx_size = 0; stat.stx_blocks = 0; stat.stx_uid = 0; stat.stx_gid = 0;
            stat.stx_dev_major = SYNTHETIC_DEV_MAJOR; stat.stx_dev_minor = SYNTHETIC_GUEST_FD_DEV_MINOR;
            stat.stx_ino = self.object.as_ref().expect("virtual random file has an identity").inode;
            sanitize_statx_timestamps(&mut stat);
        } else if let Some(seconds) = self.seconds(state) { set_statx_timestamps(&mut stat, seconds); }
        Ok(stat)
    }

    fn statfs(&self) -> Result<libc::statfs, i64> {
        if self.description.as_ref().is_some_and(|description| description.is_fdinfo_entry()) {
            return Err(negative_errno(libc::ENOSYS));
        }
        if let Some(namespace) = self.description.as_ref().and_then(|description| description.owned_namespace())
        {
            return Ok(namespace.mounts.proc_fs.statfs());
        }
        let host = self.file.as_file().as_raw_fd();
        ensure_fd_not_procfs(host)?;
        let mut stat = std::mem::MaybeUninit::<libc::statfs>::zeroed();
        // SAFETY: the selected object is pinned and stat is writable.
        if unsafe { libc::fstatfs(host, stat.as_mut_ptr()) } != 0 {
            return Err(io_error(std::io::Error::last_os_error()));
        }
        // SAFETY: fstatfs initialized the complete result.
        Ok(unsafe { stat.assume_init() })
    }
}

pub(super) fn metadata(
    state: &LoadedStaticElf,
    dirfd: i32,
    path: &[u8],
    nofollow: bool,
) -> Result<Option<ProcMetadata>, i64> {
    if path.is_empty() {
        return Ok(None);
    }
    let resolution = resolve(state, dirfd, path)?;
    let stat = match resolution {
        Some(ProcPath::Directory(directory)) => {
            return Ok(Some(with_mount(directory_stat(&directory), directory.namespace.as_ref())));
        }
        Some(ProcPath::TaskLink { directory, link }) => {
            let stat = if nofollow {
                let name = if link.contains(&b'/') {
                    b"/proc/thread-self".as_slice()
                } else {
                    b"/proc/self"
                };
                let mut stat = synthetic_proc_stat(synthetic_proc_inode(name), link.len());
                stat.st_mode = libc::S_IFLNK | 0o777;
                stat
            } else {
                directory_stat(&directory)
            };
            return Ok(Some(with_mount(stat, directory.namespace.as_ref())));
        }
        Some(ProcPath::File { path, target, namespace }) => {
            if let Some(target) = target.as_ref().filter(|target| target.is_owned()) {
                let name;
                if let Some(file) = process_file_name(&path) {
                    target.live()?;
                    name = file.name().to_vec();
                } else if let Some(fd) = fdinfo_file_name(&path) {
                    target.fd_exists(state, fd)?;
                    name = format!("fdinfo/{fd}").into_bytes();
                } else if let Some(leaf) = owner_file_name(&path) {
                    target.live()?;
                    if matches!(leaf, b"cwd" | b"root" | b"exe") {
                        // A followed owner link uses its pinned endpoint. A
                        // nofollow observation never substitutes caller state.
                        if !nofollow {
                            return Ok(Some(ProcMetadata::Fd(OwnedFdMetadata::held(
                                target.owned_link_file(state, leaf)?))));
                        }
                        target.owned_link(leaf)?.ok_or_else(|| negative_errno(libc::ENOENT))?;
                        let mut stat = synthetic_proc_stat(target.inode_for_name(leaf), 0);
                        stat.st_mode = libc::S_IFLNK | 0o777;
                        return Ok(Some(with_mount(stat, target.namespace())));
                    }
                    target.owned_content(leaf)?.ok_or_else(|| negative_errno(libc::ENOENT))?;
                    name = leaf.to_vec();
                } else { return Err(negative_errno(libc::ENOENT)); }
                return Ok(Some(with_mount(synthetic_proc_stat(target.inode_for_name(&name), 0),
                    target.namespace())));
            }
            if target.is_none() && let Some(namespace) = namespace.as_ref() {
                let content = namespace_static_content(state, namespace, &path)?
                    .ok_or_else(|| negative_errno(libc::ENOENT))?;
                return Ok(Some(with_mount(synthetic_proc_stat(synthetic_proc_inode(&path),
                    synthetic_proc_path_size(&path, &content)), Some(namespace))));
            }
            if let Some(target) = &target
                && let Some(file) = process_file_name(&path)
            {
                target.live()?;
                synthetic_proc_stat(synthetic_proc_inode(&target.identity_path(file)), 0)
            } else if let Some(target) = target
                && let Some(target_path) = fdinfo_path_target(state, &path)
            {
                let fd = match target_path {
                    FdinfoPathTarget::Target { fd, .. } => fd,
                    _ => return Err(negative_errno(libc::ENOENT)),
                };
                target.fd_exists(state, fd)?;
                synthetic_proc_stat(
                    synthetic_proc_inode(format!("/proc/{}/fdinfo/{fd}", target.tid).as_bytes()),
                    0,
                )
            } else {
                return Ok(Some(ProcMetadata::Path(path)));
            }
        }
        Some(ProcPath::Fd { target, fd }) => {
            if let Some(namespace) = target.namespace() {
                let source = target.metadata_source(state, fd)?;
                return Ok(Some(if nofollow {
                    with_mount(source.link_stat(target.inode_for_name(format!("fd/{fd}").as_bytes()))?,
                        Some(namespace))
                } else { ProcMetadata::Fd(source) }));
            }
            if !target.is_current_table(state) {
                return Err(negative_errno(libc::ENOSYS));
            }
            return Ok(Some(ProcMetadata::Path(
                format!("/proc/self/fd/{fd}").into_bytes(),
            )));
        }
        Some(ProcPath::Owned { .. }) | None => return Ok(None),
    };
    Ok(Some(ProcMetadata::Stat(stat)))
}

fn with_mount(stat: libc::stat, namespace: Option<&ProcNamespace>) -> ProcMetadata {
    match namespace {
        Some(namespace) => ProcMetadata::OwnedStat { stat, mount_id: namespace.mounts.proc_fs.mount_id },
        None => ProcMetadata::Stat(stat),
    }
}

/// Only a description carrying this run's namespace may project proc mount
/// metadata. Followed magic links instead query the selected native endpoint.
pub(super) fn filesystem(
    state: &LoadedStaticElf,
    dirfd: i32,
    path: &[u8],
) -> Result<Option<libc::statfs>, i64> {
    let namespace = match resolve(state, dirfd, path)? {
        Some(ProcPath::Directory(directory)) | Some(ProcPath::TaskLink { directory, .. }) => directory.namespace,
        Some(ProcPath::File { path, target, namespace }) => {
            if let Some(target) = target.as_ref().filter(|target| target.is_owned())
                && let Some(name @ (b"cwd" | b"root" | b"exe")) = owner_file_name(&path) {
                return OwnedFdMetadata::held(target.owned_link_file(state, name)?).statfs().map(Some);
            }
            if fdinfo_file_name(&path).is_some() { return Ok(None); }
            namespace
        }
        Some(ProcPath::Fd { target, fd }) if target.is_owned() => {
            return target.metadata_source(state, fd)?.statfs().map(Some);
        }
        _ => None,
    };
    Ok(namespace.map(|namespace| namespace.mounts.proc_fs.statfs()))
}

fn directory_stat(directory: &ProcDirectory) -> libc::stat {
    let mut stat = synthetic_proc_stat(directory.inode(), 0);
    stat.st_mode = libc::S_IFDIR | 0o555;
    stat.st_nlink = 2;
    stat
}

pub(super) fn extended(stat: &libc::stat) -> libc::statx {
    let mut extended = synthetic_proc_statx(stat.st_ino, stat.st_size as u64);
    extended.stx_mode = stat.st_mode as u16;
    extended.stx_nlink = stat.st_nlink as u32;
    extended
}
