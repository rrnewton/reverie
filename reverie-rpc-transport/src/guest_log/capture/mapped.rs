//! Capture whose guest lifetime is established by an independent process owner.

use std::os::fd::OwnedFd;

use super::*;

/// Physical wait evidence, separate from producer FINISH and RPC retirement.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct MappedProcessReport {
    pub root_pid: Option<i32>,
    pub root_status: Option<i32>,
    pub admission_closed: bool,
    pub all_reaped: bool,
    pub abandoned: bool,
}

/// The single owner which records actual process disposition for a capture set.
/// This does not spawn, signal, wait for, or otherwise supervise a process.
pub struct MappedProcessOwner {
    state: Arc<Mutex<MappedProcessReport>>,
}

/// Read-only process disposition shared by the independent output captures.
#[derive(Clone)]
pub struct MappedProcessObserver {
    state: Arc<Mutex<MappedProcessReport>>,
}

impl MappedProcessOwner {
    /// Create a lifetime record before starting any guest or capture worker.
    ///
    /// # Safety
    /// The caller owns all guest process creation and wait disposition, including
    /// births before runtime enrollment, and keeps that ownership through final
    /// cleanup. No other waiter or auto-reaping disposition may consume those
    /// statuses. The caller must retain this owner across cancellation; this
    /// record is not itself proof of kernel ownership or a general launcher.
    ///
    /// ```compile_fail,E0133
    /// use reverie_rpc_transport::guest_log::MappedProcessOwner;
    /// let _ = MappedProcessOwner::new();
    /// ```
    pub unsafe fn new() -> (Self, MappedProcessObserver) {
        let state = Arc::new(Mutex::new(MappedProcessReport::default()));
        (
            Self {
                state: state.clone(),
            },
            MappedProcessObserver { state },
        )
    }

    /// Record the root's actual consumed wait status, without closing admission.
    ///
    /// # Safety
    /// `pid` and the raw wait status are from the owned physical root's successful
    /// wait. A readiness observation, ECHILD, or a guest message is insufficient.
    /// This may only follow Command::spawn's complete ownership handoff.
    pub unsafe fn root_reaped(&mut self, pid: i32, status: i32) -> io::Result<()> {
        let mut state = self.state.lock().unwrap();
        if pid <= 0
            || !(libc::WIFEXITED(status) || libc::WIFSIGNALED(status))
            || state.root_status.is_some()
            || state.all_reaped
        {
            return Err(io::Error::other("mapped root wait recorded out of order"));
        }
        state.root_pid = Some(pid);
        state.root_status = Some(status);
        Ok(())
    }

    /// Record closure of every host-controlled source of new guest processes.
    ///
    /// This does not claim that existing guests or pending forks are gone.
    pub fn close_admission(&mut self) {
        self.state.lock().unwrap().admission_closed = true;
    }

    /// Record the final actual absence of all owned descendants.
    ///
    /// # Safety
    /// After closing spawn/setup admission, the sole process-wide wait owner
    /// obtained ECHILD with the full admitted wait class (including __WALL where
    /// required), retained the real root status, and ruled out in-progress host
    /// spawns and competing waiters. All admitted processes are physically gone;
    /// a PID list, group kill, FINISH, setup EOF or RPC-idle count is insufficient.
    /// No producer may subsequently run in this lifetime. RPC/worker/publication
    /// completion must still be established independently.
    pub unsafe fn all_reaped(&mut self) -> io::Result<()> {
        let mut state = self.state.lock().unwrap();
        if !state.admission_closed || state.root_status.is_none() || state.all_reaped {
            return Err(io::Error::other(
                "mapped descendant wait recorded out of order",
            ));
        }
        state.all_reaped = true;
        Ok(())
    }
}

impl Drop for MappedProcessOwner {
    fn drop(&mut self) {
        let mut state = self.state.lock().unwrap();
        if !state.all_reaped {
            state.abandoned = true;
        }
    }
}

impl MappedProcessObserver {
    pub fn snapshot(&self) -> MappedProcessReport {
        self.state.lock().unwrap().clone()
    }

    pub(super) fn closed(&self) -> io::Result<bool> {
        let state = self.state.lock().unwrap();
        if state.abandoned {
            return Err(io::Error::other(
                "mapped process owner lost before actual reap",
            ));
        }
        Ok(state.all_reaped)
    }
}

/// A mapped capture keeps the original capture fields and physical process
/// facts separate. Its embedded socket report never claims peer closure.
#[derive(Clone, Debug)]
pub struct MappedCaptureReport {
    pub capture: CaptureReport,
    pub processes: MappedProcessReport,
}

impl MappedCaptureReport {
    pub fn qualifies(&self) -> bool {
        self.processes.root_pid.is_some()
            && self.processes.root_status.is_some()
            && self.processes.admission_closed
            && self.processes.all_reaped
            && !self.processes.abandoned
            && !self.capture.guest.peer_closed
            && self.capture.complete_except_socket_lifetime()
    }
}

pub struct MappedCaptureOwner {
    capture: CaptureOwner,
    processes: MappedProcessObserver,
}

impl MappedCaptureOwner {
    pub fn handle(&self) -> LogHandle {
        self.capture.handle()
    }

    pub fn request_close_until(&self, deadline: Instant) {
        self.capture.request_close_until(deadline);
    }

    pub fn snapshot(&self) -> MappedCaptureReport {
        MappedCaptureReport {
            capture: self
                .capture
                .handle
                .capture_snapshot()
                .expect("prepared mapped capture"),
            processes: self.processes.snapshot(),
        }
    }

    pub fn finish_until(&mut self, deadline: Instant) -> MappedCaptureReport {
        let capture = self.capture.finish_until(deadline);
        MappedCaptureReport {
            capture,
            processes: self.processes.snapshot(),
        }
    }
}

/// Start the same ordered collector/publication pipeline with a direct mapping.
/// The returned descriptor is setup-only and supplies no process-lifetime signal.
///
/// # Safety
/// Every descriptor/mapping alias must obey the module's full-lifetime trusted
/// shared-memory contract. Guest producers use exclusive fork incarnations.
/// The supplied observer belongs to the actual independent process owner; its
/// record cannot be substituted for physical waits. Retain this capture through
/// emitter cleanup, and close both outputs before waiting on the same deadline.
///
/// ```compile_fail,E0133
/// use reverie_rpc_transport::guest_log as g;
/// fn requires_ownership<D: g::CaptureDestination>(options: g::CaptureOptions, destination: D, processes: g::MappedProcessObserver) {
///     let _ = g::prepared_mapped_capture(options, destination, processes);
/// }
/// ```
pub unsafe fn prepared_mapped_capture<D: CaptureDestination>(
    options: CaptureOptions,
    destination: D,
    processes: MappedProcessObserver,
) -> Result<(MappedCaptureOwner, OwnedFd, HostProducer), CaptureStartError> {
    validate_options(options)?;
    let deadline = Instant::now() + options.timeouts.startup;
    let (buffer, descriptor) = unsafe { ordered::Buffer::create(options.limits.ordered()) }?;
    let (capture, mut startup, producer) = prepare_buffer(
        options,
        deadline,
        destination,
        buffer,
        GuestLifetime::Mapped(processes.clone()),
        |worker| {
            std::thread::Builder::new()
                .name("capture-collector".into())
                .spawn(worker)
        },
    )?;
    // The returned capture is the startup/cleanup owner; no socket sink escapes.
    startup.transferred = true;
    Ok((
        MappedCaptureOwner { capture, processes },
        descriptor,
        producer,
    ))
}
