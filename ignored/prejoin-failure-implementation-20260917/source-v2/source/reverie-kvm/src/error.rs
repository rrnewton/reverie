/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

use thiserror::Error;

/// Errors produced by the KVM backend prototype.
#[derive(Debug, Error)]
pub enum Error {
    /// A peer or the Tool scheduler has made this run terminal. This internal
    /// outcome is never a successful guest status or a syscall errno.
    #[error("KVM execution stopped after a fatal run failure")]
    RunAborted,

    /// A shared typed failure retained until the root has joined all children.
    #[error("{0}")]
    SharedFailure(#[source] std::sync::Arc<Error>),

    /// A typed worker failure with its original guest thread identity.
    #[error("KVM worker cleanup failed: thread {tid}: {error}")]
    WorkerFailure {
        /// Guest thread whose physical join returned this failure.
        tid: i32,
        /// Original typed cause retained by the worker-error cache.
        #[source]
        error: std::sync::Arc<Error>,
    },

    /// Cleanup failed in addition to the original execution failure.
    #[error("{primary}; {}", cleanup.iter().map(ToString::to_string).collect::<Vec<_>>().join("; "))]
    WithCleanup {
        /// Original typed cause, also exposed through the source chain.
        #[source]
        primary: Box<Error>,
        /// Additional failures, without replacing the original cause.
        cleanup: Vec<Error>,
    },

    /// A typed cleanup cause with the operation that failed.
    #[error("{phase}: {error}")]
    Cleanup {
        /// Cleanup operation.
        phase: &'static str,
        /// Original typed cleanup error.
        #[source]
        error: Box<Error>,
    },

    /// A guest branch counter is unavailable or its accounting cannot be trusted.
    #[error("guest clock failed: {0}")]
    GuestClock(String),

    /// Reusing a completed initial ELF requires resetting pending KVM transport state.
    #[error("initial ELF reinstallation after execution is unsupported; create a fresh KvmBackend")]
    InitialElfReinstallationUnsupported,

    /// A host filesystem operation failed while preparing the guest.
    #[error("host filesystem operation failed: {0}")]
    HostIo(#[from] std::io::Error),

    /// A post-exec tool hook rejected the new guest image.
    #[error("Reverie post-exec hook failed: {0}")]
    PostExec(reverie::syscalls::Errno),

    /// A shared Reverie tool callback failed.
    #[error("Reverie tool failed: {0}")]
    Reverie(#[source] reverie::Error),

    /// A KVM ioctl or vCPU operation failed.
    #[error("KVM operation failed: {0}")]
    Kvm(#[from] kvm_ioctls::Error),

    /// The guest-memory mapping could not be created.
    #[error("failed to allocate guest memory: {0}")]
    MemoryMapping(#[source] std::io::Error),

    /// The ELF image could not be parsed.
    #[error("failed to parse ELF image: {0}")]
    ElfParse(#[from] goblin::error::Error),

    /// The ELF image cannot run in the minimal KVM process personality.
    #[error("unsupported ELF image: {0}")]
    UnsupportedElf(String),

    /// Guest memory must be non-empty and page aligned.
    #[error("invalid guest memory layout: base={guest_base:#x}, size={size:#x}")]
    InvalidMemoryLayout {
        /// First guest-physical address in the mapping.
        guest_base: u64,
        /// Mapping size in bytes.
        size: usize,
    },

    /// Linux process identifiers must be positive.
    #[error("invalid KVM root guest PID {0}")]
    InvalidGuestPid(i32),

    /// A guest-memory access fell outside the registered mapping.
    #[error(
        "guest memory access is out of bounds: address={address:#x}, length={length:#x}, mapping={guest_base:#x}..{guest_end:#x}"
    )]
    InvalidGuestAddress {
        /// First byte requested by the caller.
        address: u64,
        /// Requested number of bytes.
        length: usize,
        /// First guest-physical address in the mapping.
        guest_base: u64,
        /// Address immediately after the mapping.
        guest_end: u64,
    },

    /// A host-side user copy crossed an unmapped or `PROT_NONE` guest page.
    // TODO-HUMAN-REVIEW(PR-132): Review the guest user-access error API.
    #[error("guest user memory access is denied: address={address:#x}, length={length:#x}")]
    GuestMemoryAccessDenied {
        /// First byte requested by the caller.
        address: u64,
        /// Requested number of bytes.
        length: usize,
    },

    /// The transport frame named a number outside the architecture syscall table.
    #[error("invalid x86-64 syscall number {0}")]
    InvalidSyscallNumber(u64),

    /// Syscall frames must use addresses accepted by KVM's hypercall ABI.
    #[error("invalid syscall frame address {0:#x}")]
    InvalidSyscallFrameAddress(u64),

    /// The host kernel cannot forward the selected hypercall to userspace.
    #[error("KVM userspace hypercall exits are not supported")]
    HypercallExitUnsupported,

    /// The virtual CPU exposes neither the Intel nor AMD hypercall instruction.
    #[error("the virtual CPU exposes neither vmcall nor vmmcall")]
    HypercallInstructionUnsupported,

    /// The host cannot execute every instruction advertised by the fixed CPU profile.
    // TODO-HUMAN-REVIEW(PR-129): Review fail-closed CPUID capability validation.
    #[error("host cannot support deterministic CPUID profile: {0}")]
    UnsupportedCpuidProfile(String),

    /// The guest used a hypercall number other than the syscall transport.
    #[error("unexpected guest hypercall number {0}")]
    UnexpectedHypercall(u64),

    /// The bounded bootstrap area cannot allocate another thread transport.
    // TODO-HUMAN-REVIEW(PR-172): Review the fixed KVM guest-thread limit.
    #[error("KVM guest thread limit exceeded by tid {0}")]
    GuestThreadLimitExceeded(i32),

    /// Replacing the process image from a guest thread is not implemented.
    #[error("KVM guest threads cannot replace the process image")]
    GuestThreadExecUnsupported,

    /// Exec cancelled its siblings, but one of their consuming hooks failed.
    /// The Tool owner must finish consuming its own state before returning this
    /// error; no old or replacement guest continuation remains available.
    #[error(transparent)]
    ExecWorkerTeardown(Box<Error>),

    /// The fixed long-mode bootstrap layout does not fit in guest memory.
    #[error("guest memory is too small for the long-mode bootstrap")]
    LongModeMemoryTooSmall,

    /// No static ELF has been installed on this backend.
    #[error("no static ELF is installed")]
    StaticElfNotInstalled,

    /// KVM accepted only part of the long-mode MSR table.
    #[error("KVM installed {actual} of {expected} long-mode MSRs")]
    IncompleteMsrSetup {
        /// Number of MSRs supplied.
        expected: usize,
        /// Number of MSRs accepted.
        actual: usize,
    },

    /// The guest raised an x86 exception while running a loaded ELF image.
    #[error("guest exception vector {vector} at {instruction_pointer:#x} (CR2={fault_address:#x})")]
    GuestException {
        /// Architectural exception vector.
        vector: u8,
        /// Guest instruction pointer saved by the exception frame.
        instruction_pointer: u64,
        /// Guest CR2 value, meaningful for page faults.
        fault_address: u64,
    },

    /// The vCPU stopped for an event this prototype does not handle.
    #[error("unexpected vCPU exit: {0}")]
    UnexpectedVcpuExit(String),
}

impl Error {
    /// Original typed cause beneath shared ownership and cleanup aggregation.
    pub fn primary(&self) -> &Self {
        match self {
            Self::SharedFailure(error) | Self::WorkerFailure { error, .. } => error.primary(),
            Self::WithCleanup { primary, .. } => primary.primary(),
            Self::Cleanup { error, .. } => error.primary(),
            _ => self,
        }
    }

    pub(crate) fn retains_primary(&self, primary: &std::sync::Arc<Error>) -> bool {
        match self {
            Self::SharedFailure(error) | Self::WorkerFailure { error, .. } => {
                std::sync::Arc::ptr_eq(error, primary) || error.retains_primary(primary)
            }
            Self::WithCleanup { primary: error, .. }
            | Self::Cleanup { error, .. }
            | Self::ExecWorkerTeardown(error) => error.retains_primary(primary),
            _ => false,
        }
    }

    pub(crate) fn cleanup(self, phase: &'static str) -> Self {
        Self::Cleanup {
            phase,
            error: Box::new(self),
        }
    }

    pub(crate) fn with_cleanup(self, cleanup: Vec<Error>) -> Self {
        if cleanup.is_empty() {
            self
        } else {
            Self::WithCleanup {
                primary: Box::new(self),
                cleanup,
            }
        }
    }

    pub(crate) fn combine(mut errors: Vec<Error>) -> crate::Result<()> {
        if errors.is_empty() {
            Ok(())
        } else {
            let primary = errors.remove(0);
            Err(primary.with_cleanup(errors))
        }
    }
}
