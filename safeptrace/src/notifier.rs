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
use std::sync::atomic::AtomicI32;
use std::sync::atomic::AtomicPtr;
use std::sync::atomic::Ordering;
use std::task::Context;
use std::task::Poll;
use std::task::RawWakerVTable;
use std::task::Waker;
use std::thread;
use std::thread::JoinHandle;
use std::time::Duration;
use std::time::Instant;

use nix::sys::wait::WaitPidFlag;
use parking_lot::Condvar;
use parking_lot::Mutex;

use super::Errno;
use super::Error;
use super::Pid;
use super::Running;
use super::Stopped;
use super::TraceeToken;
use super::Wait;
use super::waitid;

static NOTIFIER: LazyLock<Notifier> = LazyLock::new(Notifier::new);

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

#[derive(Debug)]
struct Event {
    /// Waker for exit events.
    exit_waker: WakerSlot,

    /// Waker for regular status events.
    status_waker: WakerSlot,

    /// Ordered regular statuses plus a retained terminal publication.
    status: Mutex<StatusState>,

    /// Independently retained `PTRACE_EVENT_EXIT` publication. Keeping this
    /// separate prevents a following final wait status from stealing the exit
    /// event from a held [`ExitFuture`].
    exit_status: AtomicI32,
}

#[derive(Debug)]
struct StatusState {
    pending: VecDeque<i32>,
    terminal: i32,
}

impl Event {
    pub fn new() -> Self {
        Self {
            exit_waker: WakerSlot::default(),
            status_waker: WakerSlot::default(),
            status: Mutex::new(StatusState {
                pending: VecDeque::new(),
                terminal: INVALID_STATUS,
            }),
            exit_status: AtomicI32::new(EXIT_PENDING),
        }
    }

    /// Replaces the status and notifies the notifier of the change. Returns the
    /// old status if there was one.
    pub fn update(&self, status: i32) -> Option<i32> {
        if status == PTRACE_EVENT_EXIT_STOP {
            let previous = self.exit_status.compare_exchange(
                EXIT_PENDING,
                EXIT_STOPPED,
                Ordering::SeqCst,
                Ordering::SeqCst,
            );
            debug_assert!(matches!(previous, Ok(_) | Err(EXIT_STOPPED)));
            self.exit_waker.wake();
            return None;
        }

        let terminal = libc::WIFEXITED(status) || libc::WIFSIGNALED(status);
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
        if terminal {
            let _ = self.exit_status.compare_exchange(
                EXIT_PENDING,
                EXIT_ECHILD,
                Ordering::SeqCst,
                Ordering::SeqCst,
            );
            // A terminal publication resolves both waiter classes. ExitFuture
            // observes either the retained exit stop or typed ECHILD, while
            // WaitFuture retains the exact final status.
            self.status_waker.wake();
            self.exit_waker.wake();
        } else {
            self.status_waker.wake();
        }

        (previous != INVALID_STATUS).then_some(previous)
    }

    /// Publishes a terminal `ECHILD` observation to every kind of waiter.
    fn mark_echild(&self) {
        let mut state = self.status.lock();
        if state.terminal == INVALID_STATUS {
            state.terminal = ECHILD_STATUS;
        }
        drop(state);
        let _ = self.exit_status.compare_exchange(
            EXIT_PENDING,
            EXIT_ECHILD,
            Ordering::SeqCst,
            Ordering::SeqCst,
        );
        self.status_waker.wake();
        self.exit_waker.wake();
    }

    fn is_terminal(&self) -> bool {
        self.status.lock().terminal != INVALID_STATUS
    }

    /// Polls the event to check if there is a new status ready to be consumed.
    pub fn poll_status(&self, waker: &Waker) -> Poll<Result<i32, Errno>> {
        // Register the waker *before* checking the status to avoid a race condition.
        self.status_waker.register(waker);

        let mut state = self.status.lock();
        if let Some(status) = state.pending.pop_front() {
            return Poll::Ready(Ok(status));
        }
        match state.terminal {
            INVALID_STATUS => Poll::Pending,
            ECHILD_STATUS => Poll::Ready(Err(Errno::ECHILD)),
            status => {
                // Final status is immutable so old state generations retain
                // the actual exit code or terminating signal after removal.
                Poll::Ready(Ok(status))
            }
        }
    }

    fn take_pending_status(&self) -> Option<i32> {
        self.status.lock().pending.pop_front()
    }

    /// Polls the event to check if there is a new status ready to be consumed.
    pub fn poll_exit(&self, waker: &Waker) -> Poll<Result<(), Errno>> {
        // Register the waker *before* checking the status to avoid a race condition.
        self.exit_waker.register(waker);

        match self.exit_status.load(Ordering::SeqCst) {
            EXIT_STOPPED => Poll::Ready(Ok(())),
            EXIT_ECHILD => Poll::Ready(Err(Errno::ECHILD)),
            EXIT_PENDING => Poll::Pending,
            state => unreachable!("invalid exit publication state {state}"),
        }
    }
}

/// One immutable notifier generation carried by typed tracee states.
#[derive(Clone, Debug)]
pub(super) struct EventHandle(Arc<Event>);

impl EventHandle {
    pub(super) fn new() -> Self {
        Self(Arc::new(Event::new()))
    }

    pub(super) fn current_or_new(pid: Pid) -> Self {
        NOTIFIER
            .pids
            .lock()
            .get(&pid)
            .cloned()
            .map(Self)
            .unwrap_or_else(Self::new)
    }
}

impl PartialEq for EventHandle {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

impl Eq for EventHandle {}

impl Hash for EventHandle {
    fn hash<H: Hasher>(&self, state: &mut H) {
        Arc::as_ptr(&self.0).hash(state);
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
}

fn spawn_worker(pid: Pid, event: Arc<Event>, identity: WorkerIdentity) -> JoinHandle<()> {
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
fn worker_thread(pid: Pid, event: Arc<Event>, identity: WorkerIdentity) {
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

struct Notifier {
    /// Mapping of pids to wakers.
    pids: Mutex<HashMap<Pid, Arc<Event>>>,
    /// Notifies synchronous cancellation cleanup after the worker unregisters.
    removed: Condvar,
}

impl Notifier {
    /// Creates the notifier.
    pub fn new() -> Self {
        let pids = Mutex::new(HashMap::new());
        Notifier {
            pids,
            removed: Condvar::new(),
        }
    }

    /// Registers the exact event generation carried by a typed state.
    fn event(&self, pid: Pid, handle: &EventHandle) -> Arc<Event> {
        let requested = &handle.0;
        if requested.is_terminal() {
            return Arc::clone(requested);
        }

        let mut pids = self.pids.lock();
        match pids.entry(pid) {
            Entry::Occupied(occupied) if Arc::ptr_eq(occupied.get(), requested) => {
                Arc::clone(occupied.get())
            }
            Entry::Occupied(_) => {
                // Another generation owns this numeric PID. Resolve the stale
                // state against its own event instead of binding it to the
                // replacement tracee.
                requested.mark_echild();
                Arc::clone(requested)
            }
            Entry::Vacant(vacant) => {
                // Recheck after taking the registry lock: the old worker can
                // publish terminal status and remove itself between the first
                // check and this vacant entry.
                if requested.is_terminal() {
                    return Arc::clone(requested);
                }
                let Ok(identity) = WorkerIdentity::capture(pid) else {
                    // Never start a numeric-PID worker without an immutable
                    // procfs generation. A late or replaced typed state is
                    // terminal for this exact Event generation.
                    requested.mark_echild();
                    return Arc::clone(requested);
                };
                vacant.insert(Arc::clone(requested));
                spawn_worker(pid, Arc::clone(requested), identity);
                Arc::clone(requested)
            }
        }
    }

    /// Removes a completed PID without disturbing a reused PID's event.
    fn remove(&self, pid: Pid, event: &Arc<Event>) {
        let mut pids = self.pids.lock();
        if pids
            .get(&pid)
            .is_some_and(|current| Arc::ptr_eq(current, event))
        {
            pids.remove(&pid);
            self.removed.notify_all();
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
        NOTIFIER.event(pid, &event);
        Self { pid, event }
    }

    /// Waits up to `timeout` for the notifier worker to unregister this PID.
    ///
    /// This does not call `waitpid`: after notifier registration, the worker
    /// thread remains the sole owner of wait statuses for the PID.
    pub fn wait(&self, timeout: Duration) -> bool {
        let deadline = Instant::now()
            .checked_add(timeout)
            .unwrap_or_else(Instant::now);
        let mut pids = NOTIFIER.pids.lock();
        loop {
            let registered = pids
                .get(&self.pid)
                .is_some_and(|current| Arc::ptr_eq(current, &self.event.0));
            if !registered {
                return true;
            }

            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return false;
            }
            NOTIFIER.removed.wait_for(&mut pids, remaining);
        }
    }

    /// Takes the oldest queued nonterminal state after cancellation.
    ///
    /// This is a cancellation-only escape hatch for the ptracer thread that
    /// owns this exact event generation. A queued ptrace child event must be
    /// decoded while its returned stopped root is held so
    /// `PTRACE_GETEVENTMSG` binds that kernel-reported child before resume.
    /// The retained final status is never consumed by this method.
    pub fn take_pending_for_cleanup(&self) -> Option<Result<Wait, Error>> {
        self.event.0.take_pending_status().map(|status| {
            Wait::from_raw_with_token(
                self.pid,
                status,
                TraceeToken::from_event(self.event.clone()),
            )
        })
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
        let event = NOTIFIER.event(pid, this.running.token().event());
        let status = match futures::ready!(event.poll_status(cx.waker())) {
            Ok(status) => status,
            Err(errno) => return Poll::Ready(Err(errno.into())),
        };

        Poll::Ready(Wait::from_raw_with_token(
            pid,
            status,
            this.running.token().clone(),
        ))
    }
}

/// A future representing PTRACE_EVENT_EXIT. The future resolves when the process
/// receives a PTRACE_EVENT_EXIT. A process can receive this event at any time,
/// even when in another ptrace stop state.
///
/// The next state after this should be the final exit status.
// AUTONOMOUS-BOT-IMPLEMENTED
// TODO-HUMAN-REVIEW(PR-270): Trigger 2: review the public typed-error
// ExitFuture contract and retained exit-stop generation semantics.
pub struct ExitFuture {
    pid: Pid,
    event: EventHandle,
}

impl ExitFuture {
    pub(super) fn new(pid: Pid, token: &TraceeToken) -> Self {
        Self {
            pid,
            event: token.event().clone(),
        }
    }
}

impl Future for ExitFuture {
    type Output = Result<Stopped, Error>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context) -> Poll<Self::Output> {
        let this = self.get_mut();
        let event = NOTIFIER.event(this.pid, &this.event);
        match futures::ready!(event.poll_exit(cx.waker())) {
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
    fn held_exit_waiter_survives_terminal_final_update() {
        let counter = Arc::new(WakeCounter::default());
        let waker = Waker::from(Arc::clone(&counter));
        let event = Event::new();

        assert_eq!(event.poll_exit(&waker), Poll::Pending);
        event.update(PTRACE_EVENT_EXIT_STOP);
        event.update(42 << 8);

        assert_eq!(event.poll_exit(&waker), Poll::Ready(Ok(())));
        assert_eq!(event.poll_status(&waker), Poll::Ready(Ok(42 << 8)));
    }

    #[test]
    fn terminal_status_remains_available_to_late_waiters() {
        let waker = Waker::from(Arc::new(WakeCounter::default()));

        for terminal in [42 << 8, Signal::SIGILL as i32] {
            let event = Event::new();
            event.update(PTRACE_EVENT_EXIT_STOP);
            event.update(terminal);

            assert_eq!(event.poll_status(&waker), Poll::Ready(Ok(terminal)));
            assert_eq!(event.poll_status(&waker), Poll::Ready(Ok(terminal)));
            assert_eq!(event.poll_exit(&waker), Poll::Ready(Ok(())));
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
    fn terminal_cleanup_removes_stale_pid_registration() {
        let pid = Pid::from_raw(i32::MAX - 17);
        let running = Running::new(pid.into());
        let cleanup = running.terminal_cleanup();
        let old_event = cleanup.event.clone();

        assert!(cleanup.wait(Duration::from_secs(1)));
        let replacement_handle = EventHandle::new();
        let replacement = NOTIFIER.event(pid.into(), &replacement_handle);
        assert!(
            !Arc::ptr_eq(&old_event.0, &replacement),
            "terminal cleanup retained a stale PID registry entry"
        );
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
}
