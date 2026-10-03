/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! A fake Narf kernel for driving `reverie_narf_core` without Narf.
//!
//! It names only `core` and `alloc` so that the same file is compiled into
//! the core's unit tests, its integration tests and `nostd-gate` (the includer
//! supplies `extern crate alloc`). It models the parts of Narf's hook contract
//! the core depends on and records every breach instead of hiding it:
//!
//! * each interceptor call gets a fresh transition whose original syscall runs
//!   natively at most once; a second request is a [`Violation::SecondOriginal`];
//! * injected requests are repeatable;
//! * after any transition reports `ContextManaged`, nothing may execute; a
//!   later request is a [`Violation::AfterContextManaged`];
//! * a created task is reported once, and exits are reported to the host after
//!   the interceptor returns, as Narf's teardown would. A task that exits
//!   while its own callback runs (a sibling's `exit_group` during the other
//!   tasks' work, say) is reported only once that callback has returned;
//! * each process has its own fake address space, so memory read through the
//!   wrong task's accessor faults;
//! * `wait_for_repoll` answers from a script the test sets, and is
//!   `Unsupported` once the script is used up; a `Yielded` answer first runs
//!   whatever the test gave as the other tasks' work, and a `Killed` answer
//!   ends the task's process with SIGKILL, as a pending SIGKILL ends Narf's
//!   wait, and nothing may execute after it. If the other tasks' work ended
//!   the waiting task's process, a `Yielded` answer becomes `Killed`, as
//!   Narf checks for a kill again once the task runs again;
//! * `kill(getpid(), SIGKILL)` ends the caller's process and is
//!   context-managed;
//! * an RDTSC callback first forgets the task's parked syscall, as Narf
//!   drops its park record when it delivers the instruction, and its
//!   `killed` answers whether the task has exited. Syscall and lifecycle
//!   callbacks answer `false`, as Narf's syscall transitions do.
//!
//! What it does not model:
//!
//! * the script is shared by every task, so a callback that waits while it is
//!   nested in another task's work takes the next answer; `repoll_waits`
//!   records which task asked;
//! * a task whose process ends in the middle of one of its own polls, with no
//!   wait in between, can still run transitions; Narf's signal gate would make
//!   them context-managed.

#![allow(dead_code)]

use alloc::collections::BTreeMap;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::cell::UnsafeCell;
use core::sync::atomic::AtomicBool;
use core::sync::atomic::Ordering;

use reverie::Auxv;
use reverie::ExitStatus;
use reverie::Pid;
#[cfg(target_arch = "x86_64")]
use reverie::Rdtsc;
use reverie::Signal;
use reverie::Tool;
use reverie::syscalls::Errno;
use reverie::syscalls::IoSlice;
use reverie::syscalls::IoSliceMut;
use reverie::syscalls::MemoryAccess;
use reverie::syscalls::Sysno;
use reverie::syscalls::libc;
use reverie_narf_core::CreatedTask;
use reverie_narf_core::CreatedTaskKind;
use reverie_narf_core::Disposition;
use reverie_narf_core::KernelServices;
use reverie_narf_core::LifecycleOutcome;
use reverie_narf_core::NarfFatal;
use reverie_narf_core::NarfSyscallOutcome;
use reverie_narf_core::NarfSyscallRequest;
use reverie_narf_core::NarfToolHost;
use reverie_narf_core::OriginalSyscallError;
#[cfg(target_arch = "x86_64")]
use reverie_narf_core::RdtscOutcome;
use reverie_narf_core::RepollWait;
use reverie_narf_core::SyscallEntry;
use reverie_narf_core::TaskExit;
use reverie_narf_core::TaskLock;
use reverie_narf_core::TaskTable;

/// A spin lock usable without `std`, as the kernel's would be.
pub struct SpinLock<V> {
    locked: AtomicBool,
    value: UnsafeCell<V>,
}

// SAFETY: `with` gives out the value only while `locked` is held.
unsafe impl<V: Send> Send for SpinLock<V> {}
// SAFETY: as above; one holder at a time.
unsafe impl<V: Send> Sync for SpinLock<V> {}

struct Unlock<'a>(&'a AtomicBool);

impl Drop for Unlock<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

impl<V: Send> TaskLock<V> for SpinLock<V> {
    fn new(value: V) -> Self {
        Self {
            locked: AtomicBool::new(false),
            value: UnsafeCell::new(value),
        }
    }

    fn with<R>(&self, f: impl FnOnce(&mut V) -> R) -> R {
        while self
            .locked
            .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            core::hint::spin_loop();
        }
        let _unlock = Unlock(&self.locked);
        // SAFETY: the lock is held until `_unlock` drops.
        f(unsafe { &mut *self.value.get() })
    }
}

/// The host type the fake drives.
pub type FakeHost<T> = NarfToolHost<T, SpinLock<TaskTable<T>>>;

/// A breach of the transition contract, recorded by the fake.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Violation {
    /// The original syscall was requested again in one interceptor call.
    SecondOriginal { tid: i32 },
    /// A transition, or a repoll wait, was requested after one reported
    /// `ContextManaged`.
    AfterContextManaged { tid: i32 },
    /// The original was requested from a lifecycle callback, which has none.
    OriginalOutsideSyscall { tid: i32 },
}

/// How a native execution was requested.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Via {
    /// Through `execute_original`.
    Original,
    /// Through `execute_injected`.
    Injected,
}

/// One native syscall the fake executed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Native {
    pub tid: i32,
    pub request: NarfSyscallRequest,
    pub via: Via,
}

/// Pipe read end served by the fake: `read` on it parks while it is empty.
pub const PIPE_FD: u64 = 3;
/// Value of `AT_UID` in every fake auxiliary vector.
pub const FAKE_UID: u64 = 4242;
/// Size of each fake address space.
pub const SPACE_SIZE: usize = 0x2000;
/// Linux `-ENOSYS`.
pub const ENOSYS_RET: i64 = -38;

struct Task {
    pid: i32,
    ppid: Option<i32>,
    regs: libc::user_regs_struct,
    exited: bool,
    daemon: bool,
    /// The guest syscall to re-execute after a park, if parked.
    parked: Option<NarfSyscallRequest>,
}

struct Space {
    base: usize,
    bytes: Vec<u8>,
}

impl Space {
    fn range(&self, addr: usize, len: usize) -> Result<core::ops::Range<usize>, Errno> {
        let start = addr.checked_sub(self.base).ok_or(Errno::EFAULT)?;
        let end = start.checked_add(len).ok_or(Errno::EFAULT)?;
        if end > self.bytes.len() {
            return Err(Errno::EFAULT);
        }
        Ok(start..end)
    }
}

struct World {
    tasks: BTreeMap<i32, Task>,
    spaces: BTreeMap<i32, Space>,
    next_tid: i32,
    output: Vec<u8>,
    pipe: Vec<u8>,
    natives: Vec<Native>,
    violations: Vec<Violation>,
    pending_exits: Vec<(i32, ExitStatus)>,
    /// The tasks whose interceptor or lifecycle call is running, innermost
    /// last. Their exits stay pending until the call returns.
    in_callback: Vec<i32>,
    /// Each process's group exit status (`exit_group`), the first winning.
    /// With none, Linux reports the last thread's own status.
    group_status: BTreeMap<i32, ExitStatus>,
    teardowns: Vec<(i32, Result<TaskExit, NarfFatal>)>,
    /// The answers to the next `wait_for_repoll` calls, front first.
    repoll_script: Vec<RepollWait>,
    /// The task of every `wait_for_repoll` call, in order.
    repoll_waits: Vec<i32>,
    /// The error `daemonize` fails with, if it is to fail.
    daemonize_refusal: Option<Errno>,
}

/// The fake kernel: tasks, address spaces and a log of what ran.
pub struct FakeKernel {
    /// Shared with every [`FakeMemory`], which must be `'static` because a
    /// Tool future (and anything it holds) can outlive one callback.
    world: Arc<SpinLock<World>>,
}

fn user_regs(rsp: u64) -> libc::user_regs_struct {
    // SAFETY: user_regs_struct is plain integers; all-zero is valid.
    let mut regs: libc::user_regs_struct = unsafe { core::mem::zeroed() };
    regs.rsp = rsp;
    regs
}

impl Default for FakeKernel {
    fn default() -> Self {
        Self::new()
    }
}

impl FakeKernel {
    /// An empty fake kernel whose first task ID is 1000.
    pub fn new() -> Self {
        Self {
            world: Arc::new(SpinLock::new(World {
                tasks: BTreeMap::new(),
                spaces: BTreeMap::new(),
                next_tid: 1000,
                output: Vec::new(),
                pipe: Vec::new(),
                natives: Vec::new(),
                violations: Vec::new(),
                pending_exits: Vec::new(),
                in_callback: Vec::new(),
                group_status: BTreeMap::new(),
                teardowns: Vec::new(),
                repoll_script: Vec::new(),
                repoll_waits: Vec::new(),
                daemonize_refusal: None,
            })),
        }
    }

    fn with<R>(&self, f: impl FnOnce(&mut World) -> R) -> R {
        self.world.with(f)
    }

    /// Creates a root process whose address space starts at `base`, registers
    /// it with `host` and runs its thread-start callback.
    pub fn spawn_root<T: Tool>(&self, host: &FakeHost<T>, base: usize) -> Pid {
        let tid = self.with(|world| {
            let tid = world.next_tid;
            world.next_tid += 1;
            world.spaces.insert(
                tid,
                Space {
                    base,
                    bytes: alloc::vec![0; SPACE_SIZE],
                },
            );
            world.tasks.insert(
                tid,
                Task {
                    pid: tid,
                    ppid: None,
                    regs: user_regs((base + SPACE_SIZE) as u64),
                    exited: false,
                    daemon: false,
                    parked: None,
                },
            );
            tid
        });
        let tid = Pid::from_raw(tid);
        host.register_root(tid, tid).expect("register root");
        tid
    }

    /// Services for one callback of `tid`; `original` is `None` for a
    /// lifecycle callback.
    pub fn services(&self, tid: Pid, original: Option<NarfSyscallRequest>) -> FakeServices<'_> {
        FakeServices {
            kernel: self,
            tid: tid.as_raw(),
            original,
            original_executed: false,
            context_managed: false,
            created: None,
            others: None,
            kill_query: false,
        }
    }

    /// Delivers a new guest syscall from `tid` to `host`, then reports any
    /// task that died to `host.task_exited`.
    pub fn syscall<T: Tool + 'static>(
        &self,
        host: &FakeHost<T>,
        tid: Pid,
        request: NarfSyscallRequest,
    ) -> Result<Disposition, NarfFatal> {
        self.enter(host, tid, SyscallEntry::new(request))
    }

    /// Re-executes `tid`'s parked syscall, as Narf's backstop tick does.
    pub fn reexecute<T: Tool + 'static>(
        &self,
        host: &FakeHost<T>,
        tid: Pid,
    ) -> Result<Disposition, NarfFatal> {
        let request = self
            .with(|world| {
                world
                    .tasks
                    .get_mut(&tid.as_raw())
                    .and_then(|t| t.parked.take())
            })
            .expect("task is parked");
        self.enter(host, tid, SyscallEntry::reexecution(request))
    }

    /// Delivers an arbitrary interceptor entry.
    pub fn enter<T: Tool + 'static>(
        &self,
        host: &FakeHost<T>,
        tid: Pid,
        entry: SyscallEntry,
    ) -> Result<Disposition, NarfFatal> {
        let result = self.in_callback(tid, || {
            let mut services = self.services(tid, Some(entry.request));
            host.handle_syscall(&mut services, entry)
        });
        self.report_exits(host);
        result
    }

    /// Like [`syscall`](Self::syscall), but every wait of this callback
    /// answered `Yielded` first runs `others`, as other tasks run while Narf
    /// has the waiting task switched out.
    pub fn syscall_with_others<T: Tool + 'static>(
        &self,
        host: &FakeHost<T>,
        tid: Pid,
        request: NarfSyscallRequest,
        others: &mut (dyn FnMut() + Send + Sync),
    ) -> Result<Disposition, NarfFatal> {
        let result = self.in_callback(tid, || {
            let mut services = self.services(tid, Some(request));
            services.others = Some(others);
            host.handle_syscall(&mut services, SyscallEntry::new(request))
        });
        self.report_exits(host);
        result
    }

    /// Delivers an RDTSC or RDTSCP that `tid` executed to `host`, then
    /// reports any task that died.
    #[cfg(target_arch = "x86_64")]
    pub fn rdtsc<T: Tool + 'static>(
        &self,
        host: &FakeHost<T>,
        tid: Pid,
        request: Rdtsc,
    ) -> Result<RdtscOutcome, NarfFatal> {
        self.with(|world| {
            if let Some(task) = world.tasks.get_mut(&tid.as_raw()) {
                task.parked = None;
            }
        });
        let result = self.in_callback(tid, || {
            let mut services = self.services(tid, None);
            services.kill_query = true;
            host.handle_rdtsc(&mut services, request)
        });
        self.report_exits(host);
        result
    }

    /// Runs `tid`'s thread-start callback.
    pub fn thread_start<T: Tool + 'static>(
        &self,
        host: &FakeHost<T>,
        tid: Pid,
    ) -> Result<LifecycleOutcome, NarfFatal> {
        let result = self.in_callback(tid, || {
            let mut services = self.services(tid, None);
            host.handle_thread_start(&mut services)
        });
        self.report_exits(host);
        result
    }

    /// Runs `call` as `tid`'s callback: exits reported meanwhile skip `tid`.
    fn in_callback<R>(&self, tid: Pid, call: impl FnOnce() -> R) -> R {
        self.with(|world| world.in_callback.push(tid.as_raw()));
        let result = call();
        let innermost = self.with(|world| world.in_callback.pop());
        assert_eq!(innermost, Some(tid.as_raw()), "callbacks nest");
        result
    }

    /// Reports every pending exit of a task that is not inside a callback.
    fn report_exits<T: Tool>(&self, host: &FakeHost<T>) {
        let exits = self.with(|world| {
            let (now, later): (Vec<_>, Vec<_>) = core::mem::take(&mut world.pending_exits)
                .into_iter()
                .partition(|(tid, _)| !world.in_callback.contains(tid));
            world.pending_exits = later;
            now
        });
        for (tid, status) in exits {
            let process_status = self.with(|world| {
                let pid = world.tasks.get(&tid).map_or(tid, |task| task.pid);
                world.group_status.get(&pid).copied().unwrap_or(status)
            });
            let result = host.task_exited(Pid::from_raw(tid), status, process_status);
            self.with(|world| world.teardowns.push((tid, result)));
        }
    }

    /// Answers the next `wait_for_repoll` calls with `answers`, in order;
    /// once they are used up, a wait is `Unsupported`.
    pub fn script_repoll(&self, answers: &[RepollWait]) {
        self.with(|world| world.repoll_script = answers.to_vec());
    }

    /// The task of every `wait_for_repoll` call so far, in order.
    pub fn repoll_waits(&self) -> Vec<i32> {
        self.with(|world| world.repoll_waits.clone())
    }

    /// Makes every later `daemonize` fail with `errno`.
    pub fn refuse_daemonize(&self, errno: Errno) {
        self.with(|world| world.daemonize_refusal = Some(errno));
    }

    /// Queues bytes on the pipe that `read(PIPE_FD, ..)` drains.
    pub fn push_pipe(&self, bytes: &[u8]) {
        self.with(|world| world.pipe.extend_from_slice(bytes));
    }

    /// Everything written to fds 1 and 2.
    pub fn output(&self) -> Vec<u8> {
        self.with(|world| world.output.clone())
    }

    /// Every native execution, in order.
    pub fn natives(&self) -> Vec<Native> {
        self.with(|world| world.natives.clone())
    }

    /// Every recorded contract breach.
    pub fn violations(&self) -> Vec<Violation> {
        self.with(|world| world.violations.clone())
    }

    /// Every exit reported to the host, with the host's answer.
    pub fn teardowns(&self) -> Vec<(i32, Result<TaskExit, NarfFatal>)> {
        self.with(|world| core::mem::take(&mut world.teardowns))
    }

    /// Process ID of `tid`.
    pub fn pid_of(&self, tid: Pid) -> Pid {
        Pid::from_raw(self.with(|world| world.tasks[&tid.as_raw()].pid))
    }

    /// Whether `tid` has exited.
    pub fn exited(&self, tid: Pid) -> bool {
        self.with(|world| world.tasks[&tid.as_raw()].exited)
    }

    /// Whether `tid` was daemonized.
    pub fn daemon(&self, tid: Pid) -> bool {
        self.with(|world| world.tasks[&tid.as_raw()].daemon)
    }

    /// The stack pointer `tid` entered the kernel with.
    pub fn rsp(&self, tid: Pid) -> u64 {
        self.with(|world| world.tasks[&tid.as_raw()].regs.rsp)
    }

    /// Copies `bytes` into process `pid`'s address space at `addr`.
    pub fn poke(&self, pid: Pid, addr: usize, bytes: &[u8]) {
        self.with(|world| {
            let space = world.spaces.get_mut(&pid.as_raw()).expect("space");
            let range = space.range(addr, bytes.len()).expect("in range");
            space.bytes[range].copy_from_slice(bytes);
        })
    }

    /// Reads `len` bytes of process `pid`'s address space at `addr`.
    pub fn peek(&self, pid: Pid, addr: usize, len: usize) -> Vec<u8> {
        self.with(|world| {
            let space = &world.spaces[&pid.as_raw()];
            space.bytes[space.range(addr, len).expect("in range")].to_vec()
        })
    }
}

/// Guest memory of one fake process.
pub struct FakeMemory {
    world: Arc<SpinLock<World>>,
    pid: i32,
}

impl MemoryAccess for FakeMemory {
    fn read_vectored(&self, from: &[IoSlice], to: &mut [IoSliceMut]) -> Result<usize, Errno> {
        // Remote slices name guest addresses; they are never dereferenced.
        let mut gathered = Vec::new();
        self.world.with(|world| {
            let space = world.spaces.get(&self.pid).ok_or(Errno::ESRCH)?;
            for remote in from {
                let range = space.range(remote.as_ptr() as usize, remote.len())?;
                gathered.extend_from_slice(&space.bytes[range]);
            }
            Ok::<(), Errno>(())
        })?;
        let mut copied = 0;
        for local in to.iter_mut() {
            let n = local.len().min(gathered.len() - copied);
            local[..n].copy_from_slice(&gathered[copied..copied + n]);
            copied += n;
        }
        Ok(copied)
    }

    fn write_vectored(&mut self, from: &[IoSlice], to: &mut [IoSliceMut]) -> Result<usize, Errno> {
        let mut source = Vec::new();
        for local in from {
            source.extend_from_slice(local);
        }
        self.world.with(|world| {
            let space = world.spaces.get_mut(&self.pid).ok_or(Errno::ESRCH)?;
            let mut written = 0;
            for remote in to.iter() {
                let n = remote.len().min(source.len() - written);
                let range = space.range(remote.as_ptr() as usize, n)?;
                space.bytes[range].copy_from_slice(&source[written..written + n]);
                written += n;
            }
            Ok(written)
        })
    }
}

/// [`KernelServices`] for one callback of one fake task.
pub struct FakeServices<'k> {
    kernel: &'k FakeKernel,
    tid: i32,
    original: Option<NarfSyscallRequest>,
    original_executed: bool,
    context_managed: bool,
    created: Option<CreatedTask>,
    /// The other tasks' work, run at each wait answered `Yielded`.
    others: Option<&'k mut (dyn FnMut() + Send + Sync)>,
    /// Whether `killed` reports the task's exit, as Narf's instruction
    /// transition does.
    kill_query: bool,
}

impl FakeServices<'_> {
    fn violate(&self, violation: Violation) {
        self.kernel.with(|world| world.violations.push(violation));
    }

    fn native(&mut self, request: NarfSyscallRequest, via: Via) -> NarfSyscallOutcome {
        let tid = self.tid;
        let original = self.original;
        let (outcome, created) = self.kernel.with(|world| {
            world.natives.push(Native { tid, request, via });
            run_native(world, tid, original, request)
        });
        if created.is_some() {
            self.created = created;
        }
        if outcome == NarfSyscallOutcome::ContextManaged {
            self.context_managed = true;
        }
        outcome
    }
}

fn exit_task(world: &mut World, tid: i32, status: ExitStatus, group: bool) {
    if let Some(task) = world.tasks.get_mut(&tid)
        && !task.exited
    {
        task.exited = true;
        let pid = task.pid;
        world.pending_exits.push((tid, status));
        if group {
            world.group_status.entry(pid).or_insert(status);
        }
    }
}

/// Ends every live thread of process `pid` with `status`, as `exit_group`
/// or a fatal signal does.
fn exit_process(world: &mut World, pid: i32, status: ExitStatus) {
    let threads: Vec<i32> = world
        .tasks
        .iter()
        .filter(|(_, task)| task.pid == pid && !task.exited)
        .map(|(tid, _)| *tid)
        .collect();
    for thread in threads {
        exit_task(world, thread, status, true);
    }
}

fn run_native(
    world: &mut World,
    tid: i32,
    original: Option<NarfSyscallRequest>,
    request: NarfSyscallRequest,
) -> (NarfSyscallOutcome, Option<CreatedTask>) {
    use NarfSyscallOutcome::ContextManaged;
    use NarfSyscallOutcome::Returned;

    let pid = world.tasks[&tid].pid;
    let [a0, a1, a2, _, _, _] = request.args;
    let fault = -(Errno::EFAULT.into_raw() as i64);
    match Sysno::new(request.linux_number() as usize) {
        Some(Sysno::getpid) => (Returned(pid as i64), None),
        Some(Sysno::gettid) => (Returned(tid as i64), None),
        Some(Sysno::getppid) => (Returned(world.tasks[&tid].ppid.unwrap_or(0) as i64), None),
        Some(Sysno::write) if a0 == 1 || a0 == 2 => {
            let space = &world.spaces[&pid];
            match space.range(a1 as usize, a2 as usize) {
                Ok(range) => {
                    let bytes = space.bytes[range].to_vec();
                    world.output.extend_from_slice(&bytes);
                    (Returned(a2 as i64), None)
                }
                Err(_) => (Returned(fault), None),
            }
        }
        Some(Sysno::read) if a0 == PIPE_FD => {
            if world.pipe.is_empty() {
                // Narf parks by rewinding the guest's instruction pointer and
                // re-executes the guest's own syscall on a later tick.
                let task = world.tasks.get_mut(&tid).expect("task");
                task.parked = original;
                return (ContextManaged, None);
            }
            let n = world.pipe.len().min(a2 as usize);
            let space = world.spaces.get_mut(&pid).expect("space");
            match space.range(a1 as usize, n) {
                Ok(range) => {
                    let bytes: Vec<u8> = world.pipe.drain(..n).collect();
                    space.bytes[range].copy_from_slice(&bytes);
                    (Returned(n as i64), None)
                }
                Err(_) => (Returned(fault), None),
            }
        }
        Some(Sysno::clone | Sysno::fork | Sysno::vfork) => {
            let thread = request.linux_number() == Sysno::clone.id() as u32
                && a0 & libc::CLONE_THREAD as u64 != 0;
            let child = world.next_tid;
            world.next_tid += 1;
            let parent = &world.tasks[&tid];
            let (child_pid, ppid, kind) = if thread {
                (pid, parent.ppid, CreatedTaskKind::Thread)
            } else {
                (child, Some(pid), CreatedTaskKind::Process)
            };
            let regs = parent.regs;
            if !thread {
                let space = &world.spaces[&pid];
                let copy = Space {
                    base: space.base,
                    bytes: space.bytes.clone(),
                };
                world.spaces.insert(child, copy);
            }
            world.tasks.insert(
                child,
                Task {
                    pid: child_pid,
                    ppid,
                    regs,
                    exited: false,
                    daemon: false,
                    parked: None,
                },
            );
            let created = CreatedTask {
                tid: Pid::from_raw(child),
                pid: Pid::from_raw(child_pid),
                kind,
            };
            (Returned(child as i64), Some(created))
        }
        Some(Sysno::exit) => {
            exit_task(world, tid, ExitStatus::Exited(a0 as i32), false);
            (ContextManaged, None)
        }
        Some(Sysno::exit_group) => {
            exit_process(world, pid, ExitStatus::Exited(a0 as i32));
            (ContextManaged, None)
        }
        Some(Sysno::kill) if a0 == pid as u64 && a1 == Signal::SIGKILL as u64 => {
            exit_process(world, pid, ExitStatus::Signaled(Signal::SIGKILL, false));
            (ContextManaged, None)
        }
        // A NULL filename fails as Linux fails it, before anything is
        // replaced.
        Some(Sysno::execve) if a0 == 0 => (Returned(fault), None),
        Some(Sysno::execve) => (ContextManaged, None),
        _ => (Returned(ENOSYS_RET), None),
    }
}

impl<'k> KernelServices for FakeServices<'k> {
    type Memory = FakeMemory;

    fn tid(&self) -> Pid {
        Pid::from_raw(self.tid)
    }

    fn pid(&self) -> Pid {
        Pid::from_raw(self.kernel.with(|world| world.tasks[&self.tid].pid))
    }

    fn ppid(&self) -> Option<Pid> {
        self.kernel
            .with(|world| world.tasks[&self.tid].ppid)
            .map(Pid::from_raw)
    }

    fn auxv(&self) -> Auxv {
        let pid = self.pid().as_raw() as u64;
        Auxv::from_entries([
            (libc::AT_UID as _, FAKE_UID as _),
            (libc::AT_GID as _, pid as _),
        ])
    }

    fn memory(&self) -> Self::Memory {
        FakeMemory {
            world: self.kernel.world.clone(),
            pid: self.pid().as_raw(),
        }
    }

    fn regs(&self) -> libc::user_regs_struct {
        let mut regs = self.kernel.with(|world| world.tasks[&self.tid].regs);
        if let Some(original) = self.original {
            regs.orig_rax = u64::from(original.number);
            regs.rax = u64::from(original.number);
            regs.rdi = original.args[0];
            regs.rsi = original.args[1];
            regs.rdx = original.args[2];
            regs.r10 = original.args[3];
            regs.r8 = original.args[4];
            regs.r9 = original.args[5];
        }
        regs
    }

    fn execute_original(&mut self) -> Result<NarfSyscallOutcome, OriginalSyscallError> {
        let tid = self.tid;
        if self.context_managed {
            self.violate(Violation::AfterContextManaged { tid });
            return Err(OriginalSyscallError::ContextManaged);
        }
        let Some(original) = self.original else {
            self.violate(Violation::OriginalOutsideSyscall { tid });
            return Err(OriginalSyscallError::AlreadyExecuted);
        };
        if self.original_executed {
            self.violate(Violation::SecondOriginal { tid });
            return Err(OriginalSyscallError::AlreadyExecuted);
        }
        self.original_executed = true;
        Ok(self.native(original, Via::Original))
    }

    fn execute_injected(&mut self, request: NarfSyscallRequest) -> NarfSyscallOutcome {
        if self.context_managed {
            self.violate(Violation::AfterContextManaged { tid: self.tid });
            return NarfSyscallOutcome::ContextManaged;
        }
        self.native(request, Via::Injected)
    }

    fn take_created_task(&mut self) -> Option<CreatedTask> {
        self.created.take()
    }

    fn daemonize(&mut self) -> Result<(), Errno> {
        let tid = self.tid;
        self.kernel.with(|world| {
            if let Some(errno) = world.daemonize_refusal {
                return Err(errno);
            }
            world.tasks.get_mut(&tid).expect("task").daemon = true;
            Ok(())
        })
    }

    fn wait_for_repoll(&mut self) -> RepollWait {
        let tid = self.tid;
        if self.context_managed {
            self.violate(Violation::AfterContextManaged { tid });
            return RepollWait::Unsupported;
        }
        let answer = self.kernel.with(|world| {
            world.repoll_waits.push(tid);
            if world.repoll_script.is_empty() {
                RepollWait::Unsupported
            } else {
                world.repoll_script.remove(0)
            }
        });
        match answer {
            RepollWait::Yielded => {
                if let Some(others) = self.others.as_mut() {
                    others();
                }
                if self.kernel.with(|world| world.tasks[&tid].exited) {
                    self.context_managed = true;
                    return RepollWait::Killed;
                }
            }
            RepollWait::Killed => {
                self.context_managed = true;
                self.kernel.with(|world| {
                    let pid = world.tasks[&tid].pid;
                    exit_process(world, pid, ExitStatus::Signaled(Signal::SIGKILL, false));
                });
            }
            RepollWait::Unsupported => {}
        }
        answer
    }

    fn killed(&self) -> bool {
        self.kill_query && self.kernel.with(|world| world.tasks[&self.tid].exited)
    }
}

/// A request with Narf's version byte clear.
pub fn request(sysno: Sysno, args: [u64; 6]) -> NarfSyscallRequest {
    NarfSyscallRequest {
        number: sysno.id() as u32,
        args,
    }
}
