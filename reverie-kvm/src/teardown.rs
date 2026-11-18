/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! Deferred physical destruction of a backend's VM.
//!
//! Closing a VM fd destroys the VM in the kernel (`kvm_destroy_vm`), whose
//! MMU teardown waits for an RCU grace period. That wait is tens of
//! milliseconds on a busy many-CPU host and has nothing to do with the
//! guest: every guest-visible effect of a process's or thread's exit is
//! already published before its backend drops. A backend with a
//! [`VmTeardownSink`] installed therefore hands duplicates of its vCPU and
//! VM fds to the sink at drop, after its logical teardown (worker
//! cancellation and joins) has run on the dropping thread. The dropping
//! thread's own closes then only drop file references; the sink's closes
//! are the last ones and run the kernel destroy wherever the sink chooses.
//!
//! If duplicating a descriptor fails, or the sink panics, the backend's
//! fields drop as they would without a sink and destroy the VM on the
//! dropping thread. Deferral can degrade to synchronous destruction; it
//! cannot leak a VM, because some close is always the last one.
//!
//! The thread reaper here is also safe in a host-fork copy of a run (a
//! process made by `fork()` from a paused template): `fork()` copies
//! only the calling thread, so the workers do not exist in the copy,
//! but their queue does. The reaper records the process its workers
//! were spawned in and, on first use in a different process, spawns a
//! fresh pool that keeps draining that queue — each process's closes
//! drop only its own descriptor references, and the kernel destroy
//! still runs at whichever close is globally last. If no worker can
//! spawn in the copy, descriptors close inline instead. Without this,
//! a copy adopting into the copied queue would wait forever for
//! workers that are not there.

use std::collections::VecDeque;
use std::os::fd::FromRawFd;
use std::os::fd::OwnedFd;
use std::os::fd::RawFd;
use std::sync::Arc;
use std::sync::Condvar;
use std::sync::Mutex;
use std::sync::MutexGuard;

use crate::Error;
use crate::Result;
use crate::error::ErrorExt;

/// Where a dropped backend's VM fds go to be closed last.
///
/// The backend calls this once per VM, from its drop, after logical
/// teardown. The descriptors are close-on-exec duplicates of the backend's
/// vCPU and VM fds. The sink must:
///
/// - close `vcpu` before `vm` (the backend's own field drop order);
/// - eventually close both, whatever else fails — dropping them unclosed
///   is impossible for [`OwnedFd`], so holding them forever is the only
///   way to break this, and a VM held past its run is a leak;
/// - not panic (the backend catches a panic and falls back to destroying
///   the VM on the dropping thread, but nothing is reported to the sink).
///
/// It may block, for backpressure: a bounded sink that blocks when full
/// degenerates to synchronous teardown under overload rather than holding
/// unbounded guest memory in undestroyed VMs.
pub trait VmTeardownSink: Send + Sync {
    fn adopt_vm(&self, vcpu: OwnedFd, vm: OwnedFd);
}

/// A close-on-exec duplicate of `fd`, for handing to a [`VmTeardownSink`].
pub(crate) fn dup_cloexec(fd: RawFd) -> std::io::Result<OwnedFd> {
    // SAFETY: `fd` is a live descriptor owned by the caller; a successful
    // F_DUPFD_CLOEXEC returns a new descriptor this call uniquely owns.
    let dup = unsafe { libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, 0) };
    if dup < 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: as above, `dup` is a fresh descriptor owned by this call.
    Ok(unsafe { OwnedFd::from_raw_fd(dup) })
}

/// Concurrent closes overlap their kernel RCU grace-period waits, so a
/// small pool destroys a fork-heavy run's VMs much faster than one thread
/// could, without unbounded thread creation.
const TEARDOWN_WORKERS: usize = 4;

/// Bundles queued behind the workers before [`VmTeardownSink::adopt_vm`]
/// blocks. Each bundle keeps one guest memory's pinned pages alive, so the
/// queue is bounded; the workers' in-flight bundles are additional.
const MAX_QUEUED_BUNDLES: usize = 32;

struct Bundle {
    vcpu: OwnedFd,
    vm: OwnedFd,
}

#[derive(Default)]
struct State {
    queue: VecDeque<Bundle>,
    adopted: u64,
    destroyed: u64,
    handles: usize,
    /// Process the live workers were spawned in (0 before any spawn).
    /// A host-fork copy has a different pid and none of those workers;
    /// first use there spawns a fresh pool (see the module docs).
    worker_pid: libc::pid_t,
    /// No worker could spawn in `worker_pid`; closes run inline there.
    workers_failed: bool,
}

fn current_pid() -> libc::pid_t {
    // SAFETY: getpid takes no arguments and touches no memory.
    unsafe { libc::getpid() }
}

#[derive(Default)]
struct Shared {
    state: Mutex<State>,
    work: Condvar,
    space: Condvar,
    idle: Condvar,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn wait<'a>(&self, condvar: &Condvar, state: MutexGuard<'a, State>) -> MutexGuard<'a, State> {
        condvar
            .wait(state)
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

fn teardown_worker(shared: Arc<Shared>) {
    loop {
        let bundle = {
            let mut state = shared.lock();
            loop {
                if let Some(bundle) = state.queue.pop_front() {
                    shared.space.notify_all();
                    break bundle;
                }
                // No handle remains, so no bundle can still arrive.
                if state.handles == 0 {
                    return;
                }
                state = shared.wait(&shared.work, state);
            }
        };
        // Field order in the backend: the vCPU fd closes before the VM fd.
        // Dropping an OwnedFd closes it; the VM close runs the kernel
        // destroy this thread exists to absorb.
        drop(bundle.vcpu);
        drop(bundle.vm);
        let mut state = shared.lock();
        state.destroyed = state.destroyed.saturating_add(1);
        shared.idle.notify_all();
    }
}

/// Spawns up to [`TEARDOWN_WORKERS`] workers for `shared`, detaching
/// them: a worker stops on its own once no handle remains and the queue
/// is empty, so nothing needs their join handles. Returns how many
/// spawned and the first spawn error, if any.
fn spawn_workers(shared: &Arc<Shared>) -> (usize, Option<std::io::Error>) {
    let mut spawned = 0;
    let mut first_error = None;
    for _ in 0..TEARDOWN_WORKERS {
        match crate::failure::spawn_owned(
            crate::supervisor_thread::builder().name("reverie-kvm-vm-teardown".into()),
            shared.clone(),
            teardown_worker,
        ) {
            Ok(_handle) => spawned += 1,
            Err((error, _)) => {
                if first_error.is_none() {
                    first_error = Some(error);
                }
            }
        }
    }
    (spawned, first_error)
}

/// A [`VmTeardownSink`] that closes adopted VM fds on a small pool of
/// dedicated threads (`reverie-kvm-vm-teardown`).
///
/// This suits a process that keeps running after a run ends. A process
/// that is about to exit gains less: process exit closes any descriptor
/// the pool has not reached yet, in the exiting process. Such an embedder
/// can implement [`VmTeardownSink`] itself and hand the descriptors to a
/// longer-lived owner.
///
/// A host-fork copy of a process holding a reaper may keep using it:
/// the first adoption or wait in the copy spawns fresh workers there
/// (the template's workers were not copied), and any bundles the queue
/// held at the fork are drained by whichever process reaches them. If
/// no worker can spawn in the copy, its adoptions close inline.
pub struct VmTeardownReaper {
    shared: Arc<Shared>,
}

impl VmTeardownReaper {
    /// Spawns the teardown worker threads.
    pub fn new() -> Result<Self> {
        let shared = Arc::new(Shared::default());
        {
            let mut state = shared.lock();
            state.handles = 1;
            state.worker_pid = current_pid();
        }
        let (spawned, error) = spawn_workers(&shared);
        if let Some(error) = error {
            // Construction is all-or-nothing. No handle escapes, so any
            // workers that did spawn stop once they see no handle and an
            // empty queue.
            {
                let mut state = shared.lock();
                state.handles = 0;
                state.workers_failed = spawned == 0;
            }
            shared.work.notify_all();
            return Err(Error::from(error).cleanup("VM teardown worker spawn"));
        }
        Ok(Self { shared })
    }

    /// Whether workers are running in this process, spawning them first
    /// if this is a host-fork copy whose template's workers were not
    /// copied. A partial pool still counts: one worker drains the queue.
    fn ensure_workers(&self) -> bool {
        let pid = current_pid();
        let mut state = self.shared.lock();
        if state.worker_pid == pid {
            return !state.workers_failed;
        }
        // First use in a forked copy (or, if a reaper was somehow made
        // without `new`, in any process): the recorded workers belong
        // to the other process. The queue and counts carry over as-is.
        // The state lock is held across the spawn so concurrent first
        // uses in the copy serialize on this one attempt; the new
        // workers simply block on the lock until it is released.
        state.worker_pid = pid;
        state.workers_failed = false;
        let (spawned, _error) = spawn_workers(&self.shared);
        if spawned == 0 {
            state.workers_failed = true;
            return false;
        }
        true
    }

    /// Blocks until every bundle adopted so far has been destroyed.
    /// Bundles adopted concurrently with the wait may or may not be
    /// included; callers that need a quiescent point stop adopting first
    /// (a finished run has no backend left to drop).
    ///
    /// In a process where no worker can run, this drains the queue on
    /// the calling thread instead of waiting for workers that are not
    /// there.
    pub fn wait_idle(&self) {
        if !self.ensure_workers() {
            loop {
                let bundle = {
                    let mut state = self.shared.lock();
                    match state.queue.pop_front() {
                        Some(bundle) => bundle,
                        None => {
                            assert_eq!(
                                state.adopted, state.destroyed,
                                "VM teardown queue empty with undestroyed bundles"
                            );
                            return;
                        }
                    }
                };
                drop(bundle.vcpu);
                drop(bundle.vm);
                let mut state = self.shared.lock();
                state.destroyed = state.destroyed.saturating_add(1);
            }
        }
        let mut state = self.shared.lock();
        while !(state.queue.is_empty() && state.adopted == state.destroyed) {
            state = self.shared.wait(&self.shared.idle, state);
        }
    }

    /// Bundles adopted and destroyed so far. Equal counts with an empty
    /// queue (as [`Self::wait_idle`] establishes) prove no adopted VM is
    /// still alive.
    pub fn counts(&self) -> (u64, u64) {
        let state = self.shared.lock();
        (state.adopted, state.destroyed)
    }
}

impl Clone for VmTeardownReaper {
    fn clone(&self) -> Self {
        let mut state = self.shared.lock();
        state.handles = state.handles.saturating_add(1);
        drop(state);
        Self {
            shared: self.shared.clone(),
        }
    }
}

impl Drop for VmTeardownReaper {
    fn drop(&mut self) {
        let mut state = self.shared.lock();
        state.handles = state.handles.saturating_sub(1);
        self.shared.work.notify_all();
    }
}

impl VmTeardownSink for VmTeardownReaper {
    fn adopt_vm(&self, vcpu: OwnedFd, vm: OwnedFd) {
        if !self.ensure_workers() {
            // No worker can run in this process (spawn refused, e.g. in
            // a host-fork copy): close inline, in the sink's order, and
            // count the bundle so `counts` and `wait_idle` stay truthful.
            drop(vcpu);
            drop(vm);
            let mut state = self.shared.lock();
            state.adopted = state.adopted.saturating_add(1);
            state.destroyed = state.destroyed.saturating_add(1);
            self.shared.idle.notify_all();
            return;
        }
        // Kept outside the queue until space is certain, so the wait for
        // space cannot lose the bundle.
        let mut bundle = Some(Bundle { vcpu, vm });
        let mut state = self.shared.lock();
        while state.queue.len() >= MAX_QUEUED_BUNDLES {
            state = self.shared.wait(&self.shared.space, state);
        }
        state
            .queue
            .push_back(bundle.take().expect("bundle taken before adoption"));
        state.adopted = state.adopted.saturating_add(1);
        self.shared.work.notify_one();
    }
}

#[cfg(test)]
#[allow(clippy::disallowed_methods, reason = "tests may make threads freely")]
mod tests {
    use std::os::fd::AsRawFd;

    use super::*;

    /// A pipe whose read end reports EOF once both adopted write ends —
    /// standing in for a vCPU fd and a VM fd — have been closed.
    fn pipe() -> (OwnedFd, OwnedFd) {
        let mut fds = [0; 2];
        // SAFETY: `fds` is a valid two-element array for pipe2 to fill.
        assert_eq!(unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) }, 0);
        // SAFETY: pipe2 just created both descriptors for this call.
        unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) }
    }

    fn is_eof(fd: &OwnedFd) -> bool {
        let mut poll = libc::pollfd {
            fd: fd.as_raw_fd(),
            events: libc::POLLHUP,
            revents: 0,
        };
        // SAFETY: `poll` is one valid pollfd for the duration of the call.
        let ready = unsafe { libc::poll(&mut poll, 1, 0) };
        ready == 1 && poll.revents & libc::POLLHUP != 0
    }

    #[test]
    fn adopted_descriptors_are_closed_by_wait_idle() {
        let reaper = VmTeardownReaper::new().expect("spawn teardown workers");
        let (read_a, write_a) = pipe();
        let (read_b, write_b) = pipe();
        assert!(!is_eof(&read_a));
        reaper.adopt_vm(write_a, write_b);
        reaper.wait_idle();
        assert_eq!(reaper.counts(), (1, 1));
        assert!(is_eof(&read_a), "vcpu stand-in still open");
        assert!(is_eof(&read_b), "vm stand-in still open");
    }

    #[test]
    fn more_bundles_than_the_queue_bound_all_complete() {
        let reaper = VmTeardownReaper::new().expect("spawn teardown workers");
        let bundles = (MAX_QUEUED_BUNDLES * 2 + TEARDOWN_WORKERS) as u64;
        let mut readers = Vec::new();
        for _ in 0..bundles {
            let (read_a, write_a) = pipe();
            let (read_b, write_b) = pipe();
            readers.push((read_a, read_b));
            reaper.adopt_vm(write_a, write_b);
        }
        reaper.wait_idle();
        assert_eq!(reaper.counts(), (bundles, bundles));
        for (read_a, read_b) in &readers {
            assert!(is_eof(read_a) && is_eof(read_b));
        }
    }

    /// The anon-inode name of one open descriptor, if it is still open.
    fn fd_target(fd: RawFd) -> Option<String> {
        std::fs::read_link(format!("/proc/self/fd/{fd}"))
            .ok()?
            .to_str()
            .map(str::to_owned)
    }

    #[derive(Default)]
    struct RecordingSink {
        bundles: Mutex<Vec<(OwnedFd, OwnedFd)>>,
    }

    impl VmTeardownSink for RecordingSink {
        fn adopt_vm(&self, vcpu: OwnedFd, vm: OwnedFd) {
            self.bundles
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .push((vcpu, vm));
        }
    }

    /// A dropped backend hands the sink live duplicates: the VM stays
    /// open in the sink after the backend is gone, and closing the
    /// sink's copies is what finally removes it. Descriptor numbers are
    /// checked individually so parallel tests creating their own VMs
    /// cannot disturb this one.
    #[test]
    fn backend_drop_defers_vm_close_to_the_sink() {
        let sink = Arc::new(RecordingSink::default());
        {
            let mut backend = crate::KvmBackend::new(16 * 1024 * 1024)
                .expect("the deferred-teardown test requires /dev/kvm");
            backend.set_vm_teardown_sink(Some(sink.clone()));
        }
        let (vcpu_fd, vm_fd) = {
            let bundles = sink
                .bundles
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            assert_eq!(bundles.len(), 1);
            (bundles[0].0.as_raw_fd(), bundles[0].1.as_raw_fd())
        };
        // The backend is gone; both adopted descriptors are still open,
        // so no destroy ran on the dropping thread.
        let vcpu_target = fd_target(vcpu_fd).expect("adopted vcpu fd closed early");
        let vm_target = fd_target(vm_fd).expect("adopted vm fd closed early");
        assert!(vcpu_target.contains("kvm"), "vcpu fd target: {vcpu_target}");
        assert!(vm_target.contains("kvm-vm"), "vm fd target: {vm_target}");
        // Closing the adopted copies is the last close: the VM goes away.
        sink.bundles
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clear();
        assert!(
            fd_target(vm_fd).is_none_or(|target| !target.contains("kvm-vm")),
            "vm fd still open after the sink closed it"
        );
    }

    /// A host-fork copy holds the reaper's queue and counts but none of
    /// its workers (fork copies only the calling thread). Adoption in
    /// the copy must spawn fresh workers there and close the bundle;
    /// the parent's reaper and counts are untouched.
    #[test]
    fn reaper_restarts_workers_in_forked_child() {
        let reaper = VmTeardownReaper::new().expect("spawn teardown workers");
        let (read_a, write_a) = pipe();
        let (read_b, write_b) = pipe();
        reaper.adopt_vm(write_a, write_b);
        reaper.wait_idle();
        assert_eq!(reaper.counts(), (1, 1));
        assert!(is_eof(&read_a) && is_eof(&read_b));

        // Adopted only in the child. The parent closes its copies of the
        // write ends right after the fork, so the child's readers can
        // reach EOF there; the verdict rides on the child's counts,
        // which only move when a worker in the child closed a bundle.
        let (read_c, write_c) = pipe();
        let (read_d, write_d) = pipe();
        let mut write_c = Some(write_c);
        let mut write_d = Some(write_d);
        // SAFETY: the child touches only the reaper and these pipes,
        // then _exits without unwinding into the test harness; alarm
        // bounds a hang (adoption waiting for workers that are not
        // there) as a killed child, which the parent reports.
        let pid = unsafe { libc::fork() };
        assert!(pid >= 0, "fork: {}", std::io::Error::last_os_error());
        if pid == 0 {
            // SAFETY: alarm and _exit take only integer arguments.
            unsafe { libc::alarm(15) };
            let write_c = write_c.take().expect("child write end");
            let write_d = write_d.take().expect("child write end");
            let verdict = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                reaper.adopt_vm(write_c, write_d);
                reaper.wait_idle();
                if reaper.counts() != (2, 2) {
                    return false;
                }
                // EOF also needs the parent's copies closed, which it
                // does right after the fork; poll briefly for that.
                for _ in 0..500 {
                    if is_eof(&read_c) && is_eof(&read_d) {
                        return true;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
                false
            }));
            // SAFETY: terminate the child immediately with the verdict.
            unsafe { libc::_exit(if verdict.unwrap_or(false) { 0 } else { 1 }) };
        }
        drop(write_c);
        drop(write_d);
        let mut status = 0;
        // SAFETY: wait for the exact child created above.
        assert_eq!(unsafe { libc::waitpid(pid, &mut status, 0) }, pid);
        assert!(
            libc::WIFEXITED(status),
            "fork child did not exit normally (status {status:#x}); a hang ends in SIGALRM"
        );
        assert_eq!(
            libc::WEXITSTATUS(status),
            0,
            "fork child could not restart the reaper workers"
        );
        // The child's adoption was its own copy of the counts.
        assert_eq!(reaper.counts(), (1, 1));
    }

    /// The thread reaper as the installed sink: once it reports idle,
    /// its destroyed count proves both closes completed.
    #[test]
    fn backend_drop_with_thread_reaper_leaves_no_vm() {
        let reaper = VmTeardownReaper::new().expect("spawn teardown workers");
        {
            let mut backend = crate::KvmBackend::new(16 * 1024 * 1024)
                .expect("the deferred-teardown test requires /dev/kvm");
            backend.set_vm_teardown_sink(Some(Arc::new(reaper.clone())));
        }
        reaper.wait_idle();
        assert_eq!(reaper.counts(), (1, 1));
    }
}
