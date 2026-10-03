/* Copyright (c) Meta Platforms, Inc. and affiliates. All rights reserved.
 * Licensed under the BSD-style license in the root LICENSE file. */
//! One exact held executable-source capture. Scalar observations are not proof.
//! AUTONOMOUS-BOT-IMPLEMENTED; TODO-HUMAN-REVIEW:
//! https://github.com/rrnewton/reverie/pull/897

use std::sync::Arc;

use syscalls::Errno;

use crate::NativeUserReadError as Error;
use crate::NativeUserReadRefusal as Refusal;

/// Numeric observations; constructing or copying these supplies no authority.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ExecutableCaptureRequest {
    pub target_tid: i32,
    pub ptracer_tid: i32,
    pub source_address: usize,
    pub source_length: usize,
    pub iovec_address: usize,
    pub register_buffer_address: usize,
}
impl ExecutableCaptureRequest {
    pub const fn ptrace_request(&self) -> usize {
        0x4204
    }
    pub const fn register_note(&self) -> usize {
        1
    }
    pub const fn register_bytes(&self) -> usize {
        216
    }
}

/// Borrowed only during synchronous positive installation acknowledgement.
pub struct ExecutableSourceChallenge<'a> {
    request: &'a ExecutableCaptureRequest,
}
impl ExecutableSourceChallenge<'_> {
    pub fn request(&self) -> ExecutableCaptureRequest {
        *self.request
    }
}

/// Plain geometry observation; neither this value nor its fields issue proof.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ExecutableBackingGeometry {
    pub vma_start: usize,
    pub vma_end: usize,
    pub file_offset: usize,
    pub device_major: usize,
    pub device_minor: usize,
    pub inode: usize,
    pub file_size: usize,
}

/// Backend-owned one-shot issuer. Safe callers cannot construct it.
pub struct ExecutableCaptureIssuer {
    request: ExecutableCaptureRequest,
    identity: Arc<()>,
}
impl ExecutableCaptureIssuer {
    /// # Safety
    /// The caller owns the original complete physically stopped cohort and the
    /// selected sender's same-generation register-capture reservation. The
    /// exact native PRSTATUS iovec and initialized 216-byte buffer already have
    /// stable retained addresses. They must remain allocated through native
    /// capture, registered collection and actual worker join. No mutex spans
    /// arbitrary armer code; the backend's sticky failure guard is already live.
    pub unsafe fn new_backend(request: ExecutableCaptureRequest) -> Self {
        Self {
            request,
            identity: Arc::new(()),
        }
    }
    pub fn challenge(&self) -> ExecutableSourceChallenge<'_> {
        ExecutableSourceChallenge {
            request: &self.request,
        }
    }
    /// # Safety
    /// Positive actual ARM preceded the one original-ptracer GETREGSET. That
    /// exact request completed raw0/full216/native mode, then genuine XSTATE and
    /// PKRU capture completed under the same original reservation. The complete
    /// cohort and original allocation remain retained; no unknown effect or
    /// cancellation is being promoted to completed capture.
    pub unsafe fn completed(self) -> (ExecutableSourceCapture, ExecutableCaptureVerifier) {
        let verifier = ExecutableCaptureVerifier {
            request: self.request,
            identity: Arc::clone(&self.identity),
        };
        let capture = ExecutableSourceCapture {
            request: self.request,
            identity: self.identity,
        };
        (capture, verifier)
    }
}

/// Genuine backend-completed capture, consumed once by the trusted collector.
pub struct ExecutableSourceCapture {
    request: ExecutableCaptureRequest,
    identity: Arc<()>,
}
impl ExecutableSourceCapture {
    pub fn request(&self) -> ExecutableCaptureRequest {
        self.request
    }
    /// # Safety
    /// The caller has authenticated the original installed/collected/retained
    /// ABI11 op26 command, actual target and tracer task identities, full
    /// original intent, exact PRSTATUS operands and native return, both actual
    /// VMA observations, current original vm_file==mm.exe_file, original non-HSM
    /// deny-write lifetime, supported pinned direct-Btrfs inode/dispatch/image,
    /// and exact range coverage. Geometry is the verified current backing,
    /// translated explicitly from raw kernel device encoding. Caller retains
    /// original command and any ACK debt independently; dropping these values
    /// never certifies command or semantic retirement.
    pub unsafe fn certify_direct_executable(
        self,
        geometry: ExecutableBackingGeometry,
    ) -> ExecutableBackingProof {
        ExecutableBackingProof {
            request: self.request,
            identity: self.identity,
            geometry,
        }
    }
}

/// Non-Clone proof; safe code cannot manufacture or copy its capture identity.
pub struct ExecutableBackingProof {
    request: ExecutableCaptureRequest,
    identity: Arc<()>,
    geometry: ExecutableBackingGeometry,
}

/// The original backend plan retains this verifier independently of collector.
pub struct ExecutableCaptureVerifier {
    request: ExecutableCaptureRequest,
    identity: Arc<()>,
}
impl ExecutableCaptureVerifier {
    pub fn consume(
        self,
        proof: ExecutableBackingProof,
    ) -> Result<ExecutableBackingGeometry, Error> {
        if self.request != proof.request || !Arc::ptr_eq(&self.identity, &proof.identity) {
            return Err(Error::Refused(Refusal::TargetState(Errno::ESTALE)));
        }
        Ok(proof.geometry)
    }
}

pub trait ExecutableSourceArmer: Send {
    /// Must return only after positive actual command installation ACK. The
    /// original Call/controller retains command debt independently of this box.
    fn arm(
        self: Box<Self>,
        challenge: &ExecutableSourceChallenge<'_>,
    ) -> Result<Box<dyn ArmedExecutableSource>, Error>;
}
pub trait ArmedExecutableSource: Send {
    /// Called only on the registered source worker, before proc binding/pread.
    /// An arbitrary safe implementation cannot manufacture backing authority.
    fn collect(
        self: Box<Self>,
        capture: ExecutableSourceCapture,
    ) -> Result<ExecutableBackingProof, Error>;
}

#[cfg(test)]
mod tests {
    use super::*;
    fn request() -> ExecutableCaptureRequest {
        ExecutableCaptureRequest {
            target_tid: 10,
            ptracer_tid: 20,
            source_address: 0x1000,
            source_length: 5,
            iovec_address: 0x2000,
            register_buffer_address: 0x3000,
        }
    }
    fn geometry() -> ExecutableBackingGeometry {
        ExecutableBackingGeometry {
            vma_start: 0x1000,
            vma_end: 0x2000,
            file_offset: 0,
            device_major: 0,
            device_minor: 41,
            inode: 123,
            file_size: 4096,
        }
    }
    // Controlled unsafe issuers test only the safe consumption boundary.
    #[test]
    fn executable_capture_equal_scalars_do_not_replace_identity() {
        let one = unsafe { ExecutableCaptureIssuer::new_backend(request()) };
        let two = unsafe { ExecutableCaptureIssuer::new_backend(request()) };
        let (_, expected) = unsafe { one.completed() };
        let (foreign, _) = unsafe { two.completed() };
        assert_eq!(
            expected.consume(unsafe { foreign.certify_direct_executable(geometry()) }),
            Err(Error::Refused(Refusal::TargetState(Errno::ESTALE)))
        );
    }
    #[test]
    fn executable_capture_checks_every_request_field_with_same_identity() {
        for field in 0..6 {
            let issuer = unsafe { ExecutableCaptureIssuer::new_backend(request()) };
            let (capture, verifier) = unsafe { issuer.completed() };
            let mut proof = unsafe { capture.certify_direct_executable(geometry()) };
            match field {
                0 => proof.request.target_tid += 1,
                1 => proof.request.ptracer_tid += 1,
                2 => proof.request.source_address += 1,
                3 => proof.request.source_length += 1,
                4 => proof.request.iovec_address += 1,
                5 => proof.request.register_buffer_address += 1,
                _ => unreachable!(),
            }
            assert_eq!(
                verifier.consume(proof),
                Err(Error::Refused(Refusal::TargetState(Errno::ESTALE)))
            );
        }
    }
    #[test]
    fn executable_capture_same_one_shot_identity_returns_only_geometry() {
        let issuer = unsafe { ExecutableCaptureIssuer::new_backend(request()) };
        assert_eq!(issuer.challenge().request(), request());
        let (capture, verifier) = unsafe { issuer.completed() };
        assert_eq!(
            verifier.consume(unsafe { capture.certify_direct_executable(geometry()) }),
            Ok(geometry())
        );
    }
}
