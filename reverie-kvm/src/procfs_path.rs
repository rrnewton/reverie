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
}

impl ProcTaskTarget {
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
        }))
    }

    fn new_process(state: &LoadedStaticElf) -> Result<Arc<Self>, i64> {
        Self::new(state, state.tid)?.process()
    }

    fn process(&self) -> Result<Arc<Self>, i64> {
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
        }))
    }

    fn live(&self) -> Result<(), i64> {
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

    pub(super) fn is_current_table(&self, state: &LoadedStaticElf) -> bool {
        std::sync::Weak::ptr_eq(&self.table, &state.fdinfo_table)
    }

    fn with_table<T>(&self, f: impl FnOnce(&FileTableState) -> Result<T, i64>) -> Result<T, i64> {
        let table = self
            .table
            .upgrade()
            .ok_or_else(|| negative_errno(libc::ENOENT))?;
        let table = table.lock().expect("KVM file-table lock poisoned");
        self.live_task()?;
        f(&table)
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
            view.umask = state.umask;
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
            self.identity_path(file)
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
        if self.is_current_table(state) {
            host_fd(state, fd).ok_or_else(|| negative_errno(libc::ENOENT))?;
            validate_fdinfo_open_flags(state, flags)?;
            ensure_fdinfo_content_supported(state, fd, capture_output)?;
        } else {
            self.with_table(|table| {
                let host = table_host_fd(table, fd).ok_or_else(|| negative_errno(libc::ENOENT))?;
                validate_fdinfo_open_flags(state, flags)?;
                if table.proc_files.contains_key(&fd)
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
                ensure_fdinfo_object(host)
            })?;
        }
        if self.table.upgrade().is_none() {
            return Err(negative_errno(libc::ENOSYS));
        }
        Ok(FdinfoTarget {
            target_tid: self.tid,
            target_generation: self.generation,
            target_fd: fd,
            table: self.table.clone(),
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
    components: Vec<Vec<u8>>,
    target: Option<Arc<ProcTaskTarget>>,
    pub(super) status_flags: i32,
}

impl ProcDirectory {
    pub(super) fn path(&self) -> Vec<u8> {
        absolute(&self.components)
    }
}

pub(super) enum ProcPath {
    File {
        path: Vec<u8>,
        target: Option<Arc<ProcTaskTarget>>,
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
    } else if let Some(description) = state.fdinfo_files.get(&dirfd)
        && let SeqProcSource::Directory(directory) = &description.source
    {
        components = directory.components.clone();
        target = directory.target.clone();
    } else if state.proc_files.get(&dirfd) == Some(&synthetic_proc_inode(b"/proc")) {
        // The synthetic /proc root only lists its files to relative lookups;
        // its directories are reached through absolute paths.
        return match resolve(
            state,
            libc::AT_FDCWD,
            &[b"/proc/".as_slice(), path].concat(),
        )? {
            // Climbing above /proc must not reach the host filesystem.
            Some(ProcPath::File { path, .. }) if !path.starts_with(b"/proc/") => {
                Err(negative_errno(libc::ENOENT))
            }
            file @ Some(ProcPath::File { .. }) => Ok(file),
            _ => Err(negative_errno(libc::ENOENT)),
        };
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
                // A lookup relative to a synthetic proc directory descriptor
                // never leaves the synthetic namespace: the descriptor is
                // backed by a harmless host directory, not by the host /proc.
                if !path.starts_with(b"/") {
                    return Err(negative_errno(libc::ENOENT));
                }
                return Ok(Some(ProcPath::File {
                    path: absolute(&pending.into_iter().collect::<Vec<_>>()),
                    target: None,
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
            components.push(state.pid.to_string().into_bytes());
            let tid = if component == b"thread-self" {
                components.push(b"task".to_vec());
                components.push(state.tid.to_string().into_bytes());
                state.tid
            } else {
                state.pid
            };
            target = Some(if component == b"thread-self" {
                ProcTaskTarget::new(state, tid)?
            } else {
                ProcTaskTarget::new_process(state)?
            });
            if pending.is_empty() && !path.ends_with(b"/") {
                let link = if component == b"thread-self" {
                    format!("{}/task/{}", state.pid, state.tid).into_bytes()
                } else {
                    state.pid.to_string().into_bytes()
                };
                return Ok(Some(ProcPath::TaskLink {
                    directory: ProcDirectory {
                        components,
                        target,
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
            if raw_proc_path && tid != state.pid && !live_thread_in_process(state, tid) {
                return Ok(None);
            }
            raw_proc_path = false;
            target = Some(if tid == state.pid {
                ProcTaskTarget::new_process(state)?
            } else {
                ProcTaskTarget::new(state, tid)?
            });
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
            b"/proc/sys/kernel" => matches!(component.as_slice(), b"pty" | b"random"),
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
                target
                    .as_ref()
                    .ok_or_else(|| negative_errno(libc::ENOENT))?
                    .live_task()?;
            }
            continue;
        }
        let canonical = canonical_file(state, &components);
        let process_file = target.is_some() && process_file_name(&canonical).is_some();
        let fdinfo_file = if let Some(FdinfoPathTarget::Target { fd, .. }) =
            fdinfo_path_target(state, &canonical)
        {
            target
                .as_ref()
                .ok_or_else(|| negative_errno(libc::ENOENT))?
                .fd_exists(state, fd)?;
            true
        } else {
            false
        };
        let exists = process_file
            || synthetic_proc_content(state, &canonical).is_some()
            || guest_proc_exe_path(state, &canonical).is_some()
            || matches!(canonical.as_slice(), b"/proc/self/cwd" | b"/proc/self/root")
            || canonical == PROC_SELF_LOGINUID
            || fdinfo_file;
        if !exists {
            return Err(negative_errno(libc::ENOENT));
        }
        if !pending.is_empty() || path.ends_with(b"/") {
            return Err(negative_errno(libc::ENOTDIR));
        }
        return Ok(Some(ProcPath::File {
            path: canonical,
            target,
        }));
    }
    Ok(Some(ProcPath::Directory(ProcDirectory {
        components,
        target,
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
    if directory.components.len() == 1 {
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
    directory.status_flags = flags as i32 & libc::O_DIRECTORY;
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

pub(super) enum ProcMetadata {
    Path(Vec<u8>),
    Stat(libc::stat),
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
        Some(ProcPath::Directory(directory)) => directory_stat(&directory),
        Some(ProcPath::TaskLink { directory, link }) => {
            if nofollow {
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
            }
        }
        Some(ProcPath::File { path, target }) => {
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

fn directory_stat(directory: &ProcDirectory) -> libc::stat {
    let mut stat = synthetic_proc_stat(synthetic_proc_inode(&directory.path()), 0);
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
