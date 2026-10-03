/* Copyright (c) Meta Platforms, Inc. and affiliates. All rights reserved.
 * Licensed under the BSD-style license in the root LICENSE file. */

//! Source acquisition from an actual consumed stop, never a PID reconstruction.

use std::os::fd::AsFd;
use std::os::fd::BorrowedFd;

use super::*;

#[path = "control_stop.rs"]
mod control_stop;
pub use control_stop::ControlHold;
pub use control_stop::ControlStop;
#[cfg(all(feature = "memory", target_arch = "x86_64"))]
pub use control_stop::NativeStorePermit;

#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub(crate) struct SourceStamp {
    receipt: ConsumedReceipt,
    thread: thread::ThreadId,
    consumed: bool,
}

/// Created only with the source gate held after an actual consuming wait.
/// Weak ownership prevents Event -> FIFO -> receipt -> Event reference cycles.
#[derive(Clone, Debug)]
pub(super) struct ConsumedReceipt {
    origin: Weak<Event>,
    // None is permanent exhaustion, not a reusable eligible revision.
    revision: Option<u64>,
    // From the same consuming waitid, never reconstructed from WIFSTOPPED.
    si_code: i32,
}

impl PartialEq for ConsumedReceipt {
    fn eq(&self, other: &Self) -> bool {
        self.origin.ptr_eq(&other.origin)
            && self.revision == other.revision
            && self.si_code == other.si_code
    }
}

impl Eq for ConsumedReceipt {}

impl Hash for ConsumedReceipt {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.origin.as_ptr().hash(state);
        self.revision.hash(state);
        self.si_code.hash(state);
    }
}

impl ConsumedReceipt {
    fn is_ptrace_stop(&self) -> bool {
        self.si_code == libc::CLD_TRAPPED
    }

    fn matches(&self, state: &SourceState, event: &Event) -> bool {
        std::ptr::eq(self.origin.as_ptr(), event)
            && self.revision == Some(state.revision)
            && state.revision != u64::MAX
    }
}

#[derive(Debug, Default)]
pub(crate) struct SourceState {
    revision: u64,
    published: Option<u64>,
    consumed: bool,
    issued: bool,
    acquired: bool,
    acquiring: bool,
    // Separate from legacy consumed-stop/source issuance. Never restored by Drop.
    control_stop: Option<u64>,
    // A distinct physical interval; invalidation NEVER releases its owner.
    hold: Option<Arc<()>>,
    signals: usize,
    // Never cleared by invalidation/cancellation. Only this ticket's guard
    // may release the in-flight physical effect.
    pub(super) mutation: Option<Arc<()>>,
    // Only the synchronous native register pair owns this ticket. It never
    // spans proc IO, worker execution, or the rest of an acquisition/hold.
    pub(super) register_capture: Option<Arc<()>>,
    // Distinct whole native-store interval, including proc observation and
    // postchecks. Never borrow the short register-capture ticket for this.
    pub(super) held_write: Option<Arc<()>>,
}

impl SourceState {
    #[cfg(all(feature = "memory", target_arch = "x86_64"))]
    fn reserve_register_capture(&mut self) -> Result<Arc<()>, Errno> {
        if self.register_capture.is_some()
            || self.held_write.is_some()
            || self.mutation.is_some()
            || self.signals != 0
        {
            return Err(Errno::EBUSY);
        }
        let ticket = Arc::new(());
        self.register_capture = Some(Arc::clone(&ticket));
        Ok(ticket)
    }

    fn reserve_hold(&mut self, revision: u64) -> Result<Arc<()>, Errno> {
        if self.revision != revision
            || self.control_stop != Some(revision)
            || self.mutation.is_some()
            || self.signals != 0
            || self.acquiring
            || self.hold.is_some()
            || self.held_write.is_some()
        {
            return Err(Errno::EBUSY);
        }
        let ticket = Arc::new(());
        self.hold = Some(Arc::clone(&ticket));
        Ok(ticket)
    }

    pub(super) fn invalidate(&mut self) {
        // Exhaustion is a refusal, never generation reuse.
        self.revision = self.revision.saturating_add(1);
        self.published = None;
        self.consumed = false;
        self.issued = false;
        self.acquired = false;
        self.control_stop = None;
    }

    pub(super) fn record_consumed(
        &mut self,
        event: &Arc<Event>,
        raw: i32,
        si_code: i32,
    ) -> Option<ConsumedReceipt> {
        self.invalidate();
        (libc::WIFSTOPPED(raw) && raw != PTRACE_EVENT_EXIT_STOP).then(|| ConsumedReceipt {
            origin: Arc::downgrade(event),
            revision: (self.revision != u64::MAX).then_some(self.revision),
            si_code,
        })
    }

    pub(super) fn publish(&mut self, event: &Event, receipt: Option<&ConsumedReceipt>) {
        match receipt {
            Some(receipt) if receipt.matches(self, event) => {
                self.published = receipt.revision;
            }
            Some(_) => {} // Late publication cannot renew an invalidated entry.
            None => self.invalidate(), // Terminal/synthetic status grants no source.
        }
    }
}

pub(super) fn candidate(mut token: TraceeToken, receipt: Option<ConsumedReceipt>) -> TraceeToken {
    // Bind the authenticated return/preview thread, never the notifier worker.
    // No ambient source state is sampled, and only committed return consumes.
    token.source = receipt.map(|receipt| SourceStamp {
        receipt,
        thread: thread::current().id(),
        consumed: false,
    });
    token
}

/// Called only AFTER the notifier return transaction committed its FIFO front.
/// A raw decoder/reservation preview never reaches this function.
pub(super) fn consumed(mut wait: Wait) -> Wait {
    if let Wait::Stopped(stopped, _) = &mut wait {
        let handle = stopped.1.event().clone();
        let event = handle.event();
        // The original stop must still be the latest publication. Lock order
        // agrees with Event::update; nothing here performs proc IO or ptrace.
        let status = event.status.lock();
        let mut state = event.source.lock();
        if let Some(stamp) = stopped.1.source.as_mut()
            && status.pending.is_empty()
            && status.terminal == INVALID_STATUS
            && event.exit_status.load(Ordering::Acquire) == EXIT_PENDING
            && !event.cleanup_cancel_requested.load(Ordering::Acquire)
            && stamp.receipt.matches(&state, event)
            && state.published == stamp.receipt.revision
            && !state.consumed
        {
            state.consumed = true;
            stamp.consumed = true;
        }
    }
    wait
}

/// Pins one original owner across an effect without retaining any mutex.
/// The admission reservation also fences first registration of a raw alias.
pub(crate) struct SourceControl {
    event: Arc<Event>,
    ticket: Arc<()>,
    _identity: Option<Arc<WorkerIdentity>>,
    admission: Option<super::mutation::AdmissionGuard>,
}

impl Drop for SourceControl {
    fn drop(&mut self) {
        let mut state = self.event.source.lock();
        assert!(
            state
                .mutation
                .as_ref()
                .is_some_and(|ticket| Arc::ptr_eq(ticket, &self.ticket))
        );
        state.mutation = None;
        drop(state);
        // Release admission while the original Event is still pinned. Its
        // eventual destruction may drop a user-supplied waker reentrantly.
        drop(self.admission.take());
        self.event.source_idle.notify_all();
        // Admission releases only after the original Event is idle. Neither
        // notification invokes arbitrary wakers or resolves a numeric owner.
    }
}

// Fatal signals can cause clear-child-tid and robust-list writes without CONT.
// Reserve their physical interval too, without changing legacy acquisition's
// cancellation behavior. Unknown external signal senders are outside this gate.
/// Pins the original signal-control interval. Drop does not renew a stop.
pub struct SourceSignal(Arc<Event>);
impl Drop for SourceSignal {
    fn drop(&mut self) {
        self.0.source.lock().signals -= 1;
    }
}

impl EventHandle {
    pub(super) fn source_signal(&self) -> Result<SourceSignal, Errno> {
        let event = Arc::clone(self.event());
        let mut state = event.source.lock();
        if state.hold.is_some() {
            return Err(Errno::EBUSY);
        }
        state.signals = state.signals.checked_add(1).ok_or(Errno::EOVERFLOW)?;
        state.invalidate();
        drop(state);
        Ok(SourceSignal(event))
    }
    /// Raw decoding retains known control identity but never consumes a stop.
    /// Do not capture proc state or open a numeric PID to fill an absent entry.
    pub(crate) fn for_raw_control(pid: Pid) -> Self {
        NOTIFIER
            .pids
            .lock()
            .get(&pid)
            .map(|entry| entry.handle.resolved_handle())
            .unwrap_or_else(Self::new)
    }

    pub(crate) fn source_control(&self, pid: Pid) -> Result<SourceControl, Errno> {
        // Wait only on this PID's admission, BEFORE registry/adoption/source.
        // An unresolved alias reserves the same cell as first registration.
        let admission = NOTIFIER.admissions.enter(pid);
        let registry = NOTIFIER.pids.lock();
        if let Some(entry) = registry.get(&pid) {
            if let Some(bound) = self.identity()
                && !bound.same_generation(&entry.identity)
            {
                return Err(Errno::ESTALE);
            }
            self.adopt_authoritative(&entry.handle)?;
        }
        let event = Arc::clone(self.event());
        let identity = self.identity().cloned();
        let mut state = event.source.lock();
        if state.acquiring || state.hold.is_some() {
            return Err(Errno::EBUSY);
        }
        // Retained identity checks detect stale aliases but do not close the
        // subsequent numeric ptrace PID-reuse window (I28). This reservation
        // grants neither a source receipt nor physical kernel PID custody.
        if let Some(bound) = self.identity()
            && bound.pid != pid
        {
            return Err(Errno::ESTALE);
        }
        state.invalidate();
        if let Some(bound) = self.identity()
            && !bound.pidfd_is_live()?
        {
            // Preserve ordinary dead-task control/cleanup errno handling.
            // This refusal is not a terminal wait or retirement receipt.
            return Err(Errno::ESRCH);
        }
        assert!(state.mutation.is_none(), "same owner escaped PID admission");
        let ticket = Arc::new(());
        state.mutation = Some(Arc::clone(&ticket));
        drop(state);
        drop(registry);
        Ok(SourceControl {
            event,
            ticket,
            _identity: identity,
            admission: Some(admission),
        })
    }
}

/// Non-Clone witness of one consumed stop and its original notifier identity.
/// This is acquisition/control evidence, not source-permission or writer history.
pub struct SourceStop {
    pid: Pid,
    token: TraceeToken,
    generation: Arc<Event>,
    identity: Arc<WorkerIdentity>,
    stamp: SourceStamp,
}

impl SourceStop {
    pub(crate) fn from_stopped(stopped: &Stopped) -> Result<Self, Errno> {
        let stamp = stopped.1.source.as_ref().ok_or(Errno::ENODATA)?.clone();
        if !stamp.consumed || stamp.thread != thread::current().id() {
            return Err(Errno::EPERM);
        }
        // A consumed ordinary SIGSTOP grants no execution-control ownership.
        // This preserves CLD_TRAPPED behavior; exact tracer-T/lifetime closure
        // remains a separate prerequisite, not a conclusion from si_code.
        if !stamp.receipt.is_ptrace_stop() {
            return Err(Errno::EPERM);
        }
        let handle = stopped.1.event();
        let generation = Arc::clone(handle.event());
        // Intentionally no ensure_registered, proc capture, or numeric open.
        let identity = handle.identity().cloned().ok_or(Errno::ENODATA)?;
        if identity.pid != stopped.pid() {
            return Err(Errno::ESRCH);
        }
        let result = Self {
            pid: stopped.pid(),
            token: stopped.1.clone(),
            generation,
            identity,
            stamp,
        };
        result.validate_current()?;
        let mut state = result.generation.source.lock();
        if state.issued || !result.matches(&state) {
            return Err(Errno::EALREADY);
        }
        state.issued = true;
        drop(state);
        Ok(result)
    }

    fn matches(&self, state: &SourceState) -> bool {
        state.mutation.is_none()
            && state.consumed
            && self.stamp.receipt.matches(state, &self.generation)
            && state.published == self.stamp.receipt.revision
            && Arc::ptr_eq(&self.generation, self.token.event().event())
            && !self
                .generation
                .cleanup_cancel_requested
                .load(Ordering::Acquire)
            && self.generation.exit_status.load(Ordering::Acquire) == EXIT_PENDING
    }

    /// Revalidate the original consumed stop on the thread that received the
    /// committed wait result. This does not establish that the thread is the
    /// kernel ptracer.
    pub fn validate_current(&self) -> Result<(), Errno> {
        if self.stamp.thread != thread::current().id() {
            return Err(Errno::EPERM);
        }
        if !self.matches(&self.generation.source.lock()) {
            return Err(Errno::ESTALE);
        }
        Ok(())
    }

    /// On the acquisition worker, require the installed Command filter to be
    /// the sole seccomp filter. Stacked USER_NOTIF can outrank TRACE, so an
    /// inherited or subsequently installed stack cannot support this observer.
    /// This performs bounded fd-relative proc IO while the genuine acquisition
    /// is held; it does not perform ptrace or grant stop/MM/read authority.
    pub fn validate_single_source_filter(&self) -> Result<(), Errno> {
        let validate = || {
            let state = self.generation.source.lock();
            if state.acquiring && self.matches(&state) {
                Ok(())
            } else {
                Err(Errno::ESTALE)
            }
        };
        validate()?;
        if self.stamp.thread == thread::current().id() {
            return Err(Errno::EPERM);
        }
        single_source_filter(&self.identity, self.pid)?;
        validate()
    }

    /// Compare the original task lineage of two genuine consumed stops.
    /// This does not make the earlier stop current or recapture an identity.
    pub fn same_task(&self, other: &Self) -> bool {
        self.pid == other.pid && Arc::ptr_eq(&self.generation, &other.generation)
    }

    /// Compare original retained Event references, without registering a PID.
    pub fn same_generation(&self, cleanup: &TerminalCleanup) -> bool {
        self.pid == cleanup.pid && Arc::ptr_eq(&self.generation, cleanup.event.event())
    }

    /// Interlock a single acquisition against all execution/register mutations.
    /// Conflicting control refuses immediately; no mutex spans worker proc IO.
    pub fn begin_acquisition(&self) -> Result<SourceAcquisition, Errno> {
        self.validate_current()?;
        let mut state = self.generation.source.lock();
        if !self.matches(&state) || state.acquired || state.acquiring || state.hold.is_some() {
            return Err(Errno::EBUSY);
        }
        state.acquired = true;
        state.acquiring = true;
        Ok(SourceAcquisition {
            pid: self.pid,
            token: self.token.clone(),
            generation: Arc::clone(&self.generation),
            identity: Arc::clone(&self.identity),
            stamp: self.stamp.clone(),
        })
    }
}

fn single_source_filter(identity: &WorkerIdentity, pid: Pid) -> Result<(), Errno> {
    use std::io::Read;
    let raw = unsafe {
        libc::openat(
            identity.proc_dir.as_raw_fd(),
            c"status".as_ptr(),
            libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
        )
    };
    if raw < 0 {
        return Err(Errno::last());
    }
    let mut status = unsafe { std::fs::File::from_raw_fd(raw) };
    for fd in [identity.proc_dir.as_raw_fd(), status.as_raw_fd()] {
        let mut fs = std::mem::MaybeUninit::<libc::statfs>::uninit();
        Errno::result(unsafe { libc::fstatfs(fd, fs.as_mut_ptr()) })?;
        if unsafe { fs.assume_init() }.f_type != libc::PROC_SUPER_MAGIC {
            return Err(Errno::EPROTO);
        }
    }
    let metadata = status.metadata().map_err(io_errno)?;
    if metadata.dev() != identity.proc_device
        || fd_inode(&identity.proc_dir).map_err(io_errno)? != identity.proc_inode
    {
        return Err(Errno::ESTALE);
    }
    let mut bytes = Vec::new();
    (&mut status)
        .take(16 * 1024 + 1)
        .read_to_end(&mut bytes)
        .map_err(io_errno)?;
    if bytes.len() > 16 * 1024 || !bytes.ends_with(b"\n") {
        return Err(Errno::EPROTO);
    }
    let text = std::str::from_utf8(&bytes).map_err(|_| Errno::EPROTO)?;
    let field = |name: &str| -> Result<u64, Errno> {
        let mut values = text.lines().filter_map(|line| line.strip_prefix(name));
        let value = values.next().ok_or(Errno::EPROTO)?.trim();
        if values.next().is_some() || value.is_empty() || !value.bytes().all(|b| b.is_ascii_digit())
        {
            return Err(Errno::EPROTO);
        }
        value.parse().map_err(|_| Errno::EPROTO)
    };
    if field("Pid:")? != pid.as_raw() as u64
        || field("Seccomp:")? != 2
        || field("Seccomp_filters:")? != 1
    {
        return Err(Errno::ENOTSUPP);
    }
    Ok(())
}

/// Owns the short no-control interval while the worker binds both MM files.
/// Non-Clone and Send; register-view access is restricted to the committed-wait
/// return thread. Kernel ptracer-thread authorization is a separate prerequisite.
pub struct SourceAcquisition {
    pid: Pid,
    token: TraceeToken,
    generation: Arc<Event>,
    identity: Arc<WorkerIdentity>,
    stamp: SourceStamp,
}

// Crate-private, borrowed and !Send/!Sync: no public callback or async caller
// can retain the notifier consumption exclusion. The closed native reader
// keeps it only across PRSTATUS and XSTATE, on the committed-return thread.
#[cfg(all(feature = "memory", target_arch = "x86_64"))]
pub(crate) struct SourceRegisterCapture<'a> {
    generation: Arc<Event>,
    ticket: Arc<()>,
    stopped: Stopped,
    _borrow: std::marker::PhantomData<&'a ()>,
    _thread: std::marker::PhantomData<std::rc::Rc<()>>,
}

#[cfg(all(feature = "memory", target_arch = "x86_64"))]
impl SourceRegisterCapture<'_> {
    pub(crate) fn stopped(&self) -> &Stopped {
        &self.stopped
    }
}

#[cfg(all(feature = "memory", target_arch = "x86_64"))]
impl Drop for SourceRegisterCapture<'_> {
    fn drop(&mut self) {
        let mut state = self.generation.source.lock();
        assert!(
            state
                .register_capture
                .as_ref()
                .is_some_and(|ticket| Arc::ptr_eq(ticket, &self.ticket)),
            "register capture cannot release another owner's ticket"
        );
        state.register_capture = None;
        drop(state);
        self.generation.source_idle.notify_all();
    }
}

impl SourceAcquisition {
    #[cfg(all(feature = "memory", target_arch = "x86_64"))]
    pub(crate) fn begin_register_capture(&self) -> Result<SourceRegisterCapture<'_>, Errno> {
        if self.stamp.thread != thread::current().id() {
            return Err(Errno::EPERM);
        }
        let mut state = self.generation.source.lock();
        if !state.acquiring
            || !state.consumed
            || !self.stamp.receipt.matches(&state, &self.generation)
            || !Arc::ptr_eq(&self.generation, self.token.event().event())
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
            stopped: Stopped::from_token(self.pid, self.token.clone()),
            _borrow: std::marker::PhantomData,
            _thread: std::marker::PhantomData,
        })
    }

    /// Borrow the consumed-stop view on its committed-wait return thread.
    /// A numeric ptrace operation still requires kernel ptracer ownership;
    /// this view does not bind that operation's numeric target to a generation.
    pub fn with_stopped<T>(&self, capture: impl FnOnce(&Stopped) -> T) -> Result<T, Errno> {
        if self.stamp.thread != thread::current().id() {
            return Err(Errno::EPERM);
        }
        {
            let state = self.generation.source.lock();
            if !state.acquiring
                || !state.consumed
                || !self.stamp.receipt.matches(&state, &self.generation)
                || self
                    .generation
                    .cleanup_cancel_requested
                    .load(Ordering::Acquire)
                || !Arc::ptr_eq(&self.generation, self.token.event().event())
            {
                return Err(Errno::ESTALE);
            }
        }
        // Carries the original receipt; never creates an unchecked PID owner.
        Ok(capture(&Stopped::from_token(self.pid, self.token.clone())))
    }

    /// Borrow the ORIGINAL notifier task-directory descriptor.
    pub fn task_directory(&self) -> BorrowedFd<'_> {
        self.identity.proc_dir.as_fd()
    }

    /// Device/inode recorded when that exact notifier directory was acquired.
    pub fn task_directory_identity(&self) -> (u64, u64) {
        (self.identity.proc_device, self.identity.proc_inode)
    }

    /// Expected TID within the original notifier proc view; not admission evidence.
    pub fn expected_tid(&self) -> i32 {
        self.pid.as_raw()
    }

    /// Release acquisition after BOTH MM descriptors and observations are bound.
    /// The original stop still must validate before publication after true join.
    pub fn finish_binding(self) {
        drop(self);
    }
}

impl Drop for SourceAcquisition {
    fn drop(&mut self) {
        self.generation.source.lock().acquiring = false;
    }
}

impl TerminalCleanup {
    /// Continue the retained cleanup generation while respecting source acquisition.
    /// EBUSY means the actual worker still owns acquisition, not terminal success.
    pub fn continue_for_cleanup(&self) -> Result<(), Errno> {
        let _control = self.event.source_control(self.pid)?;
        nix::sys::ptrace::cont(self.pid.into(), None).map_err(|error| Errno::new(error as i32))
    }
}

#[cfg(test)]
#[path = "consumed_receipt_state_tests.rs"]
mod consumed_receipt_state_tests;

#[cfg(test)]
mod hold_state_tests {
    use super::*;

    #[test]
    fn pure_physical_gate_ownership_survives_invalidation() {
        // Pure SourceState control, NOT a constructed authentic stopped task.
        let mut s = SourceState {
            revision: 7,
            control_stop: Some(7),
            ..Default::default()
        };
        let ticket = s.reserve_hold(7).unwrap();
        assert!(s.reserve_hold(7).is_err());
        s.invalidate();
        assert!(Arc::ptr_eq(s.hold.as_ref().unwrap(), &ticket));
        assert!(s.reserve_hold(8).is_err());
    }

    #[test]
    fn pure_physical_gate_refuses_busy_and_stale_state() {
        for state in [
            SourceState::default(),
            SourceState {
                revision: 7,
                control_stop: Some(6),
                ..Default::default()
            },
            SourceState {
                revision: 7,
                control_stop: Some(7),
                acquiring: true,
                ..Default::default()
            },
            SourceState {
                revision: 7,
                control_stop: Some(7),
                signals: 1,
                ..Default::default()
            },
            SourceState {
                revision: 7,
                control_stop: Some(7),
                mutation: Some(Arc::new(())),
                ..Default::default()
            },
        ] {
            let mut state = state;
            assert!(state.reserve_hold(7).is_err());
            assert!(state.hold.is_none());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Signal;

    #[cfg(all(feature = "memory", target_arch = "x86_64"))]
    include!("register_capture_tests.rs");
    #[cfg(all(feature = "memory", target_arch = "x86_64"))]
    include!("native_store_tests.rs");

    include!("control_stop_native_tests.rs");

    include!("../source_stop_repair_tests.rs");
    // These native controls compare the complete x86_64 register layout.
    #[cfg(target_arch = "x86_64")]
    include!("mutation_release_tests.rs");

    // Own cleanup before consuming the first stop, including assertion unwind.
    // This is only a cleanup handle; source authority still comes from wait().
    struct ExactChildGuard(TerminalCleanup);

    impl ExactChildGuard {
        fn cleanup(&self) -> Result<(), Errno> {
            if !self.0.is_reaped()? {
                match self.0.request_sigkill() {
                    Ok(()) | Err(Errno::ESRCH) => {}
                    Err(error) => return Err(error),
                }
            }
            if !self.0.wait(Duration::from_secs(2)) {
                return Err(Errno::ETIMEDOUT);
            }
            // Neither signal delivery nor ECHILD is a terminal acknowledgment.
            self.0.observed_exit_status()?.ok_or(Errno::ENODATA)?;
            futures::executor::block_on(self.0.reap_parent_terminal())?;
            if !self.0.is_reaped()? {
                return Err(Errno::EBUSY);
            }
            Ok(())
        }
    }

    impl Drop for ExactChildGuard {
        fn drop(&mut self) {
            if let Err(error) = self.cleanup() {
                eprintln!("source fixture exact-child kill/reap failed: {error}");
                // Do not silently discard the only retained cleanup owner.
                // The child also installs PDEATHSIG before entering ptrace.
                std::process::abort();
            }
        }
    }

    // Actual TRACEME child and a committed notifier wait. No source receipt is
    // assembled in the fixture. Main runs these native tests serially.
    fn child_stop() -> (ExactChildGuard, Stopped) {
        let parent = unsafe { libc::getpid() };
        let pid = unsafe { libc::fork() };
        assert!(pid >= 0);
        if pid == 0 {
            unsafe {
                if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) != 0 {
                    libc::_exit(121);
                }
                if libc::getppid() != parent {
                    libc::_exit(122);
                }
                if libc::ptrace(libc::PTRACE_TRACEME, 0, 0, 0) != 0 {
                    libc::_exit(120);
                }
                libc::raise(libc::SIGSTOP);
                libc::_exit(0);
            }
        }
        let running = Running::new(Pid::from_raw(pid));
        let cleanup = ExactChildGuard(running.terminal_cleanup());
        match running.wait().expect("consume real initial child stop") {
            Wait::Stopped(stopped, crate::Event::Signal(Signal::SIGSTOP)) => (cleanup, stopped),
            other => panic!("unexpected child state {other:?}"),
        }
    }

    fn finish(stopped: Stopped) {
        let pid = stopped.pid();
        let wait = match stopped.resume(None) {
            Ok(running) => running.wait(),
            Err(crate::Error::Died(zombie)) => {
                futures::executor::block_on(zombie.reap()).map(|status| Wait::Exited(pid, status))
            }
            Err(error) => panic!("resume original child: {error:?}"),
        };
        assert!(matches!(wait.unwrap(), Wait::Exited(_, _)));
    }

    #[test]
    fn actual_consumed_stop_rejects_unchecked_raw_and_stale_source() {
        let (_cleanup, stopped) = child_stop();
        assert!(matches!(
            Stopped::new_unchecked(stopped.pid()).source_stop(),
            Err(Errno::ENODATA)
        ));
        let raw = Wait::from_raw(stopped.pid(), (libc::SIGSTOP << 8) | 0x7f)
            .unwrap()
            .assume_stopped()
            .0;
        assert!(
            raw.source_stop().is_err(),
            "decoding a real-looking status cannot consume it"
        );
        let source = stopped
            .source_stop()
            .expect("actual committed stop issues custody");
        assert!(
            stopped.source_stop().is_err(),
            "one source witness per stop"
        );
        let regs = stopped.getregs().unwrap();
        stopped.setregs(&regs).unwrap();
        assert!(
            matches!(source.validate_current(), Err(Errno::ESTALE)),
            "even same-value control invalidates"
        );
        assert!(source.begin_acquisition().is_err());
        finish(stopped);
    }

    #[test]
    fn actual_acquisition_refuses_control_and_wrong_thread_capture() {
        let (_cleanup, stopped) = child_stop();
        let source = stopped.source_stop().unwrap();
        let acquisition = source.begin_acquisition().unwrap();
        assert!(
            source.begin_acquisition().is_err(),
            "no duplicate acquisition"
        );
        let regs = stopped.getregs().unwrap();
        assert!(matches!(
            stopped.setregs(&regs),
            Err(crate::Error::Errno(Errno::EBUSY))
        ));
        assert!(matches!(
            Stopped::new_unchecked(stopped.pid()).detach(None),
            Err(crate::Error::Errno(Errno::EBUSY))
        ));
        assert!(matches!(
            Stopped::new_unchecked(stopped.pid()).syscall(None),
            Err(crate::Error::Errno(Errno::EBUSY))
        ));
        assert!(matches!(
            Stopped::new_unchecked(stopped.pid()).step(None),
            Err(crate::Error::Errno(Errno::EBUSY))
        ));
        #[cfg(target_arch = "x86_64")]
        {
            let xstate = stopped.getxstate().unwrap();
            assert!(matches!(
                stopped.setxstate(&xstate),
                Err(crate::Error::Errno(Errno::EBUSY))
            ));
        }
        let alias = Stopped::new_unchecked(stopped.pid());
        assert!(matches!(
            alias.setregs(&regs),
            Err(crate::Error::Errno(Errno::EBUSY))
        ));
        let stopped = match stopped.resume_retaining(None) {
            Err((stopped, Errno::EBUSY)) => stopped,
            _ => panic!("acquisition permitted a resume"),
        };
        let acquisition = std::thread::spawn(move || {
            assert!(matches!(
                acquisition.with_stopped(|s| s.getregs()),
                Err(Errno::EPERM)
            ));
            acquisition
        })
        .join()
        .unwrap();
        assert_eq!(acquisition.expected_tid(), stopped.pid().as_raw());
        assert!(acquisition.task_directory_identity().1 != 0);
        acquisition.finish_binding();
        source.validate_current().unwrap();
        assert!(
            source.begin_acquisition().is_err(),
            "binding completion cannot renew acquisition"
        );
        finish(stopped);
        assert!(source.validate_current().is_err());
    }

    #[test]
    fn cancellation_during_real_acquisition_keeps_control_excluded_until_worker_releases() {
        let (_cleanup, stopped) = child_stop();
        let terminal = stopped.terminal_cleanup();
        let source = stopped.source_stop().unwrap();
        let acquisition = source.begin_acquisition().unwrap();
        let (release, wait) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            wait.recv_timeout(Duration::from_secs(2)).unwrap();
            drop(acquisition);
        });
        terminal.request_sigkill().unwrap();
        assert!(source.validate_current().is_err());
        assert_eq!(terminal.continue_for_cleanup(), Err(Errno::EBUSY));
        release.send(()).unwrap();
        worker.join().unwrap();
        let final_wait = futures::executor::block_on(stopped.wait_owned()).unwrap();
        assert!(
            matches!(final_wait, Wait::Exited(_, _)),
            "must consume actual final wait"
        );
        assert!(source.validate_current().is_err());
    }

    // These controls use actual children and the existing original notifier /
    // ExactChildGuard. No si_code, Event, receipt or task authority is modeled.
    mod stop_kind_tests {
        use super::*;

        #[derive(Debug, Eq, PartialEq)]
        struct IssuanceState {
            revision: u64,
            published: Option<u64>,
            consumed: bool,
            issued: bool,
            acquired: bool,
            acquiring: bool,
            control_stop: Option<u64>,
            held: bool,
            signals: usize,
            mutation: bool,
        }

        fn issuance_state(stopped: &Stopped) -> IssuanceState {
            let state = stopped.1.event().event().source.lock();
            IssuanceState {
                revision: state.revision,
                published: state.published,
                consumed: state.consumed,
                issued: state.issued,
                acquired: state.acquired,
                acquiring: state.acquiring,
                control_stop: state.control_stop,
                held: state.hold.is_some(),
                signals: state.signals,
                mutation: state.mutation.is_some(),
            }
        }

        fn untraced_child_stop() -> (ExactChildGuard, Stopped) {
            let parent = unsafe { libc::getpid() };
            let pid = unsafe { libc::fork() };
            assert!(pid >= 0);
            if pid == 0 {
                unsafe {
                    if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) != 0 {
                        libc::_exit(121);
                    }
                    if libc::getppid() != parent {
                        libc::_exit(122);
                    }
                    // Deliberately NO TRACEME. This is an ordinary Linux stop.
                    libc::raise(libc::SIGSTOP);
                    libc::_exit(0);
                }
            }
            let running = Running::new(Pid::from_raw(pid));
            let cleanup = ExactChildGuard(running.terminal_cleanup());
            match running.wait().expect("consume real untraced child stop") {
                Wait::Stopped(stopped, crate::Event::Signal(Signal::SIGSTOP)) => (cleanup, stopped),
                other => panic!("unexpected untraced child state {other:?}"),
            }
        }

        fn untraced_issuance(control: bool) -> (Result<(), Errno>, IssuanceState, IssuanceState) {
            let (cleanup, stopped) = untraced_child_stop();
            let before = issuance_state(&stopped);
            let result = if control {
                stopped
                    .control_stop()
                    .and_then(|stop| stop.hold().map(drop))
            } else {
                stopped.source_stop().map(drop)
            };
            let after = issuance_state(&stopped);
            let identity = Arc::clone(stopped.1.event().identity().unwrap());
            let pid = stopped.pid();
            // Ordinary continuation uses the original pidfd, NOT ptrace CONT
            // or a new PID lookup. It proves refusal did not change Linux wait.
            let continued = unsafe {
                libc::syscall(
                    libc::SYS_pidfd_send_signal,
                    identity.pidfd.as_raw_fd(),
                    libc::SIGCONT,
                    std::ptr::null::<libc::siginfo_t>(),
                    0,
                )
            };
            if continued != 0 {
                let error = io::Error::last_os_error();
                cleanup
                    .cleanup()
                    .expect("cleanup after original SIGCONT refusal");
                panic!("original pidfd SIGCONT failed: {error}");
            }
            let terminal = futures::executor::block_on(stopped.wait_owned());
            cleanup
                .cleanup()
                .expect("join/reap original untraced child");
            let reaped = cleanup.0.is_reaped().unwrap();
            drop(cleanup);
            // The intended negative oracles are in the callers, AFTER this
            // real normal exit and cleanup. Old/mutant success is not cleanup.
            assert!(matches!(terminal,
                Ok(Wait::Exited(actual, crate::ExitStatus::Exited(0))) if actual == pid));
            assert!(
                reaped,
                "original untraced child must be reaped before oracle"
            );
            eprintln!("STOP_KIND_ORIGINAL_CLEANUP pid={pid} exit=0 reaped=true");
            (result, before, after)
        }

        #[test]
        fn actual_untraced_job_stop_refuses_source_issuance() {
            let (result, before, after) = untraced_issuance(false);
            assert_eq!(result, Err(Errno::EPERM), "untraced stop issued SourceStop");
            assert_eq!(
                after, before,
                "source refusal changed original issuance state"
            );
        }

        #[test]
        fn actual_untraced_job_stop_refuses_control_hold() {
            let (result, before, after) = untraced_issuance(true);
            assert_eq!(
                result,
                Err(Errno::EPERM),
                "untraced stop issued ControlHold"
            );
            assert_eq!(
                after, before,
                "control refusal changed original issuance state"
            );
        }

        #[test]
        fn actual_ptrace_stop_keeps_source_and_control_neighbor() {
            let (cleanup, stopped) = child_stop();
            let source = stopped.source_stop().and_then(|source| {
                let acquisition = source.begin_acquisition()?;
                drop(acquisition);
                source.validate_current()
            });
            let control = stopped.control_stop().and_then(|stop| {
                let hold = stop.hold()?;
                hold.validate()
            });
            let pid = stopped.pid();
            finish(stopped);
            cleanup
                .cleanup()
                .expect("join/reap original ptraced neighbor");
            let reaped = cleanup.0.is_reaped().unwrap();
            drop(cleanup);
            assert!(
                reaped,
                "original ptraced neighbor must be reaped before oracle"
            );
            eprintln!("STOP_KIND_ORIGINAL_CLEANUP pid={pid} ptraced=true reaped=true");
            assert_eq!(source, Ok(()), "real ptrace source neighbor refused");
            assert_eq!(control, Ok(()), "real ptrace control neighbor refused");
        }
    }
}
