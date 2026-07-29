/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! # Making `ptrace` async
//!
//! Getting asynchronous notifications for a tree of child processes is tricky.
//! The common way is to just call `waitpid(-1)` in the tracer process and let
//! that scoop up every event for every child of the current process. This is
//! what `strace` and `rr` do to receive `ptrace` stop events. The problem is
//! that we shouldn't do something like that in a library like Reverie since we
//! don't know what other (untraced) processes the user has spawned. Calling
//! `waitpid(-1)` will consume and "steal" exit events from processes we aren't
//! actively tracing.
//!
//! The best solution would be one where we can wait on all child processes of a
//! specific subtree.
//!
//! ## Failed ideas
//!
//!  1. As an initial dumb implementation, we simply called `waitid` on all child
//!     processes one by one in a round-robin fashion until an event was finally
//!     received. While it worked, this wasn't the best solution for two reasons:
//!     (1) it uses a lot of CPU which starves the guest of CPU resources and
//!     slows everything down to a crawl, and (2) it didn't allow us to receive
//!     `PTRACE_EVENT_EXIT` events out-of-band which is necessary for canceling
//!     pending futures in the event a guest process is suddenly killed.
//!  2. Using `pidfd_open(2)` to receive events over file descriptors would be
//!     great, but `ptrace` events are not receivable with `pidfd`. This might
//!     change in the future, but there is currently no motivation among Linux
//!     devs to implement support for that. (Folks hate the complexity of ptrace
//!     and are fearful of introducing new security vulnerabilities.)
//!  3. Using `tokio::task::spawn_blocking` to simply call `waitid()` on the
//!     process we're interested in works, but is about twice as slow as (1)
//!     because of the overhead of locking a mutex and shuffling bits of data
//!     in/out of the Tokio thread pool.
//!  4. Process groups sound like the ideal solution, but it is possible for a
//!     process to escape a process group by simply calling `setpgid(2)`. Thus,
//!     such a solution would need to be aware of all calls to `setpgid` and
//!     `setsid` to perform proper bookkeeping and maintain an internal set of
//!     process groups.
//!  5. We could fork off a child process that calls `waitpid(-1)`, which then
//!     sends events back to the tracer process via a pipe. The forked process
//!     would need to call `prctl` with `PR_SET_CHILD_SUBREAPER` so that orphaned
//!     processes don't escape the process tree. This is similar to [what Bazel
//!     does](https://jmmv.dev/2019/11/bazel-process-wrapper.html) to keep track
//!     of the process tree of a build rule. Unfortunately, this won't work
//!     because `ptrace` must be only be called by the *thread* that spawned the
//!     initial process.
//!
//! ## Current implementation
//!
//! Currently, we spawn one thread per guest thread who each call `waitid` in a
//! loop on an individual thread/process ID. The nice thing about this is that we
//! can receive `PTRACE_EVENT_EXIT` events "out-of-band" and use that to cancel
//! any futures that may be pending in a tool's `handle_syscall_event`. This
//! approach also avoids the overhead of shuffling events through Tokio's
//! blocking thread pool. (An `AtomicI32` plus a small persistent waker slot can
//! be used instead.) The downside of this approach is that we
//! can end up spawning a lot of guest threads.

use std::collections::HashMap;
use std::collections::VecDeque;
use std::collections::hash_map::Entry;
use std::fs;
use std::fs::OpenOptions;
use std::future::Future;
use std::hash::Hash;
use std::hash::Hasher;
use std::os::fd::AsRawFd;
use std::os::fd::OwnedFd;
use std::os::unix::fs::MetadataExt;
use std::os::unix::fs::OpenOptionsExt;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::LazyLock;
use std::sync::OnceLock;
use std::sync::Weak;
use std::sync::atomic::AtomicI32;
use std::sync::atomic::AtomicPtr;
use std::sync::atomic::AtomicU8;
use std::sync::atomic::Ordering;
use std::task::Context;
use std::task::Poll;
use std::task::RawWakerVTable;
use std::task::Waker;
use std::thread;
use std::thread::JoinHandle;
#[cfg(test)]
use std::thread::ThreadId;
use std::time::Duration;
use std::time::Instant;

use nix::sys::wait::WaitPidFlag;
use parking_lot::Condvar;
use parking_lot::Mutex;
use parking_lot::MutexGuard;

use super::Errno;
use super::Error;
use super::Pid;
use super::Running;
use super::Stopped;
use super::TraceeToken;
use super::Wait;
use super::waitid;

static NOTIFIER: LazyLock<Notifier> = LazyLock::new(Notifier::new);

#[cfg(test)]
static CAPTURE_ERRORS: LazyLock<Mutex<HashMap<Pid, Errno>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

#[cfg(test)]
static CAPTURE_THREAD_ERRORS: LazyLock<Mutex<HashMap<ThreadId, VecDeque<Errno>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

#[cfg(test)]
pub(super) fn inject_capture_error_for_current_thread(error: Errno) {
    CAPTURE_THREAD_ERRORS
        .lock()
        .entry(thread::current().id())
        .or_default()
        .push_back(error);
}

/// A place-holder status used to indicate that no status has been set.
const INVALID_STATUS: i32 = -1;

/// The notifier worker found that the PID is no longer a waitable child.
const ECHILD_STATUS: i32 = -2;

/// No `PTRACE_EVENT_EXIT` outcome has been published yet.
const EXIT_PENDING: i32 = 0;

/// The tracee entered `PTRACE_EVENT_EXIT`; this observation remains latched.
const EXIT_STOPPED: i32 = 1;

/// The tracee became terminal without an observed `PTRACE_EVENT_EXIT`.
const EXIT_ECHILD: i32 = 2;

const EXIT_CAP_PENDING: u8 = 0;
const EXIT_CAP_AVAILABLE: u8 = 1;
const EXIT_CAP_CLAIMED: u8 = 2;
const EXIT_CAP_EXPIRED: u8 = 3;
const EXIT_CAP_FINALIZING: u8 = 4;

const WORKER_NOT_STARTED: i32 = 0;
const WORKER_RUNNING: i32 = 1;
const WORKER_FINISHING: i32 = 2;
const WORKER_DONE: i32 = 3;

/// The number we get when in a PTRACE_EVENT_EXIT stop.
const PTRACE_EVENT_EXIT_STOP: i32 = (libc::PTRACE_EVENT_EXIT << 16) | (libc::SIGTRAP << 8) | 0x7f;

#[derive(Debug, Default)]
struct WakerSlot {
    waker: Mutex<Option<Waker>>,
    data: AtomicPtr<()>,
    vtable: AtomicPtr<RawWakerVTable>,
}

impl WakerSlot {
    /// Keeps one task registered across all status events for a PID.
    fn register(&self, waker: &Waker) -> bool {
        let data = waker.data().cast_mut();
        let vtable = std::ptr::from_ref(waker.vtable()).cast_mut();
        // The stored waker keeps its data identity live, so this pair cannot
        // be reused for a different task while it remains in the slot.

        if self.data.load(Ordering::Acquire) == data
            && self.vtable.load(Ordering::Relaxed) == vtable
        {
            return false;
        }

        let mut slot = self.waker.lock();
        if slot
            .as_ref()
            .is_some_and(|registered| registered.will_wake(waker))
        {
            return false;
        }

        *slot = Some(waker.clone());
        self.vtable.store(vtable, Ordering::Relaxed);
        // Publish data last so an Acquire match also observes the vtable.
        self.data.store(data, Ordering::Release);
        true
    }

    fn wake(&self) {
        let waker = self.waker.lock().as_ref().cloned();
        if let Some(waker) = waker {
            waker.wake();
        }
    }
}

#[derive(Debug, Default)]
struct ExitWaiter {
    waker: WakerSlot,
}

#[derive(Debug, Default)]
struct ExitWaiters {
    waiters: Mutex<Vec<Weak<ExitWaiter>>>,
}

impl ExitWaiters {
    fn register(&self, waiter: &Arc<ExitWaiter>, waker: &Waker) {
        waiter.waker.register(waker);
        let weak = Arc::downgrade(waiter);
        let mut waiters = self.waiters.lock();
        waiters.retain(|registered| registered.strong_count() != 0);
        if !waiters.iter().any(|registered| registered.ptr_eq(&weak)) {
            waiters.push(weak);
        }
    }

    fn wake_all(&self) {
        let live = {
            let mut waiters = self.waiters.lock();
            let mut live = Vec::with_capacity(waiters.len());
            waiters.retain(|waiter| {
                if let Some(waiter) = waiter.upgrade() {
                    live.push(waiter);
                    true
                } else {
                    false
                }
            });
            live
        };
        for waiter in live {
            waiter.waker.wake();
        }
    }
}

#[derive(Debug)]
struct Event {
    /// Cancellation-safe weak registrations for every pending exit waiter.
    exit_waiters: ExitWaiters,

    /// Waker for regular status events.
    status_waker: WakerSlot,

    /// Ordered regular statuses plus a retained terminal publication.
    status: Mutex<StatusState>,

    /// Wakes synchronous cancellation cleanup when a status is published.
    status_changed: Condvar,

    /// Independently retained `PTRACE_EVENT_EXIT` publication. Keeping this
    /// separate prevents a following final wait status from stealing the exit
    /// event from a held [`ExitFuture`].
    exit_status: AtomicI32,

    /// Linear claim for the one stopped-state capability represented by this
    /// exact Event generation's retained exit-stop observation.
    exit_capability: AtomicU8,

    /// Last notifier registration error. Resource/read failures are retryable
    /// and must not be collapsed into terminal ECHILD.
    registration_error: Mutex<Option<Errno>>,

    /// Monotonic activity state owned by this exact Event generation.
    worker_state: AtomicI32,
    worker_done_lock: Mutex<()>,
    worker_done_changed: Condvar,
}

#[derive(Debug)]
struct StatusState {
    pending: VecDeque<i32>,
    terminal: i32,
}

struct StatusReservation<'a> {
    status: i32,
    state: Option<MutexGuard<'a, StatusState>>,
}

impl StatusReservation<'_> {
    fn commit(mut self) {
        if let Some(state) = self.state.as_mut() {
            let committed = state.pending.pop_front();
            debug_assert_eq!(committed, Some(self.status));
        }
    }
}

impl Event {
    pub fn new() -> Self {
        Self {
            exit_waiters: ExitWaiters::default(),
            status_waker: WakerSlot::default(),
            status: Mutex::new(StatusState {
                pending: VecDeque::new(),
                terminal: INVALID_STATUS,
            }),
            status_changed: Condvar::new(),
            exit_status: AtomicI32::new(EXIT_PENDING),
            exit_capability: AtomicU8::new(EXIT_CAP_PENDING),
            registration_error: Mutex::new(None),
            worker_state: AtomicI32::new(WORKER_NOT_STARTED),
            worker_done_lock: Mutex::new(()),
            worker_done_changed: Condvar::new(),
        }
    }

    fn expire_unclaimed_exit_capability(&self) -> bool {
        loop {
            let state = self.exit_capability.load(Ordering::Acquire);
            match state {
                EXIT_CAP_PENDING | EXIT_CAP_AVAILABLE => {
                    let replacement = if state == EXIT_CAP_PENDING {
                        EXIT_CAP_FINALIZING
                    } else {
                        EXIT_CAP_EXPIRED
                    };
                    if self
                        .exit_capability
                        .compare_exchange(state, replacement, Ordering::AcqRel, Ordering::Acquire)
                        .is_ok()
                    {
                        return state == EXIT_CAP_PENDING;
                    }
                }
                EXIT_CAP_CLAIMED | EXIT_CAP_EXPIRED => return false,
                EXIT_CAP_FINALIZING => std::hint::spin_loop(),
                state => unreachable!("invalid exit capability state {state}"),
            }
        }
    }

    fn prepare_exit_capability_for_cleanup(&self, owns_claimed: bool) -> Result<(), Errno> {
        loop {
            let state = self.exit_capability.load(Ordering::Acquire);
            match state {
                EXIT_CAP_PENDING | EXIT_CAP_AVAILABLE => {
                    if self
                        .exit_capability
                        .compare_exchange(
                            state,
                            EXIT_CAP_EXPIRED,
                            Ordering::AcqRel,
                            Ordering::Acquire,
                        )
                        .is_ok()
                    {
                        self.exit_waiters.wake_all();
                        return Ok(());
                    }
                }
                EXIT_CAP_CLAIMED if owns_claimed => {
                    if self
                        .exit_capability
                        .compare_exchange(
                            EXIT_CAP_CLAIMED,
                            EXIT_CAP_EXPIRED,
                            Ordering::AcqRel,
                            Ordering::Acquire,
                        )
                        .is_ok()
                    {
                        self.exit_waiters.wake_all();
                        return Ok(());
                    }
                }
                EXIT_CAP_CLAIMED => return Err(Errno::EALREADY),
                EXIT_CAP_EXPIRED => return Ok(()),
                EXIT_CAP_FINALIZING => std::hint::spin_loop(),
                state => unreachable!("invalid exit capability state {state}"),
            }
        }
    }

    /// Replaces the status and notifies the notifier of the change. Returns the
    /// old status if there was one.
    pub fn update(&self, status: i32) -> Option<i32> {
        if status == PTRACE_EVENT_EXIT_STOP {
            let capability = self.exit_capability.compare_exchange(
                EXIT_CAP_PENDING,
                EXIT_CAP_AVAILABLE,
                Ordering::AcqRel,
                Ordering::Acquire,
            );
            debug_assert!(matches!(
                capability,
                Ok(EXIT_CAP_PENDING)
                    | Err(EXIT_CAP_AVAILABLE
                        | EXIT_CAP_CLAIMED
                        | EXIT_CAP_EXPIRED
                        | EXIT_CAP_FINALIZING)
            ));
            let previous = self.exit_status.compare_exchange(
                EXIT_PENDING,
                EXIT_STOPPED,
                Ordering::Release,
                Ordering::Acquire,
            );
            debug_assert!(matches!(previous, Ok(_) | Err(EXIT_STOPPED)));
            self.status_changed.notify_all();
            self.exit_waiters.wake_all();
            return None;
        }

        let terminal = libc::WIFEXITED(status) || libc::WIFSIGNALED(status);
        if terminal {
            // Expire an unclaimed capability before terminal status or ECHILD
            // becomes visible. CLAIMED is already a non-duplicating state.
            let finalizing = self.expire_unclaimed_exit_capability();
            let _ = self.exit_status.compare_exchange(
                EXIT_PENDING,
                EXIT_ECHILD,
                Ordering::Release,
                Ordering::Acquire,
            );
            if finalizing {
                self.exit_capability
                    .compare_exchange(
                        EXIT_CAP_FINALIZING,
                        EXIT_CAP_EXPIRED,
                        Ordering::Release,
                        Ordering::Acquire,
                    )
                    .expect("terminal exit-capability publication changed");
            }
        }
        let mut state = self.status.lock();
        let previous = if terminal {
            let previous = state.terminal;
            if previous == INVALID_STATUS || previous == ECHILD_STATUS {
                state.terminal = status;
            } else {
                debug_assert_eq!(previous, status, "terminal publication changed");
            }
            previous
        } else {
            let previous = state.pending.back().copied().unwrap_or(INVALID_STATUS);
            state.pending.push_back(status);
            previous
        };
        drop(state);
        self.status_changed.notify_all();
        if terminal {
            // A terminal publication resolves both waiter classes. ExitFuture
            // observes either the retained exit stop or typed ECHILD, while
            // WaitFuture retains the exact final status.
            self.status_waker.wake();
            self.exit_waiters.wake_all();
        } else {
            self.status_waker.wake();
        }

        (previous != INVALID_STATUS).then_some(previous)
    }

    /// Publishes a terminal `ECHILD` observation to every kind of waiter.
    fn mark_echild(&self) {
        let finalizing = self.expire_unclaimed_exit_capability();
        let _ = self.exit_status.compare_exchange(
            EXIT_PENDING,
            EXIT_ECHILD,
            Ordering::Release,
            Ordering::Acquire,
        );
        if finalizing {
            self.exit_capability
                .compare_exchange(
                    EXIT_CAP_FINALIZING,
                    EXIT_CAP_EXPIRED,
                    Ordering::Release,
                    Ordering::Acquire,
                )
                .expect("ECHILD exit-capability publication changed");
        }
        let mut state = self.status.lock();
        if state.terminal == INVALID_STATUS {
            state.terminal = ECHILD_STATUS;
        }
        drop(state);
        self.status_changed.notify_all();
        self.status_waker.wake();
        self.exit_waiters.wake_all();
    }

    fn is_terminal(&self) -> bool {
        self.status.lock().terminal != INVALID_STATUS
    }

    /// Reserves the next status without removing a fallibly decoded FIFO front.
    fn poll_status_reservation(&self, waker: &Waker) -> Poll<Result<StatusReservation<'_>, Errno>> {
        // Register the waker *before* checking the status to avoid a race condition.
        self.status_waker.register(waker);

        let state = self.status.lock();
        if let Some(status) = state.pending.front().copied() {
            return Poll::Ready(Ok(StatusReservation {
                status,
                state: Some(state),
            }));
        }
        match state.terminal {
            INVALID_STATUS => Poll::Pending,
            ECHILD_STATUS => Poll::Ready(Err(Errno::ECHILD)),
            status => {
                // Final status is immutable so old state generations retain
                // the actual exit code or terminating signal after removal.
                Poll::Ready(Ok(StatusReservation {
                    status,
                    state: None,
                }))
            }
        }
    }

    #[cfg(test)]
    fn poll_status(&self, waker: &Waker) -> Poll<Result<i32, Errno>> {
        match self.poll_status_reservation(waker) {
            Poll::Ready(Ok(reservation)) => {
                let status = reservation.status;
                reservation.commit();
                Poll::Ready(Ok(status))
            }
            Poll::Ready(Err(error)) => Poll::Ready(Err(error)),
            Poll::Pending => Poll::Pending,
        }
    }

    fn wait_pending_status(&self, timeout: Duration) -> Option<MutexGuard<'_, StatusState>> {
        let deadline = Instant::now()
            .checked_add(timeout)
            .unwrap_or_else(Instant::now);
        let mut state = self.status.lock();
        loop {
            if !state.pending.is_empty() {
                return Some(state);
            }
            if state.terminal != INVALID_STATUS {
                return None;
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return None;
            }
            self.status_changed.wait_for(&mut state, remaining);
        }
    }

    fn pending_is_empty(&self) -> bool {
        self.status.lock().pending.is_empty()
    }

    fn try_start_worker(&self) -> bool {
        self.worker_state
            .compare_exchange(
                WORKER_NOT_STARTED,
                WORKER_RUNNING,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
    }

    fn worker_is_running(&self) -> bool {
        self.worker_state.load(Ordering::Acquire) == WORKER_RUNNING
    }

    fn try_begin_unstarted_completion(&self) -> bool {
        self.worker_state
            .compare_exchange(
                WORKER_NOT_STARTED,
                WORKER_FINISHING,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
    }

    fn mark_worker_done(&self) {
        let previous = self.worker_state.swap(WORKER_DONE, Ordering::AcqRel);
        debug_assert!(matches!(previous, WORKER_RUNNING | WORKER_FINISHING));
        self.worker_done_changed.notify_all();
    }

    fn wait_worker_done(&self, timeout: Duration) -> bool {
        if self.worker_state.load(Ordering::Acquire) == WORKER_DONE {
            return true;
        }
        let deadline = Instant::now()
            .checked_add(timeout)
            .unwrap_or_else(Instant::now);
        let mut guard = self.worker_done_lock.lock();
        loop {
            if self.worker_state.load(Ordering::Acquire) == WORKER_DONE {
                return true;
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return false;
            }
            self.worker_done_changed.wait_for(&mut guard, remaining);
        }
    }

    /// Polls the event to check if there is a new status ready to be consumed.
    pub fn poll_exit(&self, waiter: &Arc<ExitWaiter>, waker: &Waker) -> Poll<Result<(), Errno>> {
        // Register before checking publication to avoid a lost wake. The weak
        // Event registration is pruned automatically if this future is dropped.
        self.exit_waiters.register(waiter, waker);

        match self.exit_status.load(Ordering::Acquire) {
            EXIT_STOPPED => loop {
                match self.exit_capability.load(Ordering::Acquire) {
                    EXIT_CAP_AVAILABLE => {
                        if self
                            .exit_capability
                            .compare_exchange(
                                EXIT_CAP_AVAILABLE,
                                EXIT_CAP_CLAIMED,
                                Ordering::AcqRel,
                                Ordering::Acquire,
                            )
                            .is_ok()
                        {
                            break Poll::Ready(Ok(()));
                        }
                    }
                    EXIT_CAP_CLAIMED | EXIT_CAP_EXPIRED => {
                        break Poll::Ready(Err(Errno::EALREADY));
                    }
                    EXIT_CAP_PENDING | EXIT_CAP_FINALIZING => std::hint::spin_loop(),
                    state => unreachable!("invalid exit capability state {state}"),
                }
            },
            EXIT_ECHILD => Poll::Ready(Err(Errno::ECHILD)),
            EXIT_PENDING => match self.exit_capability.load(Ordering::Acquire) {
                EXIT_CAP_EXPIRED => Poll::Ready(Err(Errno::EALREADY)),
                EXIT_CAP_PENDING => Poll::Pending,
                EXIT_CAP_FINALIZING => {
                    while self.exit_capability.load(Ordering::Acquire) == EXIT_CAP_FINALIZING {
                        std::hint::spin_loop();
                    }
                    self.poll_exit(waiter, waker)
                }
                state => unreachable!("unpublished exit capability state {state}"),
            },
            state => unreachable!("invalid exit publication state {state}"),
        }
    }
}

/// One immutable notifier generation carried by typed tracee states.
#[derive(Debug)]
struct EventGeneration {
    event: Arc<Event>,
    identity: OnceLock<Arc<WorkerIdentity>>,
}

#[derive(Clone, Debug)]
pub(super) struct EventHandle(Arc<EventGeneration>);

impl EventHandle {
    pub(super) fn new() -> Self {
        Self(Arc::new(EventGeneration {
            event: Arc::new(Event::new()),
            identity: OnceLock::new(),
        }))
    }

    fn with_identity(identity: Arc<WorkerIdentity>) -> Self {
        let handle = Self::new();
        handle
            .0
            .identity
            .set(identity)
            .expect("fresh event generation identity is unset");
        handle
    }

    pub(super) fn current_or_new(pid: Pid) -> Result<Self, Errno> {
        NOTIFIER.current_or_new(pid)
    }

    pub(super) fn current_or_error(pid: Pid) -> Self {
        match Self::current_or_new(pid) {
            Ok(handle) => handle,
            Err(error) => {
                let handle = Self::new();
                *handle.event().registration_error.lock() = Some(error);
                handle
            }
        }
    }

    fn event(&self) -> &Arc<Event> {
        &self.0.event
    }

    fn identity(&self) -> Option<&Arc<WorkerIdentity>> {
        self.0.identity.get()
    }

    fn bind_identity(&self, identity: Arc<WorkerIdentity>) -> Result<(), Arc<WorkerIdentity>> {
        self.0.identity.set(identity)
    }
}

impl PartialEq for EventHandle {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(self.event(), other.event())
    }
}

impl Eq for EventHandle {}

impl Hash for EventHandle {
    fn hash<H: Hasher>(&self, state: &mut H) {
        Arc::as_ptr(self.event()).hash(state);
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct WorkerProcSnapshot {
    tgid: Pid,
    tracer_pid: Pid,
    start_time: u64,
}

/// Immutable procfs generation bound to one notifier worker.
#[derive(Debug)]
struct WorkerIdentity {
    pid: Pid,
    snapshot: WorkerProcSnapshot,
    proc_dir: OwnedFd,
    proc_inode: u64,
}

impl WorkerIdentity {
    fn capture(pid: Pid) -> Result<Self, Errno> {
        #[cfg(test)]
        if let Some(error) = CAPTURE_ERRORS.lock().remove(&pid) {
            return Err(error);
        }
        #[cfg(test)]
        {
            let thread = thread::current().id();
            let mut errors = CAPTURE_THREAD_ERRORS.lock();
            if let Some(error) = errors.get_mut(&thread).and_then(VecDeque::pop_front) {
                if errors.get(&thread).is_some_and(VecDeque::is_empty) {
                    errors.remove(&thread);
                }
                return Err(error);
            }
        }
        // Ordinary direct children have TracerPid 0 and are still valid
        // waitpid targets. The tracer binding is part of the immutable
        // snapshot, but only a live tracer-owned generation may turn ECHILD
        // into a transient retry below.
        Self::capture_process(pid)
    }

    fn capture_process(pid: Pid) -> Result<Self, Errno> {
        let before = worker_proc_snapshot(pid).map_err(io_errno)?;
        let proc_dir = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_PATH | libc::O_CLOEXEC)
            .open(format!("/proc/{pid}"))
            .map_err(io_errno)?;
        let proc_inode = proc_dir.metadata().map_err(io_errno)?.ino();
        let after = worker_proc_snapshot(pid).map_err(io_errno)?;
        let current_inode = fs::metadata(format!("/proc/{pid}"))
            .map_err(io_errno)?
            .ino();
        if before != after || current_inode != proc_inode {
            return Err(Errno::ESRCH);
        }

        Ok(Self {
            pid,
            snapshot: after,
            proc_dir: proc_dir.into(),
            proc_inode,
        })
    }

    /// Returns true only while this exact procfs generation remains attached
    /// to a live thread in this tracer process.
    fn is_active_tracee(&self) -> bool {
        self.is_same_process_generation()
            && tracer_is_current(self.snapshot.tracer_pid)
            && worker_proc_snapshot(self.pid)
                .ok()
                .is_some_and(|current| current.tracer_pid == self.snapshot.tracer_pid)
    }

    fn is_same_process_generation(&self) -> bool {
        let Ok(current) = worker_proc_snapshot(self.pid) else {
            return false;
        };
        current == self.snapshot
            && fd_inode(&self.proc_dir).ok() == Some(self.proc_inode)
            && fs::metadata(format!("/proc/{}", self.pid))
                .ok()
                .map(|metadata| metadata.ino())
                == Some(self.proc_inode)
    }

    fn same_generation(&self, other: &Self) -> bool {
        self.pid == other.pid
            && self.snapshot == other.snapshot
            && self.proc_inode == other.proc_inode
    }
}

fn spawn_worker(pid: Pid, event: Arc<Event>, identity: Arc<WorkerIdentity>) -> JoinHandle<()> {
    thread::Builder::new()
        .name(format!("guest-{}", pid))
        .spawn(move || worker_thread(pid, event, identity))
        .expect("failed to spawn thread")
}

/// Waits on a process and returns the raw status. Returns `None` if the process
/// does not exist.
fn wait(pid: Pid) -> Option<i32> {
    loop {
        let result = waitid::waitpid(pid.into(), WaitPidFlag::WEXITED | WaitPidFlag::WSTOPPED);

        return match result {
            Ok(status) => Some(status.unwrap()),
            Err(Errno::EINTR) => continue,
            Err(Errno::ECHILD) => None,
            Err(err) => {
                // No other errors should be possible because we handled EINTR
                // and ECHILD. EINVAL only happens when using the API
                // incorrectly.
                panic!("waitid::waitpid({}) failed unexpectedly: {}", pid, err)
            }
        };
    }
}

/// A worker thread that simply wakes a future when a process changes state.
fn worker_thread(pid: Pid, event: Arc<Event>, identity: Arc<WorkerIdentity>) {
    let mut retrying_echild = false;
    loop {
        // Revalidate immediately before every numeric wait retry. The old
        // generation may have disappeared while this worker yielded after a
        // transient ECHILD, allowing the kernel to reuse its numeric TID.
        if retrying_echild && !identity.is_active_tracee() {
            event.mark_echild();
            break;
        }
        let Some(status) = wait(pid) else {
            if identity.is_active_tracee() {
                // A newborn auto-attached ptrace child can briefly exist with
                // this exact procfs generation before its first wait status
                // becomes visible. ECHILD is transient only in that window.
                retrying_echild = true;
                thread::sleep(Duration::from_millis(1));
                continue;
            }
            // Publish before unregistering so held and newly registered late
            // waiters both receive a typed terminal result instead of hanging.
            event.mark_echild();
            break;
        };
        retrying_echild = false;
        event.update(status);

        // Try to avoid reaching an ECHILD error by terminating the loop on the
        // last event.
        if libc::WIFEXITED(status) || libc::WIFSIGNALED(status) {
            break;
        }
    }
    event.mark_worker_done();
    // The worker owns terminal registry cleanup. A WaitFuture may be dropped
    // before the final status is polled, and leaving cleanup to that future
    // would retain a stale event if the kernel later reuses this PID.
    NOTIFIER.remove(pid, &event);
}

fn worker_process_start_time(pid: Pid) -> std::io::Result<u64> {
    let stat = fs::read_to_string(format!("/proc/{pid}/stat"))?;
    let fields = stat
        .rsplit_once(") ")
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidData, "malformed stat"))?
        .1;
    fields
        .split_ascii_whitespace()
        .nth(19)
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidData, "missing starttime"))?
        .parse()
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))
}

fn worker_status_pid(status: &str, name: &str) -> std::io::Result<Pid> {
    status
        .lines()
        .find_map(|line| line.strip_prefix(name))
        .and_then(|value| value.trim().parse::<i32>().ok())
        .map(Pid::from_raw)
        .ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("missing or malformed {name}"),
            )
        })
}

fn worker_proc_snapshot(pid: Pid) -> std::io::Result<WorkerProcSnapshot> {
    let start_time = worker_process_start_time(pid)?;
    let status = fs::read_to_string(format!("/proc/{pid}/status"))?;
    let snapshot = WorkerProcSnapshot {
        tgid: worker_status_pid(&status, "Tgid:")?,
        tracer_pid: worker_status_pid(&status, "TracerPid:")?,
        start_time,
    };
    if worker_process_start_time(pid)? != start_time {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "tracee identity changed while reading procfs",
        ));
    }
    Ok(snapshot)
}

fn tracer_is_current(tracer_pid: Pid) -> bool {
    tracer_pid.as_raw() > 0
        && std::path::Path::new(&format!("/proc/self/task/{tracer_pid}")).exists()
}

fn fd_inode(fd: &OwnedFd) -> std::io::Result<u64> {
    fs::metadata(format!("/proc/self/fd/{}", fd.as_raw_fd())).map(|metadata| metadata.ino())
}

fn io_errno(error: std::io::Error) -> Errno {
    Errno::new(error.raw_os_error().unwrap_or(libc::EIO))
}

#[derive(Debug)]
struct NotifierEntry {
    handle: EventHandle,
    identity: Arc<WorkerIdentity>,
}

struct Notifier {
    /// Mapping of numeric PIDs to their validated current proc generation.
    pids: Mutex<HashMap<Pid, NotifierEntry>>,
}

impl Notifier {
    /// Creates the notifier.
    pub fn new() -> Self {
        let pids = Mutex::new(HashMap::new());
        Notifier { pids }
    }

    fn capture_identity(&self, pid: Pid) -> Result<Arc<WorkerIdentity>, Errno> {
        WorkerIdentity::capture(pid).map(Arc::new)
    }

    fn current_or_new(&self, pid: Pid) -> Result<EventHandle, Errno> {
        // Capture before locking or mutating the registry. A stale numeric
        // entry is never evidence about the process currently using that PID.
        let current = self.capture_identity(pid)?;
        let mut pids = self.pids.lock();
        match pids.entry(pid) {
            Entry::Occupied(occupied) if occupied.get().identity.same_generation(&current) => {
                Ok(occupied.get().handle.clone())
            }
            Entry::Occupied(mut occupied) => {
                let handle = EventHandle::with_identity(Arc::clone(&current));
                occupied.insert(NotifierEntry {
                    handle: handle.clone(),
                    identity: current,
                });
                Ok(handle)
            }
            Entry::Vacant(_) => {
                let handle = EventHandle::with_identity(Arc::clone(&current));
                // A typed state may still use synchronous wait. Defer registry
                // insertion until async notification or terminal cleanup is
                // actually requested.
                Ok(handle)
            }
        }
    }

    fn resolve_echild(&self, pid: Pid, handle: &EventHandle) -> Arc<Event> {
        let event = Arc::clone(handle.event());
        if event.worker_is_running() {
            return event;
        }
        if event.try_begin_unstarted_completion() {
            // Publish the terminal result before completion becomes visible.
            event.mark_echild();
            event.mark_worker_done();
            self.remove(pid, &event);
        }
        event
    }

    fn record_registration_error(handle: &EventHandle, error: Errno) {
        *handle.event().registration_error.lock() = Some(error);
    }

    /// Registers the exact event generation carried by a typed state.
    fn event(&self, pid: Pid, handle: &EventHandle) -> Result<Arc<Event>, Errno> {
        let requested = handle.event();
        if requested.is_terminal() {
            return Ok(Arc::clone(requested));
        }

        // Event-local RUNNING is the ownership proof. Registry replacement is
        // allowed while an old typed state still awaits its own worker, and
        // that old worker must not consult the replacement generation or
        // recapture /proc before publishing its final status.
        if requested.worker_is_running() {
            return Ok(Arc::clone(requested));
        }

        let current = match self.capture_identity(pid) {
            Ok(identity) => identity,
            Err(Errno::ENOENT | Errno::ESRCH) => {
                return Ok(self.resolve_echild(pid, handle));
            }
            Err(error) => {
                Self::record_registration_error(handle, error);
                return Err(error);
            }
        };

        if handle
            .identity()
            .is_some_and(|bound| !bound.same_generation(&current))
        {
            return Ok(self.resolve_echild(pid, handle));
        }

        let mut pids = self.pids.lock();
        let mut worker_identity = None;
        let event = match pids.entry(pid) {
            Entry::Occupied(occupied) if occupied.get().handle == *handle => {
                if !occupied.get().identity.same_generation(&current) {
                    drop(pids);
                    return Ok(self.resolve_echild(pid, handle));
                }
                if requested.try_start_worker() {
                    worker_identity = Some(Arc::clone(&occupied.get().identity));
                }
                Arc::clone(requested)
            }
            Entry::Occupied(occupied) if occupied.get().identity.same_generation(&current) => {
                drop(pids);
                return Ok(self.resolve_echild(pid, handle));
            }
            Entry::Occupied(mut occupied) => {
                if handle.identity().is_none() {
                    handle
                        .bind_identity(Arc::clone(&current))
                        .map_err(|_| Errno::ESRCH)?;
                }
                occupied.insert(NotifierEntry {
                    handle: handle.clone(),
                    identity: Arc::clone(&current),
                });
                if requested.try_start_worker() {
                    worker_identity = Some(current);
                }
                Arc::clone(requested)
            }
            Entry::Vacant(vacant) => {
                if handle.identity().is_none() {
                    handle
                        .bind_identity(Arc::clone(&current))
                        .map_err(|_| Errno::ESRCH)?;
                }
                vacant.insert(NotifierEntry {
                    handle: handle.clone(),
                    identity: Arc::clone(&current),
                });
                if requested.try_start_worker() {
                    worker_identity = Some(current);
                }
                Arc::clone(requested)
            }
        };
        *requested.registration_error.lock() = None;
        drop(pids);
        if let Some(identity) = worker_identity {
            spawn_worker(pid, Arc::clone(&event), identity);
        }
        Ok(event)
    }

    /// Removes a completed PID without disturbing a reused PID's event.
    fn remove(&self, pid: Pid, event: &Arc<Event>) {
        let mut pids = self.pids.lock();
        if pids
            .get(&pid)
            .is_some_and(|current| Arc::ptr_eq(current.handle.event(), event))
        {
            pids.remove(&pid);
        }
    }
}

impl Drop for Notifier {
    fn drop(&mut self) {
        // All guests should have exited by now.
        let pids = self.pids.lock();
        assert_eq!(
            pids.len(),
            0,
            "Some tracees have not exited yet:\n{:#?}",
            pids
        );
    }
}

/// A synchronous acknowledgment that a PID's notifier worker has observed a
/// terminal state and removed its registry entry.
// AUTONOMOUS-BOT-IMPLEMENTED
// TODO-HUMAN-REVIEW(PR-270): Trigger 2: review the public generation-bound
// terminal-cleanup acknowledgment contract.
pub struct TerminalCleanup {
    pid: Pid,
    event: EventHandle,
}

impl TerminalCleanup {
    pub(super) fn new(pid: Pid, token: &TraceeToken) -> Self {
        let event = token.event().clone();
        let _ = NOTIFIER.event(pid, &event);
        Self { pid, event }
    }

    /// Retries notifier registration and returns the exact capture/open error.
    pub fn ensure_registered(&self) -> Result<(), Errno> {
        NOTIFIER.event(self.pid, &self.event).map(drop)
    }

    /// Returns the last typed notifier registration error, if any.
    pub fn registration_error(&self) -> Option<Errno> {
        *self.event.event().registration_error.lock()
    }

    /// Returns true when both handles carry the same immutable Event generation.
    pub fn same_generation(&self, other: &Self) -> bool {
        self.event == other.event
    }

    /// Waits up to `timeout` for the notifier worker to unregister this PID.
    ///
    /// This does not call `waitpid`: after notifier registration, the worker
    /// thread remains the sole owner of wait statuses for the PID.
    pub fn wait(&self, timeout: Duration) -> bool {
        self.event.event().wait_worker_done(timeout)
    }

    /// Reserves the oldest queued nonterminal state after cancellation.
    ///
    /// This is a cancellation-only escape hatch for the ptracer thread that
    /// owns this exact event generation. The FIFO front remains present and
    /// unavailable to other consumers until [`PendingStatusReservation::commit`].
    /// Dropping the reservation performs an allocation-free rollback without
    /// changing FIFO order.
    pub fn reserve_pending_for_cleanup(
        &self,
        timeout: Duration,
    ) -> Option<PendingStatusReservation<'_>> {
        let state = self.event.event().wait_pending_status(timeout)?;
        let status = *state
            .pending
            .front()
            .expect("pending cleanup reservation requires a FIFO front");
        Some(PendingStatusReservation {
            pid: self.pid,
            status,
            event: &self.event,
            state,
        })
    }

    /// Returns true when no nonterminal status remains queued.
    pub fn pending_is_empty(&self) -> bool {
        self.event.event().pending_is_empty()
    }

    /// Returns true when this exact event observed a ptrace exit stop.
    pub fn exit_stop_observed(&self) -> bool {
        self.event.event().exit_status.load(Ordering::Acquire) == EXIT_STOPPED
    }

    /// Revokes any not-yet-claimed exit-stop capability before cancellation
    /// cleanup performs a raw ptrace transition.
    ///
    /// Returns [`Errno::EALREADY`] rather than advancing behind a capability
    /// already minted as [`Stopped`].
    pub fn revoke_unclaimed_exit_stop(&self) -> Result<(), Errno> {
        self.event
            .event()
            .prepare_exit_capability_for_cleanup(false)
    }

    /// Transfers a previously claimed exit-stop capability to cancellation
    /// cleanup and revokes all future claims.
    ///
    /// # Safety
    ///
    /// The caller must prove exclusive ownership of the exact stopped tracee
    /// generation and that the previously returned [`Stopped`] value has been
    /// destroyed or transferred to the cleanup path. Revocation cannot make an
    /// independently retained `Stopped` value safe.
    pub unsafe fn revoke_owned_exit_stop(&self) -> Result<(), Errno> {
        self.event.event().prepare_exit_capability_for_cleanup(true)
    }
}

/// A rollback-safe reservation of one exact-generation notifier FIFO front.
#[must_use = "drop rolls the reservation back; call commit after ownership is stored"]
pub struct PendingStatusReservation<'a> {
    pid: Pid,
    status: i32,
    event: &'a EventHandle,
    state: MutexGuard<'a, StatusState>,
}

impl PendingStatusReservation<'_> {
    /// Decodes the reserved status without removing it from the FIFO.
    pub fn decode(&self) -> Result<Wait, Error> {
        Wait::from_raw_with_token(
            self.pid,
            self.status,
            TraceeToken::from_event(self.event.clone()),
        )
    }

    /// Removes the reserved front after all associated ownership is durable.
    pub fn commit(mut self) {
        let committed = self.state.pending.pop_front();
        debug_assert_eq!(committed, Some(self.status));
    }
}

/// A future representing a process state change.
pub struct WaitFuture {
    running: Running,
}

impl WaitFuture {
    pub(super) fn new(running: Running) -> Self {
        Self { running }
    }
}

impl Future for WaitFuture {
    type Output = Result<Wait, Error>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context) -> Poll<Self::Output> {
        let this = self.get_mut();
        let pid = this.running.pid();
        let event = match NOTIFIER.event(pid, this.running.token().event()) {
            Ok(event) => event,
            Err(error) => return Poll::Ready(Err(error.into())),
        };
        let reservation = match futures::ready!(event.poll_status_reservation(cx.waker())) {
            Ok(reservation) => reservation,
            Err(errno) => return Poll::Ready(Err(errno.into())),
        };
        let decoded =
            Wait::from_raw_with_token(pid, reservation.status, this.running.token().clone());
        if decoded.is_ok() {
            reservation.commit();
        }
        Poll::Ready(decoded)
    }
}

/// A future representing PTRACE_EVENT_EXIT. The future resolves when the process
/// receives a PTRACE_EVENT_EXIT. A process can receive this event at any time,
/// even when in another ptrace stop state.
///
/// The next state after this should be the final exit status.
/// Exactly one future for an immutable Event generation can claim and return
/// the stopped-state capability. Duplicate or re-polled futures return
/// [`Errno::EALREADY`]. An unclaimed capability also expires before terminal
/// publication or cancellation cleanup advances the tracee; terminal status
/// remains independently retained for ordinary waiters.
// AUTONOMOUS-BOT-IMPLEMENTED
// TODO-HUMAN-REVIEW(PR-270): Trigger 2: review the public typed-error
// ExitFuture contract and retained exit-stop generation semantics.
pub struct ExitFuture {
    pid: Pid,
    event: EventHandle,
    waiter: Arc<ExitWaiter>,
}

impl ExitFuture {
    pub(super) fn new(pid: Pid, token: &TraceeToken) -> Self {
        Self {
            pid,
            event: token.event().clone(),
            waiter: Arc::new(ExitWaiter::default()),
        }
    }
}

impl Future for ExitFuture {
    type Output = Result<Stopped, Error>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context) -> Poll<Self::Output> {
        let this = self.get_mut();
        let event = match NOTIFIER.event(this.pid, &this.event) {
            Ok(event) => event,
            Err(error) => return Poll::Ready(Err(error.into())),
        };
        match futures::ready!(event.poll_exit(&this.waiter, cx.waker())) {
            Ok(()) => Poll::Ready(Ok(Stopped::from_token(
                this.pid,
                TraceeToken::from_event(this.event.clone()),
            ))),
            Err(errno) => Poll::Ready(Err(errno.into())),
        }
    }
}

#[cfg(test)]
mod test {
    use std::env;
    use std::mem;
    use std::process::Command;
    use std::sync::atomic::AtomicUsize;
    use std::task::Wake;
    use std::time::Duration;

    use nix::sys::signal::Signal;
    use nix::sys::wait::WaitStatus;
    use nix::unistd::ForkResult;
    use nix::unistd::Pid;
    use nix::unistd::fork;

    use super::*;
    use crate::Options;

    #[derive(Default)]
    struct WakeCounter(AtomicUsize);

    impl Wake for WakeCounter {
        fn wake(self: Arc<Self>) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }

        fn wake_by_ref(self: &Arc<Self>) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }

    #[test]
    fn registration_is_reused_across_status_events() {
        let counter = Arc::new(WakeCounter::default());
        let waker = Waker::from(Arc::clone(&counter));
        let event = Event::new();

        assert_eq!(event.poll_status(&waker), Poll::Pending);
        assert_eq!(event.poll_status(&waker), Poll::Pending);

        let stopped = (libc::SIGSTOP << 8) | 0x7f;
        assert_eq!(event.update(stopped), None);
        assert_eq!(counter.0.load(Ordering::SeqCst), 1);
        assert_eq!(event.poll_status(&waker), Poll::Ready(Ok(stopped)));
        assert!(!event.status_waker.register(&waker));
    }

    #[test]
    fn registration_updates_when_the_executor_changes_wakers() {
        let slot = WakerSlot::default();
        let first = Waker::from(Arc::new(WakeCounter::default()));
        let second_counter = Arc::new(WakeCounter::default());
        let second = Waker::from(Arc::clone(&second_counter));

        assert!(slot.register(&first));
        assert!(!slot.register(&first));
        assert!(slot.register(&second));
        slot.wake();
        assert_eq!(second_counter.0.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn exit_event_code() {
        assert_eq!(
            WaitStatus::from_raw(Pid::from_raw(42), PTRACE_EVENT_EXIT_STOP),
            Ok(WaitStatus::PtraceEvent(
                Pid::from_raw(42),
                Signal::SIGTRAP,
                libc::PTRACE_EVENT_EXIT
            ))
        );
    }

    #[test]
    fn held_unclaimed_exit_waiter_expires_before_terminal_publication() {
        let counter = Arc::new(WakeCounter::default());
        let waker = Waker::from(Arc::clone(&counter));
        let event = Event::new();
        let waiter = Arc::new(ExitWaiter::default());

        assert_eq!(event.poll_exit(&waiter, &waker), Poll::Pending);
        event.update(PTRACE_EVENT_EXIT_STOP);
        event.update(42 << 8);

        assert_eq!(
            event.poll_exit(&waiter, &waker),
            Poll::Ready(Err(Errno::EALREADY))
        );
        assert_eq!(event.poll_status(&waker), Poll::Ready(Ok(42 << 8)));
    }

    #[test]
    fn exit_stop_capability_is_single_claim_while_terminal_status_fans_out() {
        let waker = Waker::from(Arc::new(WakeCounter::default()));
        let event = Event::new();
        let winner = Arc::new(ExitWaiter::default());
        let duplicate = Arc::new(ExitWaiter::default());

        event.update(PTRACE_EVENT_EXIT_STOP);
        assert_eq!(event.poll_exit(&winner, &waker), Poll::Ready(Ok(())));
        assert_eq!(
            event.poll_exit(&duplicate, &waker),
            Poll::Ready(Err(Errno::EALREADY)),
            "one Event generation minted two exit-stop capabilities"
        );

        event.update(42 << 8);
        assert_eq!(event.poll_status(&waker), Poll::Ready(Ok(42 << 8)));
        assert_eq!(event.poll_status(&waker), Poll::Ready(Ok(42 << 8)));
        assert_eq!(
            event.poll_exit(&winner, &waker),
            Poll::Ready(Err(Errno::EALREADY))
        );
    }

    #[test]
    fn cleanup_requires_exclusive_transfer_of_claimed_exit_stop() {
        let waker = Waker::from(Arc::new(WakeCounter::default()));
        let event = Event::new();
        let waiter = Arc::new(ExitWaiter::default());

        event.update(PTRACE_EVENT_EXIT_STOP);
        assert_eq!(event.poll_exit(&waiter, &waker), Poll::Ready(Ok(())));
        assert_eq!(
            event.prepare_exit_capability_for_cleanup(false),
            Err(Errno::EALREADY)
        );
        event
            .prepare_exit_capability_for_cleanup(true)
            .expect("exclusive cleanup transfer rejected claimed exit stop");
        assert_eq!(
            event.poll_exit(&waiter, &waker),
            Poll::Ready(Err(Errno::EALREADY))
        );
    }

    #[test]
    fn simultaneous_exit_waiters_are_all_woken_for_one_claim() {
        for claim_a_first in [true, false] {
            let counter_a = Arc::new(WakeCounter::default());
            let counter_b = Arc::new(WakeCounter::default());
            let waker_a = Waker::from(Arc::clone(&counter_a));
            let waker_b = Waker::from(Arc::clone(&counter_b));
            let event = Event::new();
            let waiter_a = Arc::new(ExitWaiter::default());
            let waiter_b = Arc::new(ExitWaiter::default());

            assert_eq!(event.poll_exit(&waiter_a, &waker_a), Poll::Pending);
            assert_eq!(event.poll_exit(&waiter_b, &waker_b), Poll::Pending);
            event.update(PTRACE_EVENT_EXIT_STOP);

            assert_eq!(counter_a.0.load(Ordering::SeqCst), 1);
            assert_eq!(counter_b.0.load(Ordering::SeqCst), 1);
            let (winner, winner_waker, duplicate, duplicate_waker) = if claim_a_first {
                (&waiter_a, &waker_a, &waiter_b, &waker_b)
            } else {
                (&waiter_b, &waker_b, &waiter_a, &waker_a)
            };
            assert_eq!(event.poll_exit(winner, winner_waker), Poll::Ready(Ok(())));
            assert_eq!(
                event.poll_exit(duplicate, duplicate_waker),
                Poll::Ready(Err(Errno::EALREADY))
            );
        }
    }

    #[test]
    fn cancelled_last_exit_waiter_does_not_orphan_first_waiter() {
        let counter_a = Arc::new(WakeCounter::default());
        let counter_b = Arc::new(WakeCounter::default());
        let waker_a = Waker::from(Arc::clone(&counter_a));
        let waker_b = Waker::from(Arc::clone(&counter_b));
        let event = Event::new();
        let waiter_a = Arc::new(ExitWaiter::default());
        let waiter_b = Arc::new(ExitWaiter::default());

        assert_eq!(event.poll_exit(&waiter_a, &waker_a), Poll::Pending);
        assert_eq!(event.poll_exit(&waiter_b, &waker_b), Poll::Pending);
        drop(waiter_b);
        event.update(PTRACE_EVENT_EXIT_STOP);

        assert_eq!(counter_a.0.load(Ordering::SeqCst), 1);
        assert_eq!(counter_b.0.load(Ordering::SeqCst), 0);
        assert_eq!(event.poll_exit(&waiter_a, &waker_a), Poll::Ready(Ok(())));
        event.update(42 << 8);
        assert_eq!(event.poll_status(&waker_a), Poll::Ready(Ok(42 << 8)));
        assert_eq!(event.poll_status(&waker_a), Poll::Ready(Ok(42 << 8)));
    }

    #[test]
    fn terminal_status_remains_available_to_late_waiters() {
        let waker = Waker::from(Arc::new(WakeCounter::default()));

        for terminal in [42 << 8, Signal::SIGILL as i32] {
            let event = Event::new();
            let waiter = Arc::new(ExitWaiter::default());
            event.update(PTRACE_EVENT_EXIT_STOP);
            event.update(terminal);

            assert_eq!(event.poll_status(&waker), Poll::Ready(Ok(terminal)));
            assert_eq!(event.poll_status(&waker), Poll::Ready(Ok(terminal)));
            assert_eq!(
                event.poll_exit(&waiter, &waker),
                Poll::Ready(Err(Errno::EALREADY))
            );
        }
    }

    #[test]
    fn queued_stop_precedes_monotonic_terminal_status() {
        let waker = Waker::from(Arc::new(WakeCounter::default()));
        let event = Event::new();
        let stopped = (Signal::SIGSTOP as i32) << 8 | 0x7f;
        let terminal = 42 << 8;

        event.update(stopped);
        event.update(terminal);

        assert_eq!(event.poll_status(&waker), Poll::Ready(Ok(stopped)));
        assert_eq!(event.poll_status(&waker), Poll::Ready(Ok(terminal)));
        assert_eq!(event.poll_status(&waker), Poll::Ready(Ok(terminal)));
    }

    #[test]
    fn failed_cleanup_decode_preserves_fifo_front_for_retry() {
        let pid = Pid::from_raw(i32::MAX - 31);
        let handle = EventHandle::new();
        let child_event = (libc::PTRACE_EVENT_FORK << 16) | (libc::SIGTRAP << 8) | 0x7f;
        handle.event().update(child_event);
        let cleanup = TerminalCleanup {
            pid: pid.into(),
            event: handle,
        };

        let reservation = cleanup
            .reserve_pending_for_cleanup(Duration::ZERO)
            .expect("reserve child-event FIFO front");
        assert!(reservation.decode().is_err(), "fake child event decoded");
        drop(reservation);
        let retry = cleanup
            .reserve_pending_for_cleanup(Duration::ZERO)
            .expect("decode failure removed FIFO front");
        assert_eq!(retry.status, child_event);
    }

    #[test]
    fn worker_completion_does_not_hide_pending_cleanup_status() {
        let pid = Pid::from_raw(i32::MAX - 32);
        let handle = EventHandle::new();
        let stopped = (Signal::SIGSTOP as i32) << 8 | 0x7f;
        handle.event().update(stopped);
        assert!(handle.event().try_start_worker());
        handle.event().mark_worker_done();
        let cleanup = TerminalCleanup {
            pid: pid.into(),
            event: handle,
        };

        assert!(cleanup.wait(Duration::ZERO));
        assert!(!cleanup.pending_is_empty());
    }

    #[test]
    fn terminal_cleanup_removes_stale_pid_registration() {
        let pid = Pid::from_raw(i32::MAX - 17);
        let running = Running::new(pid.into());
        let cleanup = running.terminal_cleanup();
        let old_event = cleanup.event.clone();

        assert!(cleanup.wait(Duration::from_secs(1)));
        let replacement_handle = EventHandle::new();
        let replacement = NOTIFIER
            .event(pid.into(), &replacement_handle)
            .expect("resolve absent replacement");
        assert!(
            !Arc::ptr_eq(old_event.event(), &replacement),
            "terminal cleanup retained a stale PID registry entry"
        );
    }

    #[test]
    fn terminal_cleanup_surfaces_and_retries_typed_capture_error() {
        let child = spawn_stopped_process(None).expect("spawn capture-error child");
        let pid = Pid::from_raw(child.as_raw());
        CAPTURE_ERRORS.lock().insert(pid.into(), Errno::EMFILE);

        let cleanup = Running::new(pid.into()).terminal_cleanup();
        assert_eq!(cleanup.registration_error(), Some(Errno::EMFILE));
        assert!(!cleanup.wait(Duration::ZERO));
        cleanup
            .ensure_registered()
            .expect("retry notifier registration after EMFILE");
        assert_eq!(cleanup.registration_error(), None);

        assert_eq!(unsafe { libc::kill(pid.as_raw(), libc::SIGKILL) }, 0);
        assert!(cleanup.wait(Duration::from_secs(1)));
        assert!(!std::path::Path::new(&format!("/proc/{pid}")).exists());
    }

    #[test]
    fn registry_replacement_does_not_terminalize_running_old_event() {
        let child = spawn_stopped_process(None).expect("spawn old active registry tracee");
        let pid = Pid::from_raw(child.as_raw());
        let identity = Arc::new(
            WorkerIdentity::capture_process(pid.into()).expect("capture old active generation"),
        );
        let old_handle = EventHandle::with_identity(Arc::clone(&identity));
        assert!(old_handle.event().try_start_worker());
        NOTIFIER.pids.lock().insert(
            pid.into(),
            NotifierEntry {
                handle: old_handle.clone(),
                identity: Arc::clone(&identity),
            },
        );
        let replacement = EventHandle::with_identity(Arc::clone(&identity));
        NOTIFIER.pids.lock().insert(
            pid.into(),
            NotifierEntry {
                handle: replacement,
                identity,
            },
        );

        NOTIFIER.resolve_echild(pid.into(), &old_handle);
        let terminal = old_handle.event().status.lock().terminal;
        NOTIFIER.pids.lock().remove(&pid.into());
        reap_stopped_process(child);

        assert_eq!(
            terminal, INVALID_STATUS,
            "registry replacement terminalized an Event with its own active worker"
        );
    }

    #[test]
    fn running_old_event_bypasses_replacement_capture_failure() {
        let child = spawn_stopped_process(None).expect("spawn old event fast-path tracee");
        let pid = Pid::from_raw(child.as_raw());
        let identity = Arc::new(
            WorkerIdentity::capture_process(pid.into()).expect("capture old event generation"),
        );
        let old_handle = EventHandle::with_identity(Arc::clone(&identity));
        assert!(old_handle.event().try_start_worker());
        let replacement = EventHandle::with_identity(Arc::clone(&identity));
        NOTIFIER.pids.lock().insert(
            pid.into(),
            NotifierEntry {
                handle: replacement,
                identity,
            },
        );

        inject_capture_error_for_current_thread(Errno::EMFILE);
        let selected = NOTIFIER
            .event(pid.into(), &old_handle)
            .expect("running old Event must not recapture replacement identity");
        assert!(matches!(
            NOTIFIER.capture_identity(pid.into()),
            Err(Errno::EMFILE)
        ));

        NOTIFIER.pids.lock().remove(&pid.into());
        reap_stopped_process(child);
        assert!(
            Arc::ptr_eq(&selected, old_handle.event()),
            "replacement registry entry displaced the old Event worker"
        );
    }

    #[test]
    fn registered_exit_stop_survives_procfs_disappearance_until_final_status() {
        let child = spawn_stopped_process(None).expect("spawn exiting notifier tracee");
        let pid = Pid::from_raw(child.as_raw());
        let identity = Arc::new(
            WorkerIdentity::capture_process(pid.into()).expect("capture notifier generation"),
        );
        let handle = EventHandle::with_identity(Arc::clone(&identity));
        handle.event().update(PTRACE_EVENT_EXIT_STOP);
        assert!(handle.event().try_start_worker());
        NOTIFIER.pids.lock().insert(
            pid.into(),
            NotifierEntry {
                handle: handle.clone(),
                identity,
            },
        );

        reap_stopped_process(child);
        NOTIFIER
            .event(pid.into(), &handle)
            .expect("reuse exact registered notifier generation");
        let terminal = Signal::SIGKILL as i32;
        handle.event().update(terminal);
        let waker = Waker::from(Arc::new(WakeCounter::default()));
        let observed = handle.event().poll_status(&waker);
        NOTIFIER.pids.lock().remove(&pid.into());

        assert_eq!(
            observed,
            Poll::Ready(Ok(terminal)),
            "procfs disappearance replaced the old worker's final status with ECHILD"
        );
    }

    fn spawn_traced_process(requested_pid: Option<i32>) -> Option<(Pid, Stopped)> {
        let child = if let Some(requested_pid) = requested_pid {
            #[repr(C)]
            #[derive(Default)]
            struct CloneArgs {
                flags: u64,
                pidfd: u64,
                child_tid: u64,
                parent_tid: u64,
                exit_signal: u64,
                stack: u64,
                stack_size: u64,
                tls: u64,
                set_tid: u64,
                set_tid_size: u64,
                cgroup: u64,
            }

            let mut set_tid = requested_pid as u64;
            let args = CloneArgs {
                exit_signal: libc::SIGCHLD as u64,
                set_tid: std::ptr::from_mut(&mut set_tid) as u64,
                set_tid_size: 1,
                ..CloneArgs::default()
            };
            let result = unsafe {
                libc::syscall(
                    libc::SYS_clone3,
                    std::ptr::from_ref(&args),
                    mem::size_of::<CloneArgs>(),
                )
            };
            if result == -1 {
                return None;
            }
            if result == 0 {
                crate::traceme_and_stop().expect("TRACEME requested-PID child");
                unsafe { libc::_exit(42) };
            }
            Pid::from_raw(result as i32)
        } else {
            match unsafe { fork() }.expect("fork duplicate-exit tracee") {
                ForkResult::Parent { child } => child,
                ForkResult::Child => {
                    crate::traceme_and_stop().expect("TRACEME duplicate-exit child");
                    unsafe { libc::_exit(42) };
                }
            }
        };

        let (stopped, event) = Running::new(child.into())
            .wait()
            .expect("wait duplicate-exit initial stop")
            .assume_stopped();
        assert_eq!(event, crate::Event::Signal(Signal::SIGSTOP));
        Some((child, stopped))
    }

    async fn duplicate_exit_waiter_rejects_replacement(requested_pid: Option<i32>) -> bool {
        let Some((old_pid, old_stopped)) = spawn_traced_process(requested_pid) else {
            return false;
        };
        old_stopped
            .setoptions(Options::PTRACE_O_TRACEEXIT)
            .expect("enable exit stop for duplicate waiter");

        // Register then cancel one waiter while the tracee is still stopped.
        // A Pending poll must not consume the Event capability.
        let mut cancelled = Box::pin(old_stopped.exit_event());
        let waker = futures::task::noop_waker();
        let mut context = Context::from_waker(&waker);
        assert_eq!(cancelled.as_mut().poll(&mut context), Poll::Pending);
        drop(cancelled);

        let winner = old_stopped.exit_event();
        let duplicate = old_stopped.exit_event();
        let late = old_stopped.exit_event();
        old_stopped
            .resume(None)
            .expect("resume old duplicate-waiter tracee");

        let exit_stopped = winner.await.expect("claim old exit-stop capability");
        assert_eq!(duplicate.await, Err(Error::Errno(Errno::EALREADY)));
        let final_wait = exit_stopped
            .resume(None)
            .expect("resume claimed old exit stop")
            .next_state()
            .await
            .expect("wait old final status");
        assert_eq!(
            final_wait.assume_exited(),
            (old_pid.into(), crate::ExitStatus::Exited(42))
        );

        thread::sleep(Duration::from_millis(20));
        let Some((replacement_pid, replacement)) = spawn_traced_process(requested_pid) else {
            return false;
        };
        if requested_pid.is_some() {
            assert_eq!(
                replacement_pid, old_pid,
                "clone3 did not reuse requested PID"
            );
        }

        assert_eq!(late.await, Err(Error::Errno(Errno::EALREADY)));
        replacement
            .getregs()
            .expect("late old waiter touched the stopped replacement");
        assert_eq!(
            replacement
                .resume(None)
                .expect("resume replacement tracee")
                .wait()
                .expect("wait replacement tracee")
                .assume_exited(),
            (replacement_pid.into(), crate::ExitStatus::Exited(42))
        );
        true
    }

    async fn unclaimed_exit_waiter_expires_before_cleanup_replacement(
        requested_pid: Option<i32>,
    ) -> bool {
        let Some((old_pid, old_stopped)) = spawn_traced_process(requested_pid) else {
            return false;
        };
        old_stopped
            .setoptions(Options::PTRACE_O_TRACEEXIT)
            .expect("enable exit stop for cleanup expiration");
        let terminal = old_stopped.terminal_cleanup();
        let mut late = Box::pin(old_stopped.exit_event());
        let waker = futures::task::noop_waker();
        let mut context = Context::from_waker(&waker);
        assert_eq!(late.as_mut().poll(&mut context), Poll::Pending);
        old_stopped
            .resume(None)
            .expect("resume unclaimed exit-stop tracee");

        let deadline = Instant::now() + Duration::from_secs(1);
        while !terminal.exit_stop_observed() {
            assert!(
                Instant::now() < deadline,
                "unclaimed cleanup tracee did not reach exit stop"
            );
            thread::yield_now();
        }
        terminal
            .revoke_unclaimed_exit_stop()
            .expect("cleanup failed to expire unclaimed exit stop");
        nix::sys::ptrace::cont(old_pid, None).expect("raw cleanup resume old exit stop");
        assert!(
            terminal.wait(Duration::from_secs(1)),
            "cleanup notifier did not publish old final status"
        );

        thread::sleep(Duration::from_millis(20));
        let Some((replacement_pid, replacement)) = spawn_traced_process(requested_pid) else {
            return false;
        };
        if requested_pid.is_some() {
            assert_eq!(
                replacement_pid, old_pid,
                "clone3 did not reuse cleanup test PID"
            );
        }

        assert_eq!(late.await, Err(Error::Errno(Errno::EALREADY)));
        replacement
            .getregs()
            .expect("late unclaimed waiter touched the stopped replacement");
        assert_eq!(
            replacement
                .resume(None)
                .expect("resume cleanup replacement tracee")
                .wait()
                .expect("wait cleanup replacement tracee")
                .assume_exited(),
            (replacement_pid.into(), crate::ExitStatus::Exited(42))
        );
        true
    }

    #[tokio::test(flavor = "current_thread")]
    async fn exit_waiters_never_target_reused_pid_after_claim_or_cleanup() {
        const INNER: &str = "SAFEPTRACE_EXIT_REUSE_INNER";
        if env::var_os(INNER).is_some() {
            if duplicate_exit_waiter_rejects_replacement(Some(100)).await
                && unclaimed_exit_waiter_expires_before_cleanup_replacement(Some(100)).await
            {
                println!("ACTUAL_EXIT_PID_REUSE_EXERCISED");
            } else {
                println!("ACTUAL_EXIT_PID_REUSE_UNAVAILABLE");
            }
            return;
        }

        let inner = "notifier::test::exit_waiters_never_target_reused_pid_after_claim_or_cleanup";
        let actual_reuse = env::current_exe().ok().and_then(|test_binary| {
            Command::new("unshare")
                .args([
                    "--user",
                    "--map-root-user",
                    "--pid",
                    "--fork",
                    "--mount-proc",
                ])
                .arg(test_binary)
                .args(["--exact", inner, "--nocapture"])
                .env(INNER, "1")
                .output()
                .ok()
        });
        if let Some(output) = actual_reuse.as_ref() {
            let stdout = String::from_utf8_lossy(&output.stdout);
            let stderr = String::from_utf8_lossy(&output.stderr);
            if output.status.success() && stdout.contains("ACTUAL_EXIT_PID_REUSE_EXERCISED") {
                return;
            }
            let unavailable = stdout.contains("ACTUAL_EXIT_PID_REUSE_UNAVAILABLE")
                || stderr.contains("Operation not permitted")
                || stderr.contains("unshare failed");
            assert!(
                output.status.success() || unavailable,
                "actual PID-reuse regression failed:\nstdout:\n{stdout}\nstderr:\n{stderr}"
            );
        }

        // Restricted runners may deny user namespaces or clone3(set_tid).
        // Still use two real kernel-created generations and prove the late
        // duplicate cannot mint a second stopped capability.
        assert!(duplicate_exit_waiter_rejects_replacement(None).await);
        assert!(unclaimed_exit_waiter_expires_before_cleanup_replacement(None).await);
    }

    fn spawn_stopped_process(requested_pid: Option<i32>) -> Option<Pid> {
        let child = if let Some(requested_pid) = requested_pid {
            #[repr(C)]
            #[derive(Default)]
            struct CloneArgs {
                flags: u64,
                pidfd: u64,
                child_tid: u64,
                parent_tid: u64,
                exit_signal: u64,
                stack: u64,
                stack_size: u64,
                tls: u64,
                set_tid: u64,
                set_tid_size: u64,
                cgroup: u64,
            }

            let mut set_tid = requested_pid as u64;
            let args = CloneArgs {
                exit_signal: libc::SIGCHLD as u64,
                set_tid: std::ptr::from_mut(&mut set_tid) as u64,
                set_tid_size: 1,
                ..CloneArgs::default()
            };
            let result = unsafe {
                libc::syscall(
                    libc::SYS_clone3,
                    std::ptr::from_ref(&args),
                    mem::size_of::<CloneArgs>(),
                )
            };
            if result == -1 {
                return None;
            }
            if result == 0 {
                let _ = nix::sys::signal::raise(Signal::SIGSTOP);
                unsafe { libc::_exit(0) };
            }
            Pid::from_raw(result as i32)
        } else {
            match unsafe { fork() }.expect("fork replacement-generation tracee") {
                ForkResult::Parent { child } => child,
                ForkResult::Child => {
                    let _ = nix::sys::signal::raise(Signal::SIGSTOP);
                    unsafe { libc::_exit(0) };
                }
            }
        };

        let mut status = 0;
        let waited = unsafe { libc::waitpid(child.as_raw(), &mut status, libc::WUNTRACED) };
        assert_eq!(waited, child.as_raw());
        assert!(libc::WIFSTOPPED(status));
        Some(child)
    }

    fn reap_stopped_process(pid: Pid) {
        assert_eq!(unsafe { libc::kill(pid.as_raw(), libc::SIGKILL) }, 0);
        let mut status = 0;
        let waited = unsafe { libc::waitpid(pid.as_raw(), &mut status, 0) };
        assert_eq!(waited, pid.as_raw());
        assert!(libc::WIFSIGNALED(status));
    }

    fn assert_live_replacement_rejected(first_pid: Option<i32>) -> bool {
        let Some(old_pid) = spawn_stopped_process(first_pid) else {
            return false;
        };
        let old_identity = WorkerIdentity::capture_process(old_pid.into())
            .expect("capture old notifier worker generation");
        assert!(old_identity.is_same_process_generation());
        reap_stopped_process(old_pid);

        // starttime is measured in clock ticks. Keep the two real generations
        // distinct even on fast machines where procfs could otherwise report
        // the same tick.
        thread::sleep(Duration::from_millis(20));

        let Some(new_pid) = spawn_stopped_process(first_pid) else {
            return false;
        };
        let new_identity = WorkerIdentity::capture_process(new_pid.into())
            .expect("capture replacement notifier worker generation");
        assert!(new_identity.is_same_process_generation());
        assert!(
            !old_identity.is_same_process_generation(),
            "an ECHILD worker accepted a live replacement proc generation"
        );
        // The production ECHILD decision is stricter still: it also requires
        // the bound TracerPid to remain one of this process's live threads.
        assert!(!old_identity.is_active_tracee());
        reap_stopped_process(new_pid);
        true
    }

    #[test]
    fn worker_echild_rejects_live_replacement_generation() {
        const INNER: &str = "SAFEPTRACE_ACTUAL_REUSE_INNER";
        if env::var_os(INNER).is_some() {
            if assert_live_replacement_rejected(Some(100)) {
                println!("ACTUAL_PID_REUSE_EXERCISED");
            }
            return;
        }

        // Reinvoke this exact test in a fresh user/PID namespace.
        // `clone3(set_tid)` there deterministically reuses PID 100 for two
        // different, real procfs generations.
        let inner = "notifier::test::worker_echild_rejects_live_replacement_generation";
        let actual_reuse = env::current_exe().ok().and_then(|test_binary| {
            Command::new("unshare")
                .args([
                    "--user",
                    "--map-root-user",
                    "--pid",
                    "--fork",
                    "--mount-proc",
                ])
                .arg(test_binary)
                .args(["--exact", inner, "--nocapture"])
                .env(INNER, "1")
                .output()
                .ok()
        });

        if actual_reuse.as_ref().is_some_and(|output| {
            output.status.success()
                && String::from_utf8_lossy(&output.stdout).contains("ACTUAL_PID_REUSE_EXERCISED")
        }) {
            return;
        }

        // Restricted CI runners may prohibit user namespaces or clone3
        // set_tid. Still exercise the exact generation predicate with two live,
        // kernel-created proc generations; the old O_PATH fd/starttime must
        // reject the replacement even though its numeric PID cannot be forced.
        assert!(assert_live_replacement_rejected(None));
    }

    #[test]
    fn registry_rejects_terminal_event_after_actual_pid_reuse() {
        const INNER: &str = "SAFEPTRACE_REGISTRY_REUSE_INNER";
        if env::var_os(INNER).is_some() {
            let Some(old_pid) = spawn_stopped_process(Some(100)) else {
                return;
            };
            let old_identity = Arc::new(
                WorkerIdentity::capture_process(old_pid.into())
                    .expect("capture old registry generation"),
            );
            let old_handle = EventHandle::with_identity(Arc::clone(&old_identity));
            NOTIFIER.pids.lock().insert(
                old_pid.into(),
                NotifierEntry {
                    handle: old_handle.clone(),
                    identity: old_identity,
                },
            );
            assert!(old_handle.event().try_begin_unstarted_completion());
            old_handle.event().mark_echild();
            old_handle.event().mark_worker_done();
            reap_stopped_process(old_pid);
            thread::sleep(Duration::from_millis(20));

            let Some(new_pid) = spawn_stopped_process(Some(100)) else {
                NOTIFIER.pids.lock().remove(&old_pid.into());
                return;
            };
            let selected = EventHandle::current_or_new(new_pid.into())
                .expect("select replacement registry generation");
            assert_ne!(
                selected, old_handle,
                "registry rebound a reused numeric PID to the old terminal Event"
            );
            NOTIFIER.pids.lock().remove(&new_pid.into());
            reap_stopped_process(new_pid);
            println!("ACTUAL_REGISTRY_PID_REUSE_EXERCISED");
            return;
        }

        let inner = "notifier::test::registry_rejects_terminal_event_after_actual_pid_reuse";
        let actual_reuse = env::current_exe().ok().and_then(|test_binary| {
            Command::new("unshare")
                .args([
                    "--user",
                    "--map-root-user",
                    "--pid",
                    "--fork",
                    "--mount-proc",
                ])
                .arg(test_binary)
                .args(["--exact", inner, "--nocapture"])
                .env(INNER, "1")
                .output()
                .ok()
        });
        if actual_reuse.as_ref().is_some_and(|output| {
            output.status.success()
                && String::from_utf8_lossy(&output.stdout)
                    .contains("ACTUAL_REGISTRY_PID_REUSE_EXERCISED")
        }) {
            return;
        }

        // Hosted CI may prohibit user namespaces. Project two distinct real
        // kernel generations onto the same numeric registry key while keeping
        // the old O_PATH fd, starttime, TGID, and inode. This exercises the
        // production replacement path without accepting a numeric-only match.
        assert_projected_registry_reuse_rejected();
    }

    fn assert_projected_registry_reuse_rejected() {
        let old_pid = spawn_stopped_process(None).expect("spawn old projected generation");
        let mut old_identity = WorkerIdentity::capture_process(old_pid.into())
            .expect("capture old projected generation");
        reap_stopped_process(old_pid);
        thread::sleep(Duration::from_millis(20));

        let new_pid = spawn_stopped_process(None).expect("spawn new projected generation");
        old_identity.pid = new_pid.into();
        let old_identity = Arc::new(old_identity);
        let old_handle = EventHandle::with_identity(Arc::clone(&old_identity));
        assert!(old_handle.event().try_begin_unstarted_completion());
        old_handle.event().mark_echild();
        old_handle.event().mark_worker_done();
        NOTIFIER.pids.lock().insert(
            new_pid.into(),
            NotifierEntry {
                handle: old_handle.clone(),
                identity: old_identity,
            },
        );

        let selected = EventHandle::current_or_new(new_pid.into())
            .expect("select projected replacement registry generation");
        assert_ne!(
            selected, old_handle,
            "registry selected a terminal Event using only the projected numeric PID"
        );
        NOTIFIER.pids.lock().remove(&new_pid.into());
        reap_stopped_process(new_pid);
    }

    #[test]
    fn registry_rejects_projected_pid_reuse_without_user_namespaces() {
        assert_projected_registry_reuse_rejected();
    }
}
