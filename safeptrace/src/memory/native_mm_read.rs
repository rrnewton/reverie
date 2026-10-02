/* Copyright (c) Meta Platforms, Inc. and affiliates. All rights reserved.
 * Licensed under the BSD-style license in the root LICENSE file. */

//! Private mechanics for a separately admitted, MM-descriptor-bound source read.
//!
//! This is NOT MemoryAccess::read_native_user_exact: proc-mem can use FOLL_FORCE.
//! Register, VMA and PKRU checks below remain mandatory permission evidence.
//! They do not issue a stop lease, exclude writers, or promise bounded kernel IO.
//! The production constructor consumes the notifier's real SourceAcquisition.
//! The backend must register/join the worker and validate original authority
//! before publishing its result. Component tests hold their exact child stopped;
//! they are not proof of backend admission or completion.

use std::ffi::CStr;

use super::*;

/// A permission-checked staged read under a retained physical control hold.
/// The backend must independently retain the WHOLE followed cohort through
/// true OS join. This type is not a source/publication certificate.
#[cfg(feature = "notifier")]
pub struct FollowedSourceReadPlan {
    hold: std::sync::Arc<crate::ControlHold>,
    observation: RegisterObservation,
    #[cfg(cohort_final_test)]
    pause: Option<(
        std::sync::Arc<std::sync::atomic::AtomicBool>,
        std::sync::mpsc::Receiver<()>,
    )>,
}

#[cfg(feature = "notifier")]
impl FollowedSourceReadPlan {
    /// Capture the sender's native registers and PKRU at its authentic stop.
    pub fn prepare(
        hold: std::sync::Arc<crate::ControlHold>,
        address: usize,
        length: usize,
    ) -> Result<Self, Error> {
        let observation = {
            let capture = hold
                .begin_register_capture()
                .map_err(|e| refused(Refusal::TargetState(e)))?;
            RegisterObservation::capture(capture.stopped(), hold.expected_tid(), address, length)?
        };
        Ok(Self {
            hold,
            observation,
            #[cfg(cohort_final_test)]
            pause: None,
        })
    }

    /// Native-regression pause at the real post-binding, pre-pread boundary.
    /// This is observation only and issues no source authority.
    #[cfg(cohort_final_test)]
    pub fn pause_before_read(
        mut self,
        entered: std::sync::Arc<std::sync::atomic::AtomicBool>,
        release: std::sync::mpsc::Receiver<()>,
    ) -> Self {
        self.pause = Some((entered, release));
        self
    }

    /// Execute on the original registered source worker. The control hold is
    /// retained across BOTH binding and pread; this cannot release the backend's
    /// independently retained cohort or acknowledge worker retirement.
    pub fn run(self) -> Result<Vec<u8>, Error> {
        self.hold
            .validate_single_source_filter()
            .map_err(|e| refused(Refusal::TargetState(e)))?;
        let length = self.observation.length;
        let bound = BoundRead::bind(
            self.observation,
            self.hold.task_directory(),
            self.hold.task_directory_identity(),
        )?;
        #[cfg(cohort_final_test)]
        if let Some((entered, release)) = self.pause {
            entered.store(true, std::sync::atomic::Ordering::Release);
            release
                .recv_timeout(std::time::Duration::from_secs(5))
                .map_err(|_| refused(Refusal::TargetState(Errno::ETIMEDOUT)))?;
        }
        let mut bytes = vec![0; length];
        bound.read_exact(&mut bytes)?;
        self.hold
            .validate()
            .map_err(|e| refused(Refusal::TargetState(e)))?;
        Ok(bytes)
    }
}

/// One separately admitted MM-bound read, prepared on the original ptracer
/// thread and executed on its registered worker. It never implements the
/// synchronous `MemoryAccess::read_native_user_exact` transport.
///
/// No descriptor/number/Boolean constructor is available. The acquisition owns
/// the actual consumed-stop association and original task directory. The
/// backend must retain its source epoch and caller keepalive through true worker
/// join, then revalidate the original stop/epoch/cancellation before publication.
/// Source mapping/data/history exclusion is still a caller/backend prerequisite;
/// these observations do not enforce it or decide a consuming syscall's errno.
#[cfg(feature = "notifier")]
pub struct NativeSourceReadPlan {
    acquisition: crate::SourceAcquisition,
    observation: RegisterObservation,
}

#[cfg(feature = "notifier")]
impl NativeSourceReadPlan {
    // Test-only access to the existing hooks at the actual register syscalls.
    // No production callback can acquire or prolong the consumption ticket.
    #[cfg(test)]
    pub(crate) fn set_capture_hook_for_test(mut hook: impl FnMut(u8) + 'static) {
        hooks::set(move |step| match step {
            Step::Prstatus => hook(0),
            Step::Xstate => hook(1),
            _ => {}
        });
    }

    #[cfg(test)]
    pub(crate) fn capture_steps_for_test() -> Vec<u8> {
        hooks::take()
            .into_iter()
            .map(|step| match step {
                Step::Prstatus => 0,
                Step::Xstate => 1,
                _ => 2,
            })
            .collect()
    }

    /// Captures native PRSTATUS and XSTATE/PKRU on the acquisition's committed-wait
    /// return thread. Kernel ptracer authorization is a separate check on each
    /// register read. No proc acquisition or source IO occurs.
    pub fn prepare(
        acquisition: crate::SourceAcquisition,
        address: usize,
        length: usize,
    ) -> Result<Self, Error> {
        let tid = acquisition.expected_tid();
        let observation = {
            let capture = acquisition
                .begin_register_capture()
                .map_err(|error| refused(Refusal::TargetState(error)))?;
            RegisterObservation::capture(capture.stopped(), tid, address, length)?
        };
        Ok(Self {
            acquisition,
            observation,
        })
    }

    /// Acquires both MM files and checks metadata under the SAME actual
    /// acquisition interlock, then performs one privately staged bounded pread.
    /// The returned bytes are a worker result, not authority to publish or a
    /// receipt that the actual worker joined. No host deadline is created here.
    pub fn run(self) -> Result<Vec<u8>, Error> {
        let Self {
            acquisition,
            observation,
        } = self;
        let length = observation.length;
        let bound = BoundRead::bind(
            observation,
            acquisition.task_directory(),
            acquisition.task_directory_identity(),
        )?;
        // Both owned MM handles and the complete permission observation exist
        // before releasing control exclusion. On bind failure, dropping the
        // acquisition happens only after the bind call has actually returned.
        acquisition.finish_binding();
        let mut bytes = vec![0; length];
        bound.read_exact(&mut bytes)?;
        Ok(bytes)
    }
}

// Private mechanical observations, never raw-FD admission constructors.
#[derive(Debug)]
pub(super) struct RegisterObservation {
    tid: i32,
    ptracer_thread: std::thread::ThreadId,
    address: usize,
    end: usize,
    length: usize,
    pkru: u32,
}

impl RegisterObservation {
    /// Evidence only; production holds the private register-capture reservation
    /// obtained from the acquisition or control hold across both register reads.
    /// In particular Stopped::new_unchecked is not an acquisition issuer.
    pub(super) fn capture(
        target: &Stopped,
        expected_tid: i32,
        address: usize,
        length: usize,
    ) -> Result<Self, Error> {
        if expected_tid <= 0 || target.pid().as_raw() != expected_tid {
            return Err(refused(Refusal::WrongTask));
        }
        let end = range_end(address, length)?;
        if unsafe { libc::sysconf(libc::_SC_PAGESIZE) } != PAGE as libc::c_long {
            return Err(refused(Refusal::UnsupportedPlatform));
        }
        let ptracer_thread = std::thread::current().id();
        note(Step::Prstatus);
        validate_native_mode(target)?;
        let layout = native_pkru_layout()
            .map_err(|_| refused(Refusal::UnsupportedPlatform))?
            .ok_or_else(|| refused(Refusal::UnsupportedPlatform))?;
        note(Step::Xstate);
        let state = target.getxstate().map_err(target_error)?;
        let pkru = decode_native_pkru_xstate(&state.0, layout)
            .map_err(|_| refused(Refusal::UnsupportedPlatform))?;
        Ok(Self {
            tid: expected_tid,
            ptracer_thread,
            address,
            end,
            length,
            pkru,
        })
    }
}

fn current_thread() -> Result<i32, Error> {
    Errno::result(unsafe { libc::syscall(libc::SYS_gettid) })
        .map(|tid| tid as i32)
        .map_err(|e| refused(Refusal::TargetState(e)))
}

fn open_relative(directory: BorrowedFd<'_>, mount: ProcMount, name: &CStr) -> Result<File, Error> {
    note(Step::RelativeOpen);
    let fd = Errno::result(unsafe {
        libc::openat(
            directory.as_raw_fd(),
            name.as_ptr(),
            libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
        )
    })
    .map_err(|e| refused(Refusal::Procfs(e)))?;
    let file = unsafe { File::from_raw_fd(fd) };
    verify_proc(&file, Some(mount))?;
    Ok(file)
}

pub(super) fn bounded_read(file: &mut File, limit: usize) -> Result<Vec<u8>, Error> {
    note(Step::MetadataRead);
    let mut bytes = Vec::new();
    file.take((limit + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(proc_error)?;
    if bytes.len() > limit {
        return Err(refused(Refusal::MetadataTooLarge));
    }
    Ok(bytes)
}

fn directory_identity(directory: BorrowedFd<'_>, expected: (u64, u64)) -> Result<(), Error> {
    let mut metadata = std::mem::MaybeUninit::<libc::stat>::uninit();
    Errno::result(unsafe { libc::fstat(directory.as_raw_fd(), metadata.as_mut_ptr()) })
        .map_err(|e| refused(Refusal::Procfs(e)))?;
    let metadata = unsafe { metadata.assume_init() };
    if metadata.st_mode & libc::S_IFMT != libc::S_IFDIR
        || (metadata.st_dev, metadata.st_ino) != expected
    {
        return Err(refused(Refusal::ProcfsViewMismatch));
    }
    Ok(())
}

// The worker must have a single NSpid in this proc mount (proc_view above).
// The target may live in descendant PID namespaces: its FIRST NSpid is the
// identity in this same mount. Requiring only one target entry would wrongly
// refuse an otherwise ordinary container. This observation cannot issue a
// task association or prevent exec; the original directory/interlock must.
pub(super) fn target_proc_view(status: &[u8], tid: usize) -> Result<(), Error> {
    let invalid = || refused(Refusal::ProcfsViewMismatch);
    if !status.ends_with(b"\n") {
        return Err(invalid());
    }
    let text = std::str::from_utf8(status).map_err(|_| invalid())?;
    let mut pid = None;
    let mut nspid = None;
    for line in text.lines() {
        if let Some(value) = line.strip_prefix("Pid:") {
            let value = decimal(value.trim_ascii()).map_err(|_| invalid())?;
            if pid.replace(value).is_some() {
                return Err(invalid());
            }
        }
        if let Some(value) = line.strip_prefix("NSpid:") {
            let mut ids = value.split_ascii_whitespace();
            let first = decimal(ids.next().ok_or_else(invalid)?).map_err(|_| invalid())?;
            if nspid.replace(first).is_some() {
                return Err(invalid());
            }
            for id in ids {
                if decimal(id).map_err(|_| invalid())? == 0 {
                    return Err(invalid());
                }
            }
        }
    }
    if tid == 0 || pid != Some(tid) || nspid != Some(tid) {
        return Err(invalid());
    }
    Ok(())
}

/// Owns the MM files, not admission or completion authority. No Clone.
#[derive(Debug)]
pub(super) struct BoundRead {
    mem: File,
    _smaps: File,
    worker_thread: std::thread::ThreadId,
    address: usize,
    length: usize,
}

impl BoundRead {
    /// The production bridge must hold ONE real no-replacement acquisition
    /// interval across this entire call. Matching inode/TID observations are
    /// checks within that interval, never a substitute for it.
    pub(super) fn bind(
        observation: RegisterObservation,
        original_directory: BorrowedFd<'_>,
        original_identity: (u64, u64),
    ) -> Result<Self, Error> {
        // All potentially blocking proc acquisition/metadata work stays off
        // the original ptracer thread, even opening /proc or the mem file.
        let worker = current_thread()?;
        let worker_thread = std::thread::current().id();
        if worker_thread == observation.ptracer_thread {
            return Err(refused(Refusal::TargetState(Errno::EPERM)));
        }
        note(Step::Acquisition);
        directory_identity(original_directory, original_identity)?;
        note(Step::RootOpen);
        let root = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW)
            .open("/proc")
            .map_err(proc_error)?;
        let mount = verify_proc(&root, None)?;
        verify_proc_fd(original_directory, Some(mount))?;
        let mut worker_status = open_relative(root.as_fd(), mount, c"thread-self/status")?;
        proc_view(
            &bounded_read(&mut worker_status, MAX_STATUS)?,
            worker as usize,
        )?;
        let mut target_status = open_relative(original_directory, mount, c"status")?;
        target_proc_view(
            &bounded_read(&mut target_status, MAX_STATUS)?,
            observation.tid as usize,
        )?;

        // Both opens must be under the SAME actual control interlock. Each
        // proc open acquires an mm independently; a directory pins struct pid,
        // not mm. Never release the interval between these calls.
        let mem = open_relative(original_directory, mount, c"mem")?;
        note(Step::MemBound);
        let mut smaps = open_relative(original_directory, mount, c"smaps")?;
        let bytes = bounded_read(&mut smaps, MAX_SMAPS)?;
        let map = mapping(&bytes, observation.address, observation.end)?;
        // FOLL_FORCE/FOLL_REMOTE can bypass these permissions. An owned mem
        // descriptor does not prove ordinary access or alter error precedence.
        map.read_access(observation.pkru)?;
        directory_identity(original_directory, original_identity)?;
        note(Step::Bound);
        Ok(Self {
            mem,
            _smaps: smaps,
            worker_thread,
            address: observation.address,
            length: observation.length,
        })
    }

    /// Exactly one pread; caller publication also needs true worker join,
    /// original cancellation/deadline state, and current original authority.
    /// Mapping and source stability must persist through this read. An exec/reap may
    /// empty the old mm; no retry or numeric-identity reacquisition is allowed.
    pub(super) fn read_exact(self, output: &mut [u8]) -> Result<(), Error> {
        if std::thread::current().id() != self.worker_thread {
            return Err(refused(Refusal::TargetState(Errno::EPERM)));
        }
        if output.len() != self.length {
            return Err(refused(Refusal::UnsupportedRange));
        }
        let mut staged = [0u8; MAX_READ];
        note(Step::BeforeRead);
        note(Step::Pread);
        let result = Errno::result(unsafe {
            libc::pread(
                self.mem.as_raw_fd(),
                staged.as_mut_ptr().cast(),
                self.length,
                self.address as libc::off_t,
            )
        })
        .map(|count| count as usize);
        publish(result, &staged, output)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Step {
    Prstatus,
    Xstate,
    Acquisition,
    RootOpen,
    RelativeOpen,
    MetadataRead,
    MemBound,
    Bound,
    BeforeRead,
    Pread,
}

// Hooks occur at the real boundaries, not in a wrapper that can bypass IO.
// All hooks/counters are thread-local and disappear outside tests.
fn note(step: Step) {
    #[cfg(test)]
    hooks::note(step);
    #[cfg(not(test))]
    let _ = step;
}

#[cfg(test)]
pub(super) mod hooks {
    use super::Step;

    thread_local! {
        static TRACE: std::cell::RefCell<Vec<Step>> = const { std::cell::RefCell::new(Vec::new()) };
        static HOOK: std::cell::RefCell<Option<Box<dyn FnMut(Step)>>> = const { std::cell::RefCell::new(None) };
    }

    pub(super) fn note(step: Step) {
        TRACE.with(|trace| trace.borrow_mut().push(step));
        HOOK.with(|hook| {
            if let Some(hook) = hook.borrow_mut().as_mut() {
                hook(step);
            }
        });
    }

    pub(in super::super) fn take() -> Vec<Step> {
        TRACE.with(|trace| std::mem::take(&mut *trace.borrow_mut()))
    }

    pub(in super::super) fn set(hook: impl FnMut(Step) + 'static) {
        HOOK.with(|slot| *slot.borrow_mut() = Some(Box::new(hook)));
    }
}
