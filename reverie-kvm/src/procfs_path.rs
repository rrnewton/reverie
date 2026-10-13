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

    /// A task of another guest process, named by its PID or TID. Its
    /// contents come from the view that process published in the lifecycle
    /// table, never from the host.
    fn peer(state: &LoadedStaticElf, tid: i32) -> Result<Arc<Self>, i64> {
        let lifecycle = state
            .task_lifecycle
            .lock()
            .expect("KVM lifecycle lock poisoned");
        let task = lifecycle
            .get(tid)
            .ok_or_else(|| negative_errno(libc::ENOENT))?;
        let target = Self {
            tid,
            tgid: task.tgid,
            process_generation: task.process_generation,
            generation: task.generation,
            process_directory: false,
            process_view: lifecycle
                .process_view(task.tgid, task.process_generation)
                .ok_or_else(|| negative_errno(libc::ENOENT))?,
            table: lifecycle
                .task_files(tid)
                .ok_or_else(|| negative_errno(libc::ENOENT))?,
            lifecycle: state.task_lifecycle.clone(),
            capture: lifecycle.task_capture(tid),
        };
        drop(lifecycle);
        if tid == target.tgid {
            target.process()
        } else {
            Ok(Arc::new(target))
        }
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
        // The process view samples the guest umask at file open: the caller's
        // own, or the one another process last published. Without a published
        // view a foreign process's umask is unknown, so refuse new status opens
        // rather than misreport it. Stat does not render umask and uses the
        // retained owner's cells.
        let same_process = Arc::ptr_eq(&self.lifecycle, &state.task_lifecycle)
            && lifecycle.get(state.tid).is_some_and(|task| {
                task.tgid == self.tgid && task.process_generation == self.process_generation
            });
        let mut view = self.process_view.clone();
        if same_process {
            view.umask = state.umask;
        } else if let Some(published) = lifecycle.process_view(self.tgid, self.process_generation) {
            view.umask = published.umask;
        } else if file == ProcessProcFile::Status {
            return Err(negative_errno(libc::ENOSYS));
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

    /// `cmdline` under a numerically selected process or task directory. The
    /// caller's own `/proc/self/cmdline` keeps its synthetic file.
    pub(super) fn cmdline(&self, path: &[u8]) -> Option<Vec<u8>> {
        let components = path
            .strip_prefix(b"/proc/")?
            .split(|byte| *byte == b'/')
            .collect::<Vec<_>>();
        let numeric = match components.as_slice() {
            [pid, b"cmdline"] => parse_proc_id(pid).is_some(),
            [pid, b"task", tid, b"cmdline"] => {
                parse_proc_id(pid).is_some() && parse_proc_id(tid).is_some()
            }
            _ => false,
        };
        (numeric && self.live().is_ok()).then(|| self.process_view.cmdline.clone())
    }

    /// Open descriptor numbers in this task's table, in ascending order.
    fn fds(&self, state: &LoadedStaticElf) -> Result<Vec<i32>, i64> {
        self.live_task()?;
        let mut fds = std::collections::BTreeSet::new();
        if self.is_current_table(state) {
            fds.extend((0..=2).filter(|fd| host_fd(state, *fd).is_some()));
            fds.extend(state.files.keys().copied());
        } else {
            self.with_table(|table| {
                fds.extend((0..=2).filter(|fd| table_host_fd(table, *fd).is_some()));
                fds.extend(table.files.keys().copied());
                Ok(())
            })?;
        }
        Ok(fds.into_iter().collect())
    }

    /// `stat` of the object behind another table's descriptor. Only ordinary
    /// host objects are described; a synthetic, captured or virtual-identity
    /// descriptor keeps the ENOSYS boundary rather than show its carrier.
    fn fd_stat(&self, state: &LoadedStaticElf, fd: i32) -> Result<libc::stat, i64> {
        self.with_table(|table| {
            let host = table_host_fd(table, fd).ok_or_else(|| negative_errno(libc::ENOENT))?;
            let captured = self.capture.is_some()
                && (table.stdout_alias_fds.contains(&fd)
                    || table.stderr_alias_fds.contains(&fd)
                    || ((fd == 1 || fd == 2)
                        && !table.files.contains_key(&fd)
                        && !table.closed_standard_fds.contains(&fd)));
            let virtual_identity = table
                .fd_object_inodes
                .get(&fd)
                .is_some_and(|identity| identity.timestamps != IdentityTimestamps::Host);
            if captured
                || virtual_identity
                || table.proc_files.contains_key(&fd)
                || table.fdinfo_files.contains_key(&fd)
                || table.random_device_fds.contains(&fd)
                || table.signalfd_fds.contains(&fd)
            {
                return Err(negative_errno(libc::ENOSYS));
            }
            let mut stat = std::mem::MaybeUninit::<libc::stat>::zeroed();
            // SAFETY: stat is writable and the table keeps host live.
            if unsafe { libc::fstat(host, stat.as_mut_ptr()) } != 0 {
                return Err(io_error(std::io::Error::last_os_error()));
            }
            // SAFETY: fstat initialized stat on success.
            let mut stat = unsafe { stat.assume_init() };
            if !matches!(
                stat.st_mode & libc::S_IFMT,
                libc::S_IFREG | libc::S_IFDIR | libc::S_IFCHR | libc::S_IFBLK
            ) {
                return Err(negative_errno(libc::ENOSYS));
            }
            sanitize_host_stat_timestamps(state, &mut stat);
            Ok(stat)
        })
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
            let own = tid == state.pid || live_thread_in_process(state, tid);
            if !own && tid == NAMESPACE_INIT_PID && synthetic_namespace_init(state) {
                // A directory of fixed files; nothing beneath it is a task.
                raw_proc_path = false;
                target = None;
                components.push(component);
                continue;
            }
            // Preserve the host-procfs isolation guard for a raw absolute name
            // that no guest task owns. After guest lookup, the original
            // pathname can name supervisor tasks/fds, so it must never become
            // a fallback.
            if raw_proc_path && !own && !live_guest_task(state, tid) {
                return Ok(None);
            }
            raw_proc_path = false;
            target = Some(if tid == state.pid {
                ProcTaskTarget::new_process(state)?
            } else if own {
                ProcTaskTarget::new(state, tid)?
            } else {
                ProcTaskTarget::peer(state, tid)?
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
            } else if components
                .last()
                .is_some_and(|component| component == b"task")
                && target.is_none()
            {
                // The synthetic namespace init has no guest tasks.
                return Err(negative_errno(libc::ENOENT));
            }
            continue;
        }
        let canonical = canonical_file(state, &components);
        let process_file = target.as_ref().is_some_and(|target| {
            process_file_name(&canonical).is_some() || target.cmdline(&canonical).is_some()
        });
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
        // Another process's exe link, or the synthetic init's, is not served.
        let foreign = match &target {
            Some(target) => target.tgid != state.pid,
            None => components
                .get(1)
                .is_some_and(|c| parse_proc_id(c).is_some()),
        };
        let exists = process_file
            || synthetic_proc_content(state, &canonical).is_some()
            || (!foreign && guest_proc_exe_path(state, &canonical).is_some())
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
    /// Another table's descriptor, named through `/proc/<pid>/fd/<fd>`.
    Peer(libc::stat),
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
            } else if target
                .as_ref()
                .is_some_and(|target| target.cmdline(&path).is_some())
            {
                synthetic_proc_stat(synthetic_proc_inode(&path), 0)
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
                return Ok(Some(if nofollow {
                    target.fd_exists(state, fd)?;
                    ProcMetadata::Peer(synthetic_guest_fd_symlink_stat(fd))
                } else {
                    ProcMetadata::Peer(target.fd_stat(state, fd)?)
                }));
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

/// Linux's `FIRST_PROCESS_ENTRY`: the root's own names take the positions
/// below it, `self` and `thread-self` take it and the next one, and each
/// process directory takes `TGID_OFFSET + pid`.
const FIRST_PROCESS_ENTRY: i64 = 256;
const TGID_OFFSET: i64 = FIRST_PROCESS_ENTRY + 2;
/// Linux's `PID_MAX_LIMIT`, above every PID, TID and descriptor number.
const PID_MAX_LIMIT: i64 = 4 * 1024 * 1024;
/// The position after the last entry of any directory, where Linux leaves the
/// root. A position names an entry rather than counting the entries before
/// it, so a reader resumes at the right place after an entry it already
/// passed disappears. Every position fits the host /proc root's offset limit.
const END_OF_DIRECTORY: i64 = TGID_OFFSET + PID_MAX_LIMIT;

/// Names a process or task directory may hold, in the order of Linux's
/// `tgid_base_stuff`. Only those that resolve for the directory are listed.
const PROCESS_ENTRIES: &[(&[u8], u8)] = &[
    (b"task", libc::DT_DIR),
    (b"fd", libc::DT_DIR),
    (b"fdinfo", libc::DT_DIR),
    (b"environ", libc::DT_REG),
    (b"auxv", libc::DT_REG),
    (b"status", libc::DT_REG),
    (b"comm", libc::DT_REG),
    (b"cmdline", libc::DT_REG),
    (b"stat", libc::DT_REG),
    (b"statm", libc::DT_REG),
    (b"maps", libc::DT_REG),
    (b"cwd", libc::DT_LNK),
    (b"root", libc::DT_LNK),
    (b"exe", libc::DT_LNK),
    (b"mounts", libc::DT_REG),
    (b"mountinfo", libc::DT_REG),
    (b"smaps", libc::DT_REG),
    (b"loginuid", libc::DT_REG),
];

struct ProcDirent {
    position: i64,
    inode: u64,
    kind: u8,
    name: Vec<u8>,
}

impl ProcDirent {
    fn new(position: i64, kind: u8, name: impl Into<Vec<u8>>, path: &[u8]) -> Self {
        Self {
            position,
            inode: synthetic_proc_inode(path),
            kind,
            name: name.into(),
        }
    }

    /// The `linux_dirent64` record and the number of its bytes Linux writes:
    /// the padding after the name's terminating NUL is left untouched.
    fn record(&self, next: i64) -> (Vec<u8>, usize) {
        const NAME_OFFSET: usize = 19;
        let written = NAME_OFFSET + self.name.len() + 1;
        let length = (written + 7) & !7;
        let mut record = Vec::with_capacity(length);
        record.extend_from_slice(&self.inode.to_ne_bytes());
        record.extend_from_slice(&next.to_ne_bytes());
        record.extend_from_slice(&(length as u16).to_ne_bytes());
        record.push(self.kind);
        record.extend_from_slice(&self.name);
        record.resize(length, 0);
        (record, written)
    }
}

impl ProcDirectory {
    pub(super) fn root() -> Self {
        Self {
            components: vec![b"proc".to_vec()],
            target: None,
            status_flags: 0,
        }
    }
}

/// The synthetic directory a descriptor reads, if it is one.
pub(super) fn directory_of(state: &LoadedStaticElf, fd: i32) -> Option<ProcDirectory> {
    if let Some(description) = state.fdinfo_files.get(&fd)
        && let SeqProcSource::Directory(directory) = &description.source
    {
        return Some(directory.clone());
    }
    (state.proc_files.get(&fd) == Some(&synthetic_proc_inode(b"/proc"))).then(ProcDirectory::root)
}

/// Children of a static synthetic directory, from the synthetic file table.
fn static_children(directory: &[u8]) -> Vec<(Vec<u8>, u8)> {
    let mut children = std::collections::BTreeMap::new();
    for path in SYNTHETIC_REGULAR_PROC_PATHS {
        let Some(rest) = path
            .strip_prefix(directory)
            .and_then(|rest| rest.strip_prefix(b"/"))
        else {
            continue;
        };
        if rest.starts_with(b"self/") {
            continue;
        }
        match rest.iter().position(|byte| *byte == b'/') {
            Some(slash) => {
                children.insert(rest[..slash].to_vec(), libc::DT_DIR);
            }
            // Linux's /proc/mounts is a symlink to self/mounts.
            None if path == b"/proc/mounts" => {
                children.insert(rest.to_vec(), libc::DT_LNK);
            }
            None => {
                children.entry(rest.to_vec()).or_insert(libc::DT_REG);
            }
        }
    }
    children.into_iter().collect()
}

fn child_path(directory: &[u8], name: &[u8]) -> Vec<u8> {
    [directory, b"/", name].concat()
}

/// The inode `stat` reports for an entry, as far as the resolver names it.
fn entry_inode(state: &LoadedStaticElf, resolved: &ProcPath, path: &[u8]) -> u64 {
    match resolved {
        ProcPath::Directory(directory) => synthetic_proc_inode(&directory.path()),
        ProcPath::File {
            path,
            target: Some(target),
        } if process_file_name(path).is_some() => synthetic_proc_inode(
            &target.identity_path(process_file_name(path).expect("checked process file")),
        ),
        ProcPath::File { path, .. } => {
            synthetic_proc_inode(&normalize_proc_path(state, path).unwrap_or_else(|| path.clone()))
        }
        _ => synthetic_proc_inode(path),
    }
}

/// Every entry of a synthetic directory in position order, `.` and `..`
/// first. Process directories are listed in PID order, tasks in TID order and
/// descriptors in number order, each positioned by its number.
fn directory_entries(
    state: &LoadedStaticElf,
    directory: &ProcDirectory,
) -> Result<Vec<ProcDirent>, i64> {
    if let Some(target) = &directory.target {
        target.live()?;
    }
    let path = directory.path();
    let components = directory.components.as_slice();
    let parent = absolute(&components[..components.len() - 1]);
    let mut entries = vec![
        ProcDirent::new(0, libc::DT_DIR, b".".as_slice(), &path),
        ProcDirent::new(1, libc::DT_DIR, b"..".as_slice(), &parent),
    ];
    let is_id = |component: &Vec<u8>| parse_proc_id(component).is_some();
    match components {
        [_] => {
            for (index, (name, kind)) in static_children(&path).into_iter().enumerate() {
                let position = 2 + index as i64;
                debug_assert!(position < FIRST_PROCESS_ENTRY);
                let entry_path = child_path(&path, &name);
                entries.push(ProcDirent::new(position, kind, name, &entry_path));
            }
            for (offset, name) in [b"self".as_slice(), b"thread-self"].into_iter().enumerate() {
                entries.push(ProcDirent::new(
                    FIRST_PROCESS_ENTRY + offset as i64,
                    libc::DT_LNK,
                    name,
                    &child_path(&path, name),
                ));
            }
            let mut processes: std::collections::BTreeSet<i32> = state
                .task_lifecycle
                .lock()
                .expect("KVM lifecycle lock poisoned")
                .processes()
                .map(|(tgid, _)| tgid)
                .collect();
            if synthetic_namespace_init(state) {
                processes.insert(NAMESPACE_INIT_PID);
            }
            for tgid in processes {
                let name = tgid.to_string().into_bytes();
                let entry_path = child_path(&path, &name);
                entries.push(ProcDirent::new(
                    TGID_OFFSET + i64::from(tgid),
                    libc::DT_DIR,
                    name,
                    &entry_path,
                ));
            }
        }
        [_, pid] | [_, pid, _, _]
            if is_id(pid)
                && (components.len() == 2
                    || (components[2] == b"task" && is_id(&components[3]))) =>
        {
            let thread = components.len() == 4;
            let mut position = 2;
            for &(name, kind) in PROCESS_ENTRIES {
                if thread && name == b"task" {
                    continue;
                }
                let entry_path = child_path(&path, name);
                if let Ok(Some(resolved)) = resolve(state, libc::AT_FDCWD, &entry_path) {
                    let inode = entry_inode(state, &resolved, &entry_path);
                    entries.push(ProcDirent {
                        position,
                        inode,
                        kind,
                        name: name.to_vec(),
                    });
                    position += 1;
                }
            }
        }
        [.., last] if last == b"task" => {
            let target = directory
                .target
                .as_ref()
                .ok_or_else(|| negative_errno(libc::ENOENT))?;
            let tids = target
                .lifecycle
                .lock()
                .expect("KVM lifecycle lock poisoned")
                .process_tids(target.tgid, target.process_generation);
            for tid in tids {
                let name = tid.to_string().into_bytes();
                let entry_path = child_path(&path, &name);
                entries.push(ProcDirent::new(
                    2 + i64::from(tid),
                    libc::DT_DIR,
                    name,
                    &entry_path,
                ));
            }
        }
        [.., last] if last == b"fd" => {
            let target = directory
                .target
                .as_ref()
                .ok_or_else(|| negative_errno(libc::ENOENT))?;
            for fd in target.fds(state)? {
                let name = fd.to_string().into_bytes();
                let entry_path = child_path(&path, &name);
                entries.push(ProcDirent::new(
                    2 + i64::from(fd),
                    libc::DT_LNK,
                    name,
                    &entry_path,
                ));
            }
        }
        _ => {
            for (index, (name, kind)) in static_children(&path).into_iter().enumerate() {
                let entry_path = child_path(&path, &name);
                entries.push(ProcDirent::new(2 + index as i64, kind, name, &entry_path));
            }
        }
    }
    Ok(entries)
}

/// `getdents64` on a synthetic directory. The position is the host file
/// position of the descriptor's backing file, so `dup`, `fork` and
/// `SCM_RIGHTS` aliases share it and `lseek` moves it, as on Linux.
pub(super) fn getdents64(
    memory: &GuestMemory,
    state: &LoadedStaticElf,
    file: &std::fs::File,
    directory: &ProcDirectory,
    address: u64,
    count: u32,
) -> i64 {
    // Serializes one read-and-advance of a shared position, like Linux's
    // f_pos lock.
    static POSITION: Mutex<()> = Mutex::new(());
    // Linux checks the buffer only as it copies each entry, after checking
    // that the entry fits. It keeps the count in an int; a larger one holds
    // no entry.
    let capacity = i32::try_from(count).map_or(0, |count| count as usize);
    let _position = POSITION
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    // SAFETY: file owns a live descriptor; SEEK_CUR with offset 0 only reads.
    let position = unsafe { libc::lseek(file.as_raw_fd(), 0, libc::SEEK_CUR) };
    if position < 0 {
        return io_error(std::io::Error::last_os_error());
    }
    let entries = match directory_entries(state, directory) {
        Ok(entries) => entries,
        Err(error) => return error,
    };
    let start = entries.partition_point(|entry| entry.position < position);
    let mut copied = 0;
    let mut next = position;
    for (index, entry) in entries.iter().enumerate().skip(start) {
        let following = entries
            .get(index + 1)
            .map_or(END_OF_DIRECTORY, |entry| entry.position);
        let (record, written) = entry.record(following);
        if copied + record.len() > capacity {
            if copied == 0 {
                return negative_errno(libc::EINVAL);
            }
            break;
        }
        if memory
            .user()
            .copy_to_user(address.wrapping_add(copied as u64), &record[..written])
            .is_err()
        {
            if copied == 0 {
                return negative_errno(libc::EFAULT);
            }
            break;
        }
        copied += record.len();
        next = following;
    }
    // SAFETY: file owns a live descriptor; next is a nonnegative position.
    if copied > 0 && unsafe { libc::lseek(file.as_raw_fd(), next, libc::SEEK_SET) } < 0 {
        return io_error(std::io::Error::last_os_error());
    }
    copied as i64
}
