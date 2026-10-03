/* Copyright (c) Meta Platforms, Inc. and affiliates. All rights reserved.
 * Licensed under the BSD-style license in the root LICENSE file. */
//! Separately proved direct-executable source. Anonymous predicates stay literal.
//! AUTONOMOUS-BOT-IMPLEMENTED; TODO-HUMAN-REVIEW:
//! https://github.com/rrnewton/reverie/pull/897

use std::marker::PhantomPinned;
use std::pin::Pin;
use std::sync::Arc;

use reverie_memory::ArmedExecutableSource;
use reverie_memory::ExecutableBackingGeometry;
use reverie_memory::ExecutableCaptureIssuer;
use reverie_memory::ExecutableCaptureRequest;
use reverie_memory::ExecutableCaptureVerifier;
use reverie_memory::ExecutableSourceArmer;
use reverie_memory::ExecutableSourceCapture;

use super::*;

// Controlled transport-result faults after genuine register syscalls. These
// test-only observations are never reported as kernel-produced failures.
#[cfg(cohort_final_test)]
mod capture_fault {
    use super::*;
    thread_local! {
        static NEXT: std::cell::Cell<u8> = const { std::cell::Cell::new(0) };
        static APPLIED: std::cell::Cell<u8> = const { std::cell::Cell::new(0) };
    }
    pub(super) fn install(fault: u8) {
        assert!((1..=4).contains(&fault));
        NEXT.with(|next| assert_eq!(next.replace(fault), 0));
        APPLIED.with(|applied| applied.set(0));
    }
    pub(super) fn applied() -> u8 {
        NEXT.with(|next| assert_eq!(next.get(), 0));
        APPLIED.with(|applied| applied.take())
    }
    fn take(expected: u8) -> bool {
        NEXT.with(|next| {
            if next.get() != expected {
                return false;
            }
            next.set(0);
            APPLIED.with(|applied| assert_eq!(applied.replace(expected), 0));
            true
        })
    }
    pub(super) fn prstatus(
        raw: Result<usize, Error>,
        iovec: &mut NativeIovec,
    ) -> Result<usize, Error> {
        if take(1) {
            assert_eq!(raw, Ok(0), "genuine GET precedes controlled error");
            return Err(refused(Refusal::TargetState(Errno::EIO)));
        }
        if take(2) {
            assert_eq!(raw, Ok(0));
            assert_eq!(iovec.length, PRSTATUS_BYTES);
            iovec.length = PRSTATUS_BYTES - 1;
        }
        raw
    }
    pub(super) fn xstate(state: Result<crate::XState, Error>) -> Result<crate::XState, Error> {
        if take(3) {
            assert!(state.is_ok(), "genuine XSTATE precedes controlled error");
            return Err(refused(Refusal::TargetState(Errno::EIO)));
        }
        if take(4) {
            let mut state = state.expect("genuine XSTATE precedes controlled truncation");
            assert!(state.0.len() >= 576);
            state.0.truncate(511);
            return Ok(state);
        }
        state
    }
}

// Integer words retain ABI addresses without granting worker dereference rights.
#[repr(C)]
struct NativeIovec {
    base: usize,
    length: usize,
}
const _: () = {
    assert!(std::mem::size_of::<NativeIovec>() == std::mem::size_of::<libc::iovec>());
    assert!(std::mem::align_of::<NativeIovec>() == std::mem::align_of::<libc::iovec>());
    assert!(std::mem::offset_of!(NativeIovec, base) == std::mem::offset_of!(libc::iovec, iov_base));
    assert!(
        std::mem::offset_of!(NativeIovec, length) == std::mem::offset_of!(libc::iovec, iov_len)
    );
    assert!(PRSTATUS_BYTES == 216);
};

struct CaptureFrame {
    registers: [u64; 27],
    iovec: NativeIovec,
    _pin: PhantomPinned,
}
impl CaptureFrame {
    fn allocate() -> Pin<Box<Self>> {
        let mut frame = Box::pin(Self {
            registers: [0; 27],
            iovec: NativeIovec {
                base: 0,
                length: PRSTATUS_BYTES,
            },
            _pin: PhantomPinned,
        });
        let base = frame.as_ref().get_ref().registers.as_ptr() as usize;
        // The allocation is pinned before installing its own buffer address.
        // No path moves a field or exposes a mutable frame outside this module.
        unsafe { frame.as_mut().get_unchecked_mut() }.iovec.base = base;
        frame
    }
    fn request(
        &self,
        target_tid: i32,
        ptracer_tid: i32,
        address: usize,
        length: usize,
    ) -> ExecutableCaptureRequest {
        ExecutableCaptureRequest {
            target_tid,
            ptracer_tid,
            source_address: address,
            source_length: length,
            iovec_address: &self.iovec as *const NativeIovec as usize,
            register_buffer_address: self.registers.as_ptr() as usize,
        }
    }
    fn capture(
        self: Pin<&mut Self>,
        target: &Stopped,
        request: ExecutableCaptureRequest,
    ) -> Result<(), Error> {
        // This exclusive borrow lasts through the synchronous kernel write.
        let frame = unsafe { self.get_unchecked_mut() };
        if target.pid().as_raw() != request.target_tid
            || current_thread()? != request.ptracer_tid
            || frame.iovec.base != request.register_buffer_address
            || frame.iovec.base != frame.registers.as_mut_ptr() as usize
            || frame.iovec.length != PRSTATUS_BYTES
            || &frame.iovec as *const NativeIovec as usize != request.iovec_address
        {
            return Err(refused(Refusal::TargetState(Errno::ESTALE)));
        }
        note(Step::Prstatus);
        let raw = unsafe {
            syscalls::syscall!(
                syscalls::Sysno::ptrace,
                libc::PTRACE_GETREGSET,
                target.pid().as_raw(),
                libc::NT_PRSTATUS,
                &mut frame.iovec as *mut NativeIovec
            )
        }
        .map_err(|e| refused(Refusal::TargetState(e)));
        #[cfg(cohort_final_test)]
        let raw = capture_fault::prstatus(raw, &mut frame.iovec);
        let raw = raw?;
        if raw != 0 || frame.iovec.length != PRSTATUS_BYTES {
            return Err(refused(Refusal::RegisterShape(frame.iovec.length)));
        }
        if frame.iovec.base != request.register_buffer_address
            || frame.registers[PRSTATUS_CS / 8] != 0x33
        {
            return Err(refused(Refusal::UnsupportedPlatform));
        }
        Ok(())
    }
}

/// Captured only on the original ptracer, read only on the registered worker.
/// The backend independently owns the complete cohort through true worker join.
pub struct FollowedExecutableSourceReadPlan {
    hold: Arc<crate::ControlHold>,
    observation: RegisterObservation,
    frame: Arc<Pin<Box<CaptureFrame>>>,
    collector: Box<dyn ArmedExecutableSource>,
    capture: ExecutableSourceCapture,
    verifier: ExecutableCaptureVerifier,
}
impl FollowedExecutableSourceReadPlan {
    /// Controlled transport failure after genuine GET (1/error, 2/short) or
    /// XSTATE (3/error, 4/short). No production authority is supplied.
    #[cfg(cohort_final_test)]
    pub fn set_capture_fault_for_test(fault: u8) {
        capture_fault::install(fault);
    }

    /// Confirms the exact configured fault ran; cannot skip a missing boundary.
    #[cfg(cohort_final_test)]
    pub fn take_applied_capture_fault_for_test() -> u8 {
        capture_fault::applied()
    }

    /// Prepare the actual executable register capture on the original ptracer.
    ///
    /// # Safety
    /// The caller owns the complete authenticated physically stopped cohort
    /// containing this sender. Before this call it installs an unconditional
    /// sticky fatal guard covering armer error/unwind, capture, registration,
    /// cancellation and true worker join. It retains the complete cohort and
    /// `keep_capture_allocation()` in the original SourceJobs ownership through
    /// genuine OS join. The armer independently retains H command/ACK debt.
    /// A sender-only ControlHold cannot discharge these outer obligations.
    ///
    /// ```compile_fail
    /// use std::sync::Arc;
    /// use safeptrace::{ControlHold, FollowedExecutableSourceReadPlan};
    /// use reverie_memory::ExecutableSourceArmer;
    /// fn missing_outer_obligation(hold: Arc<ControlHold>, armer: Box<dyn ExecutableSourceArmer>) {
    ///     let _ = FollowedExecutableSourceReadPlan::prepare(hold, 0x1000, 5, armer);
    /// }
    /// ```
    pub unsafe fn prepare(
        hold: Arc<crate::ControlHold>,
        address: usize,
        length: usize,
        armer: Box<dyn ExecutableSourceArmer>,
    ) -> Result<Self, Error> {
        let end = range_end(address, length)?;
        if unsafe { libc::sysconf(libc::_SC_PAGESIZE) } != PAGE as libc::c_long {
            return Err(refused(Refusal::UnsupportedPlatform));
        }
        let mut frame = CaptureFrame::allocate();
        let (observation, collector, capture, verifier) = {
            let reservation = hold
                .begin_register_capture()
                .map_err(|e| refused(Refusal::TargetState(e)))?;
            let target = reservation.stopped();
            if hold.expected_tid() <= 0 || target.pid().as_raw() != hold.expected_tid() {
                return Err(refused(Refusal::WrongTask));
            }
            let request = frame.request(hold.expected_tid(), current_thread()?, address, length);
            // The actual whole-hold sender reservation and stable frame exist.
            let issuer = unsafe { ExecutableCaptureIssuer::new_backend(request) };
            // Positive actual ARM must complete synchronously, with no R mutex
            // held and no dependence on guest/Tokio progress. No fire-and-forget.
            let collector = armer.arm(&issuer.challenge())?;
            hold.validate()
                .map_err(|e| refused(Refusal::TargetState(e)))?;
            frame.as_mut().capture(target, request)?;
            let layout = native_pkru_layout()
                .map_err(|_| refused(Refusal::UnsupportedPlatform))?
                .ok_or_else(|| refused(Refusal::UnsupportedPlatform))?;
            note(Step::Xstate);
            let state = target.getxstate().map_err(target_error);
            #[cfg(cohort_final_test)]
            let state = capture_fault::xstate(state);
            let state = state?;
            let pkru = decode_native_pkru_xstate(&state.0, layout)
                .map_err(|_| refused(Refusal::UnsupportedPlatform))?;
            hold.validate()
                .map_err(|e| refused(Refusal::TargetState(e)))?;
            let observation = RegisterObservation {
                tid: hold.expected_tid(),
                ptracer_thread: std::thread::current().id(),
                address,
                end,
                length,
                pkru,
            };
            // Exact actual native capture and XSTATE completed in this same
            // reservation; the plan now retains allocation and held authority.
            let (capture, verifier) = unsafe { issuer.completed() };
            (observation, collector, capture, verifier)
        };
        Ok(Self {
            hold,
            observation,
            frame: Arc::new(frame),
            collector,
            capture,
            verifier,
        })
    }

    /// Retain the immutable real capture allocation in the original SourceJobs
    /// retention slot through TRUE OS join, beyond the worker closure's return.
    /// This opaque owner conveys no source or provider authority.
    pub fn keep_capture_allocation(&self) -> Box<dyn Send + Sync> {
        Box::new(Arc::clone(&self.frame))
    }

    /// Test observation only; retaining Weak cannot keep the allocation alive.
    #[cfg(cohort_final_test)]
    pub fn capture_allocation_observation(&self) -> std::sync::Weak<dyn Send + Sync> {
        let owner: Arc<dyn Send + Sync> = self.frame.clone();
        Arc::downgrade(&owner)
    }

    /// Run on the registered SourceJobs worker; this never retires backend or H
    /// command custody. The original allocation stays alive through collection.
    pub fn run(self) -> Result<Vec<u8>, Error> {
        let Self {
            hold,
            observation,
            frame,
            collector,
            capture,
            verifier,
        } = self;
        hold.validate_single_source_filter()
            .map_err(|e| refused(Refusal::TargetState(e)))?;
        let proof = collector.collect(capture)?;
        let geometry = verifier.consume(proof)?;
        hold.validate()
            .map_err(|e| refused(Refusal::TargetState(e)))?;
        let length = observation.length;
        let bound = BoundRead::bind_executable(
            observation,
            hold.task_directory(),
            hold.task_directory_identity(),
            geometry,
        )?;
        let mut bytes = vec![0; length];
        bound.read_exact(&mut bytes)?;
        hold.validate()
            .map_err(|e| refused(Refusal::TargetState(e)))?;
        // Retain the real capture buffers through worker closure completion.
        drop(frame);
        Ok(bytes)
    }
}

impl Mapping<'_> {
    fn executable_access(
        &self,
        pkru: u32,
        geometry: ExecutableBackingGeometry,
        address: usize,
        end: usize,
    ) -> Result<(), Error> {
        self.complete()?;
        let file_end = self
            .offset
            .checked_add(end.checked_sub(self.start).ok_or_else(metadata_error)?)
            .ok_or_else(metadata_error)?;
        if self.kernel_page != Some(PAGE)
            || self.mmu_page != Some(PAGE)
            || self.lazy_free != Some(0)
            || self.permissions[1] != b'-'
            || self.permissions[3] != b'p'
            || self.inode == 0
            || self.device == (0, 0)
            || self.start != geometry.vma_start
            || self.end != geometry.vma_end
            || self.offset != geometry.file_offset
            || !self.offset.is_multiple_of(PAGE)
            || self.device != (geometry.device_major, geometry.device_minor)
            || self.inode != geometry.inode
            || geometry.file_size < file_end
            || address < self.start
            || end > self.end
            || address >= end
        {
            return Err(refused(Refusal::UnsupportedBacking));
        }
        for flag in self.flags.as_ref().unwrap() {
            if !matches!(
                *flag,
                "rd" | "ex"
                    | "mr"
                    | "mw"
                    | "me"
                    | "lo"
                    | "lf"
                    | "sr"
                    | "rr"
                    | "dc"
                    | "ac"
                    | "nr"
                    | "dd"
                    | "sd"
                    | "hg"
                    | "nh"
            ) {
                return Err(refused(Refusal::UnsupportedMapping));
            }
        }
        if self.permissions[0] != b'r' {
            return Err(refused(Refusal::UnsupportedMapping));
        }
        let key = self.key.unwrap();
        if pkru & (1u32 << (2 * key)) != 0 {
            return Err(Error::Fault(Fault::ProtectionKey(key)));
        }
        Ok(())
    }
}

impl BoundRead {
    fn bind_executable(
        observation: RegisterObservation,
        original_directory: BorrowedFd<'_>,
        original_identity: (u64, u64),
        geometry: ExecutableBackingGeometry,
    ) -> Result<Self, Error> {
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
        // Same original complete hold spans both opens, permission checks and
        // pread. A directory alone would pin struct pid rather than this MM.
        let mem = open_relative(original_directory, mount, c"mem")?;
        note(Step::MemBound);
        let mut smaps = open_relative(original_directory, mount, c"smaps")?;
        let bytes = bounded_read(&mut smaps, MAX_SMAPS)?;
        let map = mapping(&bytes, observation.address, observation.end)?;
        map.executable_access(
            observation.pkru,
            geometry,
            observation.address,
            observation.end,
        )?;
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
}

#[cfg(test)]
mod tests {
    use super::*;
    fn smaps(key: u8) -> String {
        format!(
            "1000-2000 r--p 00003000 00:2a 123 /controlled/executable\nKernelPageSize: 4 kB\nMMUPageSize: 4 kB\nLazyFree: 0 kB\nProtectionKey: {key}\nVmFlags: rd mr mw me sd\n"
        )
    }
    fn geometry() -> ExecutableBackingGeometry {
        ExecutableBackingGeometry {
            vma_start: 0x1000,
            vma_end: 0x2000,
            file_offset: 0x3000,
            device_major: 0,
            device_minor: 42,
            inode: 123,
            file_size: 0x4000,
        }
    }
    #[test]
    fn executable_mapping_requires_exact_geometry_and_still_refuses_anonymous_route() {
        let text = smaps(0);
        let map = mapping(text.as_bytes(), 0x1100, 0x1105).unwrap();
        assert_eq!(map.executable_access(0, geometry(), 0x1100, 0x1105), Ok(()));
        assert_eq!(
            map.read_access(0),
            Err(refused(Refusal::UnsupportedBacking))
        );
        for field in 0..7 {
            let mut g = geometry();
            match field {
                0 => g.vma_start += 4096,
                1 => g.vma_end += 4096,
                2 => g.file_offset += 4096,
                3 => g.device_major += 1,
                4 => g.device_minor += 1,
                5 => g.inode += 1,
                6 => g.file_size = 0x3104,
                _ => unreachable!(),
            }
            assert!(map.executable_access(0, g, 0x1100, 0x1105).is_err());
        }
    }
    #[test]
    fn executable_mapping_keeps_read_pkru_and_special_mapping_refusals() {
        for key in 0..16 {
            let text = smaps(key);
            let map = mapping(text.as_bytes(), 0x1100, 0x1105).unwrap();
            assert_eq!(
                map.executable_access(2 << (2 * key), geometry(), 0x1100, 0x1105),
                Ok(())
            );
            assert_eq!(
                map.executable_access(1 << (2 * key), geometry(), 0x1100, 0x1105),
                Err(Error::Fault(Fault::ProtectionKey(key)))
            );
        }
        for text in [
            smaps(0)
                .replace("r--p", "rw-p")
                .replace("rd mr", "rd wr mr"),
            smaps(0)
                .replace("r--p", "r--s")
                .replace("rd mr", "rd sh ms mr"),
            smaps(0).replace("rd mr", "rd io mr"),
            smaps(0).replace("rd mr", "rd uf mr"),
            smaps(0).replace("rd mr", "rd de mr"),
            smaps(0).replace("LazyFree: 0", "LazyFree: 4"),
            smaps(0).replace("KernelPageSize: 4", "KernelPageSize: 64"),
            smaps(0).replace("00:2a 123", "00:00 0"),
        ] {
            let map = mapping(text.as_bytes(), 0x1100, 0x1105).unwrap();
            assert!(
                map.executable_access(0, geometry(), 0x1100, 0x1105)
                    .is_err()
            );
        }
    }
}
