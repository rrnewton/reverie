/* Copyright (c) Meta Platforms, Inc. and affiliates. All rights reserved.
 * Licensed under the BSD-style license in the root LICENSE file. */

//! Original-task stop/control history and physical control exclusion, NOT a cohort.
//! No source acquisition, MM identity, writer exclusion, birth census, or
//! publication authority follows from this type. Passive Command-lineage
//! bookkeeping may retain it; no cohort source reader is activated.

use super::*;

/// Non-Clone, non-serializable witness of a genuinely consumed notifier stop,
/// advanced only by a completed PTRACE_SETREGSET of general registers.
///
/// Retains the original Event and WorkerIdentity, never reconstructing them
/// from a PID, proc scan, attach, or pidfd liveness. A numeric ptrace operation
/// still has the existing I28 limitation; this is not raw ATTACH/SEIZE support.
pub struct ControlStop {
    pid: Pid,
    // Identity/control association only: never clone the consumed-stop stamp.
    handle: EventHandle,
    generation: Arc<Event>,
    _identity: Arc<WorkerIdentity>,
    thread: thread::ThreadId,
    wait_revision: u64,
    revision: u64,
}

impl ControlStop {
    pub(crate) fn from_stopped(stopped: &Stopped) -> Result<Self, Errno> {
        let stamp = stopped.1.source.as_ref().ok_or(Errno::ENODATA)?;
        if !stamp.consumed || stamp.thread != thread::current().id() {
            return Err(Errno::EPERM);
        }
        // An ordinary job-control stop cannot issue a physical ControlHold.
        // CLD_TRAPPED alone does not establish exact tracer-thread ownership.
        if !stamp.receipt.is_ptrace_stop() {
            return Err(Errno::EPERM);
        }
        let handle = stopped.1.event();
        let identity = handle.identity().cloned().ok_or(Errno::ENODATA)?;
        if identity.pid != stopped.pid() {
            return Err(Errno::ESTALE);
        }
        let generation = Arc::clone(handle.event());
        let mut state = generation.source.lock();
        if !stamp.receipt.matches(&state, &generation)
            || !state.consumed
            || state.published != stamp.receipt.revision
            || state.mutation.is_some()
            || generation.cleanup_cancel_requested.load(Ordering::Acquire)
            || generation.exit_status.load(Ordering::Acquire) != EXIT_PENDING
        {
            return Err(Errno::ESTALE);
        }
        if state.control_stop.is_some() {
            return Err(Errno::EALREADY);
        }
        let revision = state.revision;
        state.control_stop = Some(revision);
        drop(state);
        Ok(Self {
            pid: stopped.pid(),
            handle: handle.clone(),
            generation,
            _identity: identity,
            thread: stamp.thread,
            wait_revision: revision,
            revision,
        })
    }

    fn live(&self) -> bool {
        !self
            .generation
            .cleanup_cancel_requested
            .load(Ordering::Acquire)
            && self.generation.exit_status.load(Ordering::Acquire) == EXIT_PENDING
            && Arc::ptr_eq(&self.generation, self.handle.event())
    }

    /// Revalidate on the committed-wait return thread; this does not refresh
    /// expiry or establish kernel ptracer-thread ownership.
    pub fn validate_current(&self) -> Result<(), Errno> {
        if self.thread != thread::current().id() {
            return Err(Errno::EPERM);
        }
        let state = self.generation.source.lock();
        if self.live()
            && state.mutation.is_none()
            && state.signals == 0
            && state.revision == self.revision
            && state.control_stop == Some(self.revision)
        {
            Ok(())
        } else {
            Err(Errno::ESTALE)
        }
    }

    /// Compare original notifier generations, without resolving a numeric TID.
    /// Equality does not imply that either witness remains current.
    pub fn same_task(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.generation, &other.generation)
    }

    /// Compare the genuine wait that began each control history. A successful
    /// register write advances control history without inventing another wait.
    pub fn same_wait(&self, other: &Self) -> bool {
        self.same_task(other) && self.wait_revision == other.wait_revision
    }

    /// Compile-only native regression probes using this original Event view.
    /// No synthetic wait or new authority is issued. The returned cleanup
    /// identity does not retain the hold and cannot keep the oracle passing.
    #[cfg(all(cohort_final_test, feature = "memory"))]
    pub fn probe_held_controls(
        &self,
        address: usize,
    ) -> Result<(TerminalCleanup, [bool; 4]), Errno> {
        use reverie_memory::MemoryAccess;
        self.validate_current()?;
        let mut stopped =
            Stopped::from_token(self.pid, TraceeToken::from_event(self.handle.clone()));
        let terminal = stopped.terminal_cleanup();
        let resume = terminal.continue_for_cleanup() == Err(Errno::EBUSY);
        let registers = stopped.getregs().is_ok_and(|regs| {
            matches!(
                stopped.setregs(&regs),
                Err(crate::Error::Errno(Errno::EBUSY))
            )
        });
        let memory = stopped.write_exact(
            reverie_memory::AddrMut::from_raw(address).ok_or(Errno::EFAULT)?,
            &[0x58],
        ) == Err(Errno::EBUSY);
        let signal = terminal.terminate_bound_task() == Err(Errno::EBUSY);
        Ok((terminal, [resume, registers, memory, signal]))
    }

    /// Consume this witness, perform exactly one non-resuming register write,
    /// and return new authority only on typed successful completion.
    ///
    /// Old legacy SourceStop values remain invalid. Error, cancellation, panic,
    /// and abandoned operations issue nothing. This cannot execute guest code.
    pub fn setregs(self, registers: &crate::Regs) -> Result<Self, crate::Error> {
        self.begin_register_write()?.write(registers)
    }

    /// Reserve physical control exclusion from this original, current stop.
    /// Non-resuming mutation, resume and owned fatal signalling refuse while
    /// the hold exists. This proves neither cohort completeness nor immunity
    /// to an independently authorized external writer or signal sender.
    pub fn hold(&self) -> Result<ControlHold, Errno> {
        self.validate_current()?;
        let mut state = self.generation.source.lock();
        if !self.live() {
            return Err(Errno::ESTALE);
        }
        let ticket = state.reserve_hold(self.revision)?;
        Ok(ControlHold {
            pid: self.pid,
            handle: self.handle.clone(),
            generation: Arc::clone(&self.generation),
            identity: Arc::clone(&self._identity),
            thread: self.thread,
            revision: self.revision,
            ticket,
        })
    }

    fn begin_register_write(self) -> Result<RegisterWrite, Errno> {
        self.validate_current()?;
        let control = self.handle.source_control(self.pid)?;
        // source_control invalidated BOTH old authorities before the effect.
        // Check again under its original-owner ticket; an intervening mutation
        // or notification cannot be mistaken for our single control transition.
        let next = self.revision.checked_add(1).ok_or(Errno::ESTALE)?;
        let state = self.generation.source.lock();
        if next == u64::MAX || state.revision != next || !self.live() {
            return Err(Errno::ESTALE);
        }
        drop(state);
        Ok(RegisterWrite {
            stop: self,
            control,
            next,
        })
    }
}

/// Non-Clone physical exclusion from one original ControlStop. The backend
/// must retain every member's hold through actual reader-thread join.
pub struct ControlHold {
    pid: Pid,
    handle: EventHandle,
    generation: Arc<Event>,
    identity: Arc<WorkerIdentity>,
    thread: thread::ThreadId,
    revision: u64,
    ticket: Arc<()>,
}

impl ControlHold {
    #[cfg(all(feature = "memory", target_arch = "x86_64"))]
    pub(crate) fn begin_register_capture(&self) -> Result<SourceRegisterCapture<'_>, Errno> {
        if self.thread != thread::current().id() {
            return Err(Errno::EPERM);
        }
        let mut state = self.generation.source.lock();
        if state.revision != self.revision
            || state.control_stop != Some(self.revision)
            || state.mutation.is_some()
            || state.signals != 0
            || !state
                .hold
                .as_ref()
                .is_some_and(|ticket| Arc::ptr_eq(ticket, &self.ticket))
            || self
                .generation
                .cleanup_cancel_requested
                .load(Ordering::Acquire)
            || self.generation.exit_status.load(Ordering::Acquire) != EXIT_PENDING
        {
            return Err(Errno::ESTALE);
        }
        let ticket = state.reserve_register_capture()?;
        Ok(SourceRegisterCapture {
            generation: Arc::clone(&self.generation),
            ticket,
            stopped: Stopped::from_token(self.pid, TraceeToken::from_event(self.handle.clone())),
            _borrow: std::marker::PhantomData,
            _thread: std::marker::PhantomData,
        })
    }

    /// Check the inherited single Command filter on the acquisition worker.
    pub fn validate_single_source_filter(&self) -> Result<(), Errno> {
        if self.thread == thread::current().id() {
            return Err(Errno::EPERM);
        }
        self.validate()?;
        single_source_filter(&self.identity, self.pid)?;
        self.validate()
    }
    /// Validate this interval without renewing any stop or legacy source epoch.
    pub fn validate(&self) -> Result<(), Errno> {
        let state = self.generation.source.lock();
        if state.revision == self.revision
            && state.control_stop == Some(self.revision)
            && state.mutation.is_none()
            && state.signals == 0
            && state
                .hold
                .as_ref()
                .is_some_and(|t| Arc::ptr_eq(t, &self.ticket))
            && !self
                .generation
                .cleanup_cancel_requested
                .load(Ordering::Acquire)
            && self.generation.exit_status.load(Ordering::Acquire) == EXIT_PENDING
        {
            Ok(())
        } else {
            Err(Errno::ESTALE)
        }
    }

    /// Borrow the retained Event view on the committed-wait return thread.
    /// This userspace view is not numerically reconstructed, but does not prove
    /// kernel ptracer-thread authorization or bind a later numeric ptrace
    /// operation to the retained generation.
    pub fn with_stopped<T>(&self, f: impl FnOnce(&Stopped) -> T) -> Result<T, Errno> {
        if self.thread != thread::current().id() {
            return Err(Errno::EPERM);
        }
        self.validate()?;
        Ok(f(&Stopped::from_token(
            self.pid,
            TraceeToken::from_event(self.handle.clone()),
        )))
    }

    /// Original proc directory, retained from notifier enrollment.
    pub fn task_directory(&self) -> BorrowedFd<'_> {
        self.identity.proc_dir.as_fd()
    }

    /// Original directory's recorded device and inode.
    pub fn task_directory_identity(&self) -> (u64, u64) {
        (self.identity.proc_device, self.identity.proc_inode)
    }

    /// TID for cross-checking observations, never membership admission.
    pub fn expected_tid(&self) -> i32 {
        self.pid.as_raw()
    }
}

impl Drop for ControlHold {
    fn drop(&mut self) {
        let mut state = self.generation.source.lock();
        assert!(
            state
                .hold
                .as_ref()
                .is_some_and(|t| Arc::ptr_eq(t, &self.ticket))
        );
        state.hold = None;
        drop(state);
        self.generation.source_idle.notify_all();
    }
}

// Private operation types, not caller-supplied Boolean completion or a callback
// allowed to report arbitrary success. Drop only releases the existing ticket.
struct RegisterWrite {
    stop: ControlStop,
    control: SourceControl,
    next: u64,
}
struct CompletedRegisterWrite(RegisterWrite);

impl RegisterWrite {
    fn write(self, registers: &crate::Regs) -> Result<ControlStop, crate::Error> {
        let iov = libc::iovec {
            iov_base: registers as *const _ as *mut _,
            iov_len: core::mem::size_of::<crate::Regs>(),
        };
        // SETREGSET cannot resume the tracee. No mutex spans the syscall.
        // The same per-task mutation/admission ticket fences ordinary aliases
        // and notifier consumption until completion or abandonment.
        unsafe {
            syscalls::syscall!(
                syscalls::Sysno::ptrace,
                libc::PTRACE_SETREGSET,
                self.stop.pid.as_raw(),
                libc::NT_PRSTATUS,
                &iov as *const _
            )
        }?;
        CompletedRegisterWrite(self).finish().map_err(Into::into)
    }
}

impl CompletedRegisterWrite {
    fn finish(self) -> Result<ControlStop, Errno> {
        let RegisterWrite {
            mut stop,
            control,
            next,
        } = self.0;
        let mut state = stop.generation.source.lock();
        if state.revision != next
            || !stop.live()
            || !state
                .mutation
                .as_ref()
                .is_some_and(|ticket| Arc::ptr_eq(ticket, &control.ticket))
        {
            return Err(Errno::ESTALE);
        }
        // Do not restore published/consumed/issued or the old SourceStamp.
        state.control_stop = Some(next);
        stop.revision = next;
        drop(state);
        drop(control);
        Ok(stop)
    }
}

#[cfg(test)]
pub(super) fn abandon_register_write(stop: ControlStop) -> Result<(), Errno> {
    drop(stop.begin_register_write()?);
    Ok(())
}

/// A distinct native-store reservation on the original Event, control revision
/// and WorkerIdentity. The notifier cannot consume/reap this task while it is
/// held. The backend separately excludes controls by all followed MM users.
/// External authorized writers/signals remain outside that cohort proof.
#[cfg(all(feature = "memory", target_arch = "x86_64"))]
pub struct NativeStorePermit<'a> {
    hold: &'a ControlHold,
    ticket: Arc<()>,
    stopped: Stopped,
    used: std::cell::Cell<bool>,
    _thread: std::marker::PhantomData<std::rc::Rc<()>>,
}

#[cfg(all(feature = "memory", target_arch = "x86_64"))]
impl ControlHold {
    /// Reserve one store on the original committed-wait thread. This does not
    /// release the hold or admit ordinary mutation through another alias.
    pub fn begin_native_store(&self) -> Result<NativeStorePermit<'_>, Errno> {
        if self.thread != thread::current().id() {
            return Err(Errno::EPERM);
        }
        self.validate()?;
        let mut state = self.generation.source.lock();
        if state.revision != self.revision
            || state.control_stop != Some(self.revision)
            || state.mutation.is_some()
            || state.register_capture.is_some()
            || state.held_write.is_some()
            || state.signals != 0
            || !state
                .hold
                .as_ref()
                .is_some_and(|t| Arc::ptr_eq(t, &self.ticket))
            || self
                .generation
                .cleanup_cancel_requested
                .load(Ordering::Acquire)
            || self.generation.exit_status.load(Ordering::Acquire) != EXIT_PENDING
        {
            return Err(Errno::EBUSY);
        }
        let ticket = Arc::new(());
        state.held_write = Some(Arc::clone(&ticket));
        drop(state);
        let permit = NativeStorePermit {
            hold: self,
            ticket,
            stopped: Stopped::from_token(self.pid, TraceeToken::from_event(self.handle.clone())),
            used: std::cell::Cell::new(false),
            _thread: std::marker::PhantomData,
        };
        permit.validate()?;
        Ok(permit)
    }
}

#[cfg(all(feature = "memory", target_arch = "x86_64"))]
impl NativeStorePermit<'_> {
    /// Revalidate original custody and check for a new kernel wait status
    /// without consuming it. External fatal teardown may progress despite a
    /// ptrace stop; observing it refuses success and does not hide prior effects.
    pub fn validate(&self) -> Result<(), Errno> {
        if self.hold.thread != thread::current().id() {
            return Err(Errno::EPERM);
        }
        self.hold.validate()?;
        {
            let state = self.hold.generation.source.lock();
            if !state
                .held_write
                .as_ref()
                .is_some_and(|t| Arc::ptr_eq(t, &self.ticket))
            {
                return Err(Errno::ESTALE);
            }
        }
        let flags = WaitPidFlag::from_bits_retain(
            WaitPidFlag::WEXITED.bits()
                | WaitPidFlag::WSTOPPED.bits()
                | WaitPidFlag::WNOHANG.bits()
                | WaitPidFlag::WNOWAIT.bits()
                | libc::__WALL,
        );
        if waitid::waitpidfd(self.hold.identity.pidfd.as_raw_fd(), flags)?.is_some() {
            return Err(Errno::ESTALE);
        }
        self.hold.validate()
    }

    pub(crate) fn stopped(&self) -> &Stopped {
        &self.stopped
    }
    pub(crate) fn control(&self) -> &ControlHold {
        self.hold
    }

    /// Attempt at most one bounded private-anonymous native store. The caller
    /// retains every other followed task's hold and its semantic exclusion.
    /// No ptrace/proc-mem write fallback and no retry are performed.
    pub fn write(&self, address: usize, bytes: &[u8]) -> reverie_memory::NativeUserStoreOutcome {
        use reverie_memory::NativeUserReadRefusal as E;
        use reverie_memory::NativeUserStoreOutcome as O;
        use reverie_memory::NativeUserStoreRefusal as R;
        if self.used.replace(true) {
            return O::Refused(R::Evidence(E::TargetState(Errno::EALREADY)));
        }
        crate::memory::write_held_native(self, address, bytes)
    }
}

#[cfg(all(feature = "memory", target_arch = "x86_64"))]
impl Drop for NativeStorePermit<'_> {
    fn drop(&mut self) {
        let mut state = self.hold.generation.source.lock();
        assert!(
            state
                .held_write
                .as_ref()
                .is_some_and(|t| Arc::ptr_eq(t, &self.ticket)),
            "native store cannot release another owner's ticket"
        );
        state.held_write = None;
        drop(state);
        self.hold.generation.source_idle.notify_all();
    }
}
