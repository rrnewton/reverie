/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

#[cfg(target_arch = "x86_64")]
use core::fmt;
#[cfg(all(target_arch = "x86_64", feature = "memory", feature = "notifier"))]
use std::sync::Arc;

#[cfg(target_arch = "x86_64")]
pub use libc::user_fpregs_struct as FpRegs;
pub use libc::user_regs_struct as Regs;
#[cfg(all(target_arch = "x86_64", feature = "memory", feature = "notifier"))]
use syscalls::Errno;

#[cfg(all(target_arch = "x86_64", feature = "memory", feature = "notifier"))]
use crate::Error;
#[cfg(all(target_arch = "x86_64", feature = "memory", feature = "notifier"))]
use crate::LogicalStopId;
#[cfg(all(target_arch = "x86_64", feature = "memory", feature = "notifier"))]
use crate::PhysicalEventGenerationId;
#[cfg(all(target_arch = "x86_64", feature = "memory", feature = "notifier"))]
use crate::PhysicalStatusId;
#[cfg(all(target_arch = "x86_64", feature = "memory", feature = "notifier"))]
use crate::PhysicalTaskIdentity;
#[cfg(all(target_arch = "x86_64", feature = "memory", feature = "notifier"))]
use crate::Pid;

#[cfg(target_arch = "x86_64")]
const FXSAVE64_BYTES: usize = 512;
#[cfg(target_arch = "x86_64")]
const XSAVE64_HEADER_OFFSET: usize = FXSAVE64_BYTES;
#[cfg(target_arch = "x86_64")]
const XSAVE64_HEADER_BYTES: usize = 64;
#[cfg(target_arch = "x86_64")]
const XSAVE64_XSTATE_BV_OFFSET: usize = XSAVE64_HEADER_OFFSET;
#[cfg(target_arch = "x86_64")]
const XSAVE64_XCOMP_BV_OFFSET: usize = XSAVE64_HEADER_OFFSET + 8;
#[cfg(target_arch = "x86_64")]
const XSAVE64_RESERVED_OFFSET: usize = XSAVE64_HEADER_OFFSET + 16;
#[cfg(target_arch = "x86_64")]
const XSAVE64_ALIGNMENT: usize = 64;
#[cfg(target_arch = "x86_64")]
const LEGACY_XFEATURES: u64 = 0b11;
#[cfg(all(target_arch = "x86_64", feature = "memory", feature = "notifier"))]
const FXSAVE64_ALIGNMENT: usize = 16;
// Linux REGSET_XSTATE exports its OS-enabled XCR0 mask in the first u64 of
// FXSAVE's software-reserved region (bytes 464..471).
#[cfg(target_arch = "x86_64")]
const XSAVE64_KERNEL_XCR0_OFFSET: usize = 464;

/// Variable-length x86 extended processor state returned by `NT_X86_XSTATE`.
///
/// This includes the legacy x87/SSE state plus every kernel-exposed XSAVE
/// component enabled for the tracee. The bytes are intentionally opaque: a
/// caller may save and restore them, but should not assume a fixed layout.
// AUTONOMOUS-BOT-IMPLEMENTED
// TODO-HUMAN-REVIEW(PR-270): Review public opaque XSTATE save/restore storage.
#[cfg(target_arch = "x86_64")]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct XState(pub(crate) Vec<u8>);

#[cfg(target_arch = "x86_64")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct XsaveComponentLayout {
    xfeature: u64,
    offset: usize,
    size: usize,
}

#[cfg(target_arch = "x86_64")]
impl XsaveComponentLayout {
    const fn new(xfeature: u64, offset: usize, size: usize) -> Self {
        Self {
            xfeature,
            offset,
            size,
        }
    }
}

#[cfg(target_arch = "x86_64")]
#[derive(Clone, Copy, Debug)]
struct StandardXsave64Geometry<'a> {
    allocation_len: usize,
    image_len: usize,
    preserved_components: u64,
    components: &'a [XsaveComponentLayout],
}

#[cfg(target_arch = "x86_64")]
impl<'a> StandardXsave64Geometry<'a> {
    fn try_new(
        allocation_len: usize,
        image_len: usize,
        preserved_components: u64,
        components: &'a [XsaveComponentLayout],
    ) -> Result<Self, X86StateMergeError> {
        validate_standard_xsave64_geometry(
            allocation_len,
            image_len,
            preserved_components,
            components,
        )?;
        Ok(Self {
            allocation_len,
            image_len,
            preserved_components,
            components,
        })
    }
}

/// The exact tracee instruction contract used to save x86 extended state.
#[cfg(target_arch = "x86_64")]
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum X86SavedStateInstruction {
    /// The 64-bit `FXSAVE64` instruction and its exact 512-byte layout.
    Fxsave64,

    /// The unoptimized, standard-format `XSAVE64` instruction.
    ///
    /// This does not admit `XSAVEOPT`, `XSAVEC`, or `XSAVES`.
    Xsave64,
}

/// Controller-authenticated identity of one installed tracee-side save ABI.
#[cfg(target_arch = "x86_64")]
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct X86SavedStateInstallIdentity {
    runtime_generation: u64,
    trampoline_version: u64,
    trampoline_identity: u64,
}

#[cfg(target_arch = "x86_64")]
impl X86SavedStateInstallIdentity {
    /// Creates the identity published by an authenticated runtime install.
    pub const fn new(
        runtime_generation: u64,
        trampoline_version: u64,
        trampoline_identity: u64,
    ) -> Self {
        Self {
            runtime_generation,
            trampoline_version,
            trampoline_identity,
        }
    }

    /// Returns the runtime generation that installed the save ABI.
    pub const fn runtime_generation(self) -> u64 {
        self.runtime_generation
    }

    /// Returns the authenticated trampoline ABI version.
    pub const fn trampoline_version(self) -> u64 {
        self.trampoline_version
    }

    /// Returns the authenticated identity of the installed trampoline.
    pub const fn trampoline_identity(self) -> u64 {
        self.trampoline_identity
    }
}

#[cfg(all(target_arch = "x86_64", feature = "memory", feature = "notifier"))]
#[derive(Debug)]
struct X86SavedStateInstallLayoutInner {
    identity: X86SavedStateInstallIdentity,
    instruction: X86SavedStateInstruction,
    source_address: usize,
    allocation_len: usize,
    image_len: usize,
    preserved_components: u64,
    components: Box<[XsaveComponentLayout]>,
}

/// Immutable, structurally validated metadata for one saved-XSTATE ABI.
///
/// This object is deliberately not an authentication of its declared source,
/// opcode, component table, or install lineage. Linux does not expose canonical
/// per-feature offsets through `NT_X86_XSTATE`. The sole authentication boundary
/// is [`crate::Stopped::bind_x86_saved_state_install`], which must be called for
/// each exact parent or inherited child stop before safe read/merge is possible.
#[cfg(all(target_arch = "x86_64", feature = "memory", feature = "notifier"))]
#[derive(Clone, Debug)]
pub struct X86SavedStateInstallLayout(Arc<X86SavedStateInstallLayoutInner>);

#[cfg(all(target_arch = "x86_64", feature = "memory", feature = "notifier"))]
impl PartialEq for X86SavedStateInstallLayout {
    fn eq(&self, other: &Self) -> bool {
        self.same_seal(other)
    }
}

#[cfg(all(target_arch = "x86_64", feature = "memory", feature = "notifier"))]
impl Eq for X86SavedStateInstallLayout {}

#[cfg(all(target_arch = "x86_64", feature = "memory", feature = "notifier"))]
impl X86SavedStateInstallLayout {
    /// Structurally validates and seals untrusted tracee-layout metadata.
    ///
    /// `components` contains `(one_hot_xfeature, standard_offset, size)` entries.
    /// This checks lengths, alignment, mask coverage, ordering, overlap, and
    /// bounds only. It does not prove that the declared opcode ran, that the
    /// table is canonical for the tracee's CPUID/XGETBV view, or that the source
    /// belongs to any task. Those proofs belong to the later unsafe task bind.
    pub fn from_untrusted_tracee_metadata(
        identity: X86SavedStateInstallIdentity,
        instruction: X86SavedStateInstruction,
        source_address: usize,
        allocation_len: usize,
        image_len: usize,
        preserved_components: u64,
        components: &[(u64, usize, usize)],
    ) -> Result<Self, X86StateMergeError> {
        let components: Box<[XsaveComponentLayout]> = components
            .iter()
            .map(|&(xfeature, offset, size)| XsaveComponentLayout::new(xfeature, offset, size))
            .collect();
        validate_authenticated_install(
            instruction,
            source_address,
            allocation_len,
            image_len,
            preserved_components,
            &components,
        )?;
        Ok(Self(Arc::new(X86SavedStateInstallLayoutInner {
            identity,
            instruction,
            source_address,
            allocation_len,
            image_len,
            preserved_components,
            components,
        })))
    }

    /// Returns the structurally sealed but not yet authenticated install identity.
    pub fn declared_identity(&self) -> X86SavedStateInstallIdentity {
        self.0.identity
    }

    /// Returns the structurally sealed but not yet authenticated save instruction.
    pub fn declared_instruction(&self) -> X86SavedStateInstruction {
        self.0.instruction
    }

    pub(crate) fn image_len(&self) -> usize {
        self.0.image_len
    }

    pub(crate) fn source_address(&self) -> usize {
        self.0.source_address
    }

    pub(crate) fn same_seal(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

#[cfg(all(target_arch = "x86_64", feature = "memory", feature = "notifier"))]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum X86KernelTransportBinding {
    Fxsave64,
    StandardXsave64 {
        len: usize,
        os_enabled_xfeatures: u64,
    },
}

#[cfg(all(target_arch = "x86_64", feature = "memory", feature = "notifier"))]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct X86SavedStateInvocationIdentity {
    pid: Pid,
    physical_generation: PhysicalEventGenerationId,
    physical_task: PhysicalTaskIdentity,
    logical_stop: LogicalStopId,
    physical_status: Option<PhysicalStatusId>,
}

#[cfg(all(target_arch = "x86_64", feature = "memory", feature = "notifier"))]
impl X86SavedStateInvocationIdentity {
    pub(crate) const fn new(
        pid: Pid,
        physical_generation: PhysicalEventGenerationId,
        physical_task: PhysicalTaskIdentity,
        logical_stop: LogicalStopId,
        physical_status: Option<PhysicalStatusId>,
    ) -> Self {
        Self {
            pid,
            physical_generation,
            physical_task,
            logical_stop,
            physical_status,
        }
    }
}

/// A saved-XSTATE install bound to one exact stopped task generation.
///
/// Parent and fork child must create separate bindings even when they share the
/// same inherited [`X86SavedStateInstallLayout`].
#[cfg(all(target_arch = "x86_64", feature = "memory", feature = "notifier"))]
#[derive(Clone, Debug)]
pub struct BoundX86SavedStateInstall {
    layout: X86SavedStateInstallLayout,
    invocation: X86SavedStateInvocationIdentity,
    transport: X86KernelTransportBinding,
}

#[cfg(all(target_arch = "x86_64", feature = "memory", feature = "notifier"))]
impl BoundX86SavedStateInstall {
    pub(crate) fn new(
        layout: X86SavedStateInstallLayout,
        invocation: X86SavedStateInvocationIdentity,
        transport: X86KernelTransportBinding,
    ) -> Self {
        Self {
            layout,
            invocation,
            transport,
        }
    }

    /// Returns the authenticated runtime/trampoline install identity.
    pub fn identity(&self) -> X86SavedStateInstallIdentity {
        self.layout.declared_identity()
    }

    /// Returns the physical task generation to which this invocation is bound.
    pub fn physical_generation(&self) -> PhysicalEventGenerationId {
        self.invocation.physical_generation
    }

    /// Returns the exact logical stop authenticated by the unsafe bind.
    pub fn logical_stop(&self) -> LogicalStopId {
        self.invocation.logical_stop
    }

    /// Returns the physical status provenance authenticated by the unsafe bind.
    pub fn physical_status(&self) -> Option<PhysicalStatusId> {
        self.invocation.physical_status
    }

    pub(crate) fn layout(&self) -> &X86SavedStateInstallLayout {
        &self.layout
    }

    pub(crate) fn validate_invocation(
        &self,
        current: X86SavedStateInvocationIdentity,
        layout: &X86SavedStateInstallLayout,
        expected_identity: X86SavedStateInstallIdentity,
    ) -> Result<(), X86SavedStateAccessError> {
        if self.layout.declared_identity() != expected_identity {
            return Err(X86SavedStateAccessError::InstallIdentityMismatch);
        }
        if !self.layout.same_seal(layout) {
            return Err(X86SavedStateAccessError::InstallSealMismatch);
        }
        if self.invocation.pid != current.pid {
            return Err(X86SavedStateAccessError::TaskMismatch);
        }
        if self.invocation.physical_generation != current.physical_generation {
            return Err(X86SavedStateAccessError::PhysicalGenerationMismatch);
        }
        if self.invocation.physical_task != current.physical_task {
            return Err(X86SavedStateAccessError::PhysicalTaskIdentityMismatch);
        }
        if self.invocation.logical_stop != current.logical_stop {
            return Err(X86SavedStateAccessError::LogicalStopMismatch);
        }
        if self.invocation.physical_status != current.physical_status {
            return Err(X86SavedStateAccessError::PhysicalStatusMismatch);
        }
        Ok(())
    }

    pub(crate) fn validate_transport(
        &self,
        current: X86KernelTransportBinding,
    ) -> Result<(), X86SavedStateAccessError> {
        if self.transport != current {
            return Err(X86SavedStateAccessError::KernelTransportMismatch);
        }
        Ok(())
    }

    pub(crate) fn validate_post_read_template(
        &self,
        before: &X86ExtendedState,
        after: &X86ExtendedState,
        after_transport: X86KernelTransportBinding,
    ) -> Result<(), X86SavedStateAccessError> {
        self.validate_transport(after_transport)?;
        if before != after {
            return Err(X86SavedStateAccessError::KernelTemplateChanged);
        }
        Ok(())
    }
}

/// Failure while binding, reading, or merging an authenticated saved-XSTATE install.
#[cfg(all(target_arch = "x86_64", feature = "memory", feature = "notifier"))]
#[derive(Debug, Eq, PartialEq)]
pub enum X86SavedStateAccessError {
    /// The caller's expected runtime/trampoline install identity did not match.
    InstallIdentityMismatch,
    /// A different immutable install seal was substituted after task binding.
    InstallSealMismatch,
    /// The stopped thread ID did not match the bound invocation.
    TaskMismatch,
    /// The immutable notifier generation did not match the bound invocation.
    PhysicalGenerationMismatch,
    /// The exact physical task identity did not match the bound invocation.
    PhysicalTaskIdentityMismatch,
    /// The exact logical stopped-state identity did not match the bound invocation.
    LogicalStopMismatch,
    /// The physical status provenance did not match the bound invocation.
    PhysicalStatusMismatch,
    /// The kernel regset format, length, or enabled-feature mask changed.
    KernelTransportMismatch,
    /// The stopped task's kernel register template changed across the remote read.
    KernelTemplateChanged,
    /// Reading the exact authenticated tracee save buffer failed.
    Memory(Errno),
    /// Capturing the kernel template failed.
    KernelState(Error),
    /// The authenticated image could not be merged safely.
    Merge(X86StateMergeError),
}

#[cfg(all(target_arch = "x86_64", feature = "memory", feature = "notifier"))]
impl fmt::Display for X86SavedStateAccessError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InstallIdentityMismatch => f.write_str("saved-XSTATE install identity mismatch"),
            Self::InstallSealMismatch => f.write_str("saved-XSTATE install seal mismatch"),
            Self::TaskMismatch => f.write_str("saved-XSTATE task mismatch"),
            Self::PhysicalGenerationMismatch => {
                f.write_str("saved-XSTATE physical generation mismatch")
            }
            Self::PhysicalTaskIdentityMismatch => {
                f.write_str("saved-XSTATE physical task identity mismatch")
            }
            Self::LogicalStopMismatch => f.write_str("saved-XSTATE logical stop mismatch"),
            Self::PhysicalStatusMismatch => f.write_str("saved-XSTATE physical status mismatch"),
            Self::KernelTransportMismatch => f.write_str("saved-XSTATE kernel transport mismatch"),
            Self::KernelTemplateChanged => {
                f.write_str("saved-XSTATE kernel template changed during read")
            }
            Self::Memory(error) => write!(f, "saved-XSTATE memory read failed: {error}"),
            Self::KernelState(error) => write!(f, "saved-XSTATE kernel state failed: {error}"),
            Self::Merge(error) => write!(f, "saved-XSTATE merge failed: {error}"),
        }
    }
}

#[cfg(all(target_arch = "x86_64", feature = "memory", feature = "notifier"))]
impl std::error::Error for X86SavedStateAccessError {}

/// A complete x86 state value that can be restored to a stopped tracee.
///
/// The FXSAVE variant keeps non-XSAVE processors supported without assuming
/// that `NT_X86_XSTATE` exists. The XSAVE bytes remain opaque so callers
/// cannot accidentally mutate reserved or kernel-owned fields.
#[cfg(target_arch = "x86_64")]
#[derive(Clone, Debug)]
pub enum X86ExtendedState {
    /// State obtained through the fixed-size floating-point regset.
    Fxsave64(Box<FpRegs>),

    /// State obtained through the variable-size `NT_X86_XSTATE` regset.
    StandardXsave64(XState),
}

#[cfg(target_arch = "x86_64")]
impl PartialEq for X86ExtendedState {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Fxsave64(left), Self::Fxsave64(right)) => {
                fpregs_bytes(left) == fpregs_bytes(right)
            }
            (Self::StandardXsave64(left), Self::StandardXsave64(right)) => left == right,
            _ => false,
        }
    }
}

#[cfg(target_arch = "x86_64")]
impl Eq for X86ExtendedState {}

/// Why a saved x86 state image could not be merged safely.
#[cfg(target_arch = "x86_64")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum X86StateMergeError {
    /// The saved image and ptrace template use different formats.
    FormatMismatch,

    /// An image or allocation length is internally inconsistent.
    InvalidLength,

    /// The authenticated source address is zero, wraps, or otherwise invalid.
    InvalidSourceAddress,

    /// The authenticated source address does not meet its instruction alignment.
    MisalignedSource,

    /// The advertised component mask is empty, unsupported, or inconsistent.
    InvalidComponentMask,

    /// A compacted XSAVE image was supplied where standard format is required.
    CompactedFormat,

    /// Reserved XSAVE header bytes were not zero.
    NonzeroReservedHeader,

    /// The published component table is incomplete, duplicated, overlapping,
    /// non-canonical, or out of bounds.
    UnsupportedComponentLayout,

    /// A component that the callback could alter was not preserved.
    UnpreservedActiveComponent,

    /// The authenticated mask contains a feature the kernel did not enable.
    KernelComponentMaskMismatch,

    /// The save instruction and its authenticated geometry disagree.
    InvalidInstructionContract,
}

#[cfg(target_arch = "x86_64")]
impl fmt::Display for X86StateMergeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::FormatMismatch => "saved and template x86 state formats differ",
            Self::InvalidLength => "invalid saved x86 state length",
            Self::InvalidSourceAddress => "invalid saved x86 state source address",
            Self::MisalignedSource => "saved x86 state source is misaligned",
            Self::InvalidComponentMask => "invalid saved XSAVE component mask",
            Self::CompactedFormat => "compacted XSAVE format is unsupported",
            Self::NonzeroReservedHeader => "XSAVE reserved header bytes are nonzero",
            Self::UnsupportedComponentLayout => "invalid standard XSAVE component layout",
            Self::UnpreservedActiveComponent => "an active x86 state component was not preserved",
            Self::KernelComponentMaskMismatch => {
                "saved XSAVE component mask exceeds the kernel-enabled mask"
            }
            Self::InvalidInstructionContract => "saved x86 state instruction and geometry disagree",
        })
    }
}

#[cfg(target_arch = "x86_64")]
impl std::error::Error for X86StateMergeError {}

#[cfg(target_arch = "x86_64")]
impl X86ExtendedState {
    #[cfg(all(feature = "memory", feature = "notifier"))]
    fn merge_authenticated_install(
        &self,
        install: &X86SavedStateInstallLayoutInner,
        saved: &[u8],
    ) -> Result<Self, X86StateMergeError> {
        match (self, install.instruction) {
            (Self::Fxsave64(template), X86SavedStateInstruction::Fxsave64) => {
                if saved.len() != FXSAVE64_BYTES {
                    return Err(X86StateMergeError::InvalidLength);
                }
                let mut output = **template;
                overlay_fxsave64_architectural(fpregs_bytes_mut(&mut output), saved)?;
                Ok(Self::Fxsave64(Box::new(output)))
            }
            (Self::StandardXsave64(template), X86SavedStateInstruction::Xsave64) => {
                let geometry = StandardXsave64Geometry::try_new(
                    install.allocation_len,
                    install.image_len,
                    install.preserved_components,
                    &install.components,
                )?;
                let output =
                    merge_standard_xsave64(&template.0, saved, install.source_address, geometry)?;
                Ok(Self::StandardXsave64(XState(output)))
            }
            _ => Err(X86StateMergeError::FormatMismatch),
        }
    }
}

#[cfg(all(target_arch = "x86_64", feature = "memory", feature = "notifier"))]
fn validate_authenticated_install(
    instruction: X86SavedStateInstruction,
    source_address: usize,
    allocation_len: usize,
    image_len: usize,
    preserved_components: u64,
    components: &[XsaveComponentLayout],
) -> Result<(), X86StateMergeError> {
    if source_address == 0
        || source_address.checked_add(allocation_len).is_none()
        || allocation_len == 0
    {
        return Err(X86StateMergeError::InvalidSourceAddress);
    }
    match instruction {
        X86SavedStateInstruction::Fxsave64 => {
            if !source_address.is_multiple_of(FXSAVE64_ALIGNMENT) {
                return Err(X86StateMergeError::MisalignedSource);
            }
            if allocation_len != FXSAVE64_BYTES
                || image_len != FXSAVE64_BYTES
                || preserved_components != LEGACY_XFEATURES
                || !components.is_empty()
            {
                return Err(X86StateMergeError::InvalidInstructionContract);
            }
            Ok(())
        }
        X86SavedStateInstruction::Xsave64 => {
            if !source_address.is_multiple_of(XSAVE64_ALIGNMENT) {
                return Err(X86StateMergeError::MisalignedSource);
            }
            validate_standard_xsave64_geometry(
                allocation_len,
                image_len,
                preserved_components,
                components,
            )
        }
    }
}

#[cfg(all(target_arch = "x86_64", feature = "memory", feature = "notifier"))]
impl X86SavedStateInstallLayout {
    pub(crate) fn kernel_transport_binding(
        &self,
        template: &X86ExtendedState,
    ) -> Result<X86KernelTransportBinding, X86StateMergeError> {
        match (self.0.instruction, template) {
            (X86SavedStateInstruction::Fxsave64, X86ExtendedState::Fxsave64(_)) => {
                Ok(X86KernelTransportBinding::Fxsave64)
            }
            (X86SavedStateInstruction::Xsave64, X86ExtendedState::StandardXsave64(template)) => {
                if template.0.len() < XSAVE64_HEADER_OFFSET + XSAVE64_HEADER_BYTES
                    || self.0.image_len > template.0.len()
                {
                    return Err(X86StateMergeError::InvalidLength);
                }
                let os_enabled_xfeatures = read_u64(&template.0, XSAVE64_KERNEL_XCR0_OFFSET)?;
                if self.0.preserved_components & !os_enabled_xfeatures != 0 {
                    return Err(X86StateMergeError::KernelComponentMaskMismatch);
                }
                let template_bv = read_u64(&template.0, XSAVE64_XSTATE_BV_OFFSET)?;
                if template_bv & !os_enabled_xfeatures != 0 {
                    return Err(X86StateMergeError::InvalidComponentMask);
                }
                validate_standard_header(&template.0)?;
                for component in &self.0.components {
                    let end = component
                        .offset
                        .checked_add(component.size)
                        .ok_or(X86StateMergeError::UnsupportedComponentLayout)?;
                    if end > template.0.len() {
                        return Err(X86StateMergeError::UnsupportedComponentLayout);
                    }
                }
                Ok(X86KernelTransportBinding::StandardXsave64 {
                    len: template.0.len(),
                    os_enabled_xfeatures,
                })
            }
            _ => Err(X86StateMergeError::FormatMismatch),
        }
    }

    pub(crate) fn merge(
        &self,
        template: &X86ExtendedState,
        saved: &[u8],
    ) -> Result<X86ExtendedState, X86StateMergeError> {
        template.merge_authenticated_install(&self.0, saved)
    }
}

#[cfg(target_arch = "x86_64")]
fn validate_standard_header(bytes: &[u8]) -> Result<(), X86StateMergeError> {
    if bytes.len() < XSAVE64_HEADER_OFFSET + XSAVE64_HEADER_BYTES {
        return Err(X86StateMergeError::InvalidLength);
    }
    if read_u64(bytes, XSAVE64_XCOMP_BV_OFFSET)? != 0 {
        return Err(X86StateMergeError::CompactedFormat);
    }
    if bytes[XSAVE64_RESERVED_OFFSET..XSAVE64_HEADER_OFFSET + XSAVE64_HEADER_BYTES]
        .iter()
        .any(|byte| *byte != 0)
    {
        return Err(X86StateMergeError::NonzeroReservedHeader);
    }
    Ok(())
}

#[cfg(target_arch = "x86_64")]
fn overlay_fxsave64_architectural(
    output: &mut [u8],
    saved: &[u8],
) -> Result<(), X86StateMergeError> {
    if output.len() < FXSAVE64_BYTES || saved.len() < FXSAVE64_BYTES {
        return Err(X86StateMergeError::InvalidLength);
    }

    output[0..5].copy_from_slice(&saved[0..5]);
    output[6..28].copy_from_slice(&saved[6..28]);
    for slot in 0..8 {
        let offset = 32 + slot * 16;
        output[offset..offset + 10].copy_from_slice(&saved[offset..offset + 10]);
    }
    output[160..416].copy_from_slice(&saved[160..416]);
    Ok(())
}

#[cfg(target_arch = "x86_64")]
fn validate_standard_xsave64_geometry(
    allocation_len: usize,
    image_len: usize,
    preserved_components: u64,
    components: &[XsaveComponentLayout],
) -> Result<(), X86StateMergeError> {
    let rounded_image_len = image_len
        .checked_add(XSAVE64_ALIGNMENT - 1)
        .map(|value| value & !(XSAVE64_ALIGNMENT - 1))
        .ok_or(X86StateMergeError::InvalidLength)?;
    if image_len < XSAVE64_HEADER_OFFSET + XSAVE64_HEADER_BYTES
        || allocation_len != rounded_image_len
    {
        return Err(X86StateMergeError::InvalidLength);
    }
    if preserved_components & LEGACY_XFEATURES != LEGACY_XFEATURES {
        return Err(X86StateMergeError::InvalidComponentMask);
    }
    let expected_components = preserved_components & !LEGACY_XFEATURES;
    if components.len() > 62 || components.len() != expected_components.count_ones() as usize {
        return Err(X86StateMergeError::UnsupportedComponentLayout);
    }

    let mut seen_components = 0_u64;
    let mut prior_xfeature = None;
    let mut ranges = Vec::with_capacity(components.len());
    let mut extent = XSAVE64_HEADER_OFFSET + XSAVE64_HEADER_BYTES;
    for component in components {
        if !component.xfeature.is_power_of_two()
            || component.xfeature & LEGACY_XFEATURES != 0
            || prior_xfeature.is_some_and(|prior| prior >= component.xfeature)
        {
            return Err(X86StateMergeError::UnsupportedComponentLayout);
        }
        let bit = component.xfeature;
        if expected_components & bit == 0 || seen_components & bit != 0 {
            return Err(X86StateMergeError::UnsupportedComponentLayout);
        }
        let end = component
            .offset
            .checked_add(component.size)
            .ok_or(X86StateMergeError::UnsupportedComponentLayout)?;
        if component.size == 0
            || component.offset < XSAVE64_HEADER_OFFSET + XSAVE64_HEADER_BYTES
            || end > image_len
        {
            return Err(X86StateMergeError::UnsupportedComponentLayout);
        }
        if ranges
            .iter()
            .any(|&(prior_start, prior_end)| component.offset < prior_end && prior_start < end)
        {
            return Err(X86StateMergeError::UnsupportedComponentLayout);
        }
        prior_xfeature = Some(component.xfeature);
        seen_components |= bit;
        ranges.push((component.offset, end));
        extent = extent.max(end);
    }
    if seen_components != expected_components || extent != image_len {
        return Err(X86StateMergeError::UnsupportedComponentLayout);
    }
    Ok(())
}

#[cfg(target_arch = "x86_64")]
fn merge_standard_xsave64(
    template: &[u8],
    saved: &[u8],
    source_address: usize,
    geometry: StandardXsave64Geometry<'_>,
) -> Result<Vec<u8>, X86StateMergeError> {
    validate_standard_xsave64_geometry(
        geometry.allocation_len,
        geometry.image_len,
        geometry.preserved_components,
        geometry.components,
    )?;
    if source_address == 0 {
        return Err(X86StateMergeError::InvalidSourceAddress);
    }
    if saved.len() != geometry.image_len
        || template.len() < XSAVE64_HEADER_OFFSET + XSAVE64_HEADER_BYTES
        || source_address
            .checked_add(geometry.allocation_len)
            .is_none()
    {
        return Err(X86StateMergeError::InvalidLength);
    }
    if !source_address.is_multiple_of(XSAVE64_ALIGNMENT) {
        return Err(X86StateMergeError::MisalignedSource);
    }

    validate_standard_header(template)?;
    validate_standard_header(saved)?;
    let kernel_xfeatures = read_u64(template, XSAVE64_KERNEL_XCR0_OFFSET)?;
    if geometry.preserved_components & !kernel_xfeatures != 0 {
        return Err(X86StateMergeError::KernelComponentMaskMismatch);
    }
    let template_bv = read_u64(template, XSAVE64_XSTATE_BV_OFFSET)?;
    let saved_bv = read_u64(saved, XSAVE64_XSTATE_BV_OFFSET)?;
    if template_bv & !kernel_xfeatures != 0 {
        return Err(X86StateMergeError::InvalidComponentMask);
    }
    if saved_bv & !geometry.preserved_components != 0 {
        return Err(X86StateMergeError::InvalidComponentMask);
    }
    if template_bv & !geometry.preserved_components != 0 {
        return Err(X86StateMergeError::UnpreservedActiveComponent);
    }
    for component in geometry.components {
        let end = component.offset + component.size;
        if end > template.len() {
            return Err(X86StateMergeError::UnsupportedComponentLayout);
        }
    }

    let mut output = template.to_vec();
    overlay_fxsave64_architectural(&mut output, saved)?;
    for component in geometry.components {
        let bit = component.xfeature;
        let end = component.offset + component.size;
        if saved_bv & bit != 0 {
            output[component.offset..end].copy_from_slice(&saved[component.offset..end]);
        } else {
            output[component.offset..end].fill(0);
        }
    }

    let output_bv =
        (template_bv & !geometry.preserved_components) | (saved_bv & geometry.preserved_components);
    output[XSAVE64_XSTATE_BV_OFFSET..XSAVE64_XSTATE_BV_OFFSET + 8]
        .copy_from_slice(&output_bv.to_le_bytes());
    Ok(output)
}

#[cfg(target_arch = "x86_64")]
fn read_u64(bytes: &[u8], offset: usize) -> Result<u64, X86StateMergeError> {
    let value: [u8; 8] = bytes
        .get(offset..offset + 8)
        .ok_or(X86StateMergeError::InvalidLength)?
        .try_into()
        .map_err(|_| X86StateMergeError::InvalidLength)?;
    Ok(u64::from_le_bytes(value))
}

#[cfg(target_arch = "x86_64")]
fn fpregs_bytes(regs: &FpRegs) -> &[u8] {
    debug_assert_eq!(core::mem::size_of::<FpRegs>(), FXSAVE64_BYTES);
    unsafe {
        core::slice::from_raw_parts(
            core::ptr::from_ref(regs).cast::<u8>(),
            core::mem::size_of::<FpRegs>(),
        )
    }
}

#[cfg(all(target_arch = "x86_64", feature = "memory", feature = "notifier"))]
fn fpregs_bytes_mut(regs: &mut FpRegs) -> &mut [u8] {
    debug_assert_eq!(core::mem::size_of::<FpRegs>(), FXSAVE64_BYTES);
    unsafe {
        core::slice::from_raw_parts_mut(
            core::ptr::from_mut(regs).cast::<u8>(),
            core::mem::size_of::<FpRegs>(),
        )
    }
}

#[cfg(all(test, target_arch = "x86_64"))]
mod x86_state_tests {
    use super::*;

    fn set_u64(bytes: &mut [u8], offset: usize, value: u64) {
        bytes[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
    }

    #[test]
    fn fxsave64_overlay_preserves_every_reserved_region() {
        let mut template = vec![0xa5; FXSAVE64_BYTES];
        let saved: Vec<u8> = (0..FXSAVE64_BYTES).map(|index| index as u8).collect();
        let original = template.clone();

        overlay_fxsave64_architectural(&mut template, &saved).expect("merge FXSAVE64");

        assert_eq!(&template[0..5], &saved[0..5]);
        assert_eq!(template[5], original[5]);
        assert_eq!(&template[6..28], &saved[6..28]);
        assert_eq!(&template[28..32], &original[28..32]);
        for slot in 0..8 {
            let offset = 32 + slot * 16;
            assert_eq!(&template[offset..offset + 10], &saved[offset..offset + 10]);
            assert_eq!(
                &template[offset + 10..offset + 16],
                &original[offset + 10..offset + 16]
            );
        }
        assert_eq!(&template[160..416], &saved[160..416]);
        assert_eq!(&template[416..512], &original[416..512]);
    }

    #[test]
    fn standard_xsave64_merges_present_components_and_bv() {
        const TEMPLATE_LENGTH: usize = 896;
        const SAVED_IMAGE_LENGTH: usize = 832;
        const SAVED_ALLOCATION_LENGTH: usize = 832;
        let mut template = vec![0x44; TEMPLATE_LENGTH];
        let mut saved = vec![0x99; SAVED_IMAGE_LENGTH];
        template[XSAVE64_RESERVED_OFFSET..XSAVE64_HEADER_OFFSET + XSAVE64_HEADER_BYTES].fill(0);
        saved[XSAVE64_RESERVED_OFFSET..XSAVE64_HEADER_OFFSET + XSAVE64_HEADER_BYTES].fill(0);
        let mask = LEGACY_XFEATURES | (1 << 2);
        set_u64(&mut template, XSAVE64_KERNEL_XCR0_OFFSET, mask);
        set_u64(&mut template, XSAVE64_XSTATE_BV_OFFSET, mask);
        set_u64(&mut template, XSAVE64_XCOMP_BV_OFFSET, 0);
        set_u64(&mut saved, XSAVE64_XSTATE_BV_OFFSET, mask);
        set_u64(&mut saved, XSAVE64_XCOMP_BV_OFFSET, 0);
        // XFEATURE 2 is the canonical standard-format YMM-high component.
        let components = [XsaveComponentLayout::new(1 << 2, 576, 256)];
        let geometry = StandardXsave64Geometry::try_new(
            SAVED_ALLOCATION_LENGTH,
            SAVED_IMAGE_LENGTH,
            mask,
            &components,
        )
        .expect("validate synthetic standard XSAVE64 geometry");

        let output = merge_standard_xsave64(&template, &saved, 0x1000, geometry)
            .expect("merge standard XSAVE64");

        assert_eq!(read_u64(&output, XSAVE64_XSTATE_BV_OFFSET).unwrap(), mask);
        assert_eq!(&output[576..832], &saved[576..832]);
        assert_eq!(&output[832..896], &template[832..896]);
        assert_eq!(output[5], template[5]);
        assert_eq!(&output[28..32], &template[28..32]);
        assert_eq!(&output[416..512], &template[416..512]);
        assert_eq!(&output[528..576], &template[528..576]);

        set_u64(&mut saved, XSAVE64_XSTATE_BV_OFFSET, LEGACY_XFEATURES);
        let initialized = merge_standard_xsave64(&template, &saved, 0x1000, geometry)
            .expect("initialize absent standard XSAVE64 component");
        assert_eq!(&initialized[576..832], &[0; 256]);
        assert_eq!(
            read_u64(&initialized, XSAVE64_XSTATE_BV_OFFSET).unwrap(),
            LEGACY_XFEATURES
        );
    }

    #[test]
    fn standard_xsave64_rejects_invalid_metadata() {
        const LENGTH: usize = 640;
        let mask = LEGACY_XFEATURES | (1 << 2);
        let components = [XsaveComponentLayout::new(1 << 2, 576, 64)];
        let geometry = StandardXsave64Geometry::try_new(LENGTH, LENGTH, mask, &components)
            .expect("valid geometry");
        let mut template = vec![0; LENGTH];
        let mut saved = vec![0; LENGTH];
        set_u64(&mut template, XSAVE64_KERNEL_XCR0_OFFSET, mask);
        set_u64(&mut saved, XSAVE64_XSTATE_BV_OFFSET, mask);

        assert_eq!(
            merge_standard_xsave64(&template, &saved, 0x1001, geometry),
            Err(X86StateMergeError::MisalignedSource)
        );
        let shorter_components = [XsaveComponentLayout::new(1 << 2, 576, 63)];
        let shorter_geometry =
            StandardXsave64Geometry::try_new(LENGTH, LENGTH - 1, mask, &shorter_components)
                .expect("valid shorter geometry");
        assert_eq!(
            merge_standard_xsave64(&template, &saved, 0x1000, shorter_geometry),
            Err(X86StateMergeError::InvalidLength)
        );
        set_u64(&mut saved, XSAVE64_XSTATE_BV_OFFSET, mask | (1 << 7));
        assert_eq!(
            merge_standard_xsave64(&template, &saved, 0x1000, geometry),
            Err(X86StateMergeError::InvalidComponentMask)
        );
        set_u64(&mut saved, XSAVE64_XSTATE_BV_OFFSET, mask);

        set_u64(&mut template, XSAVE64_KERNEL_XCR0_OFFSET, mask | (1 << 7));
        set_u64(&mut template, XSAVE64_XSTATE_BV_OFFSET, 1 << 7);
        assert_eq!(
            merge_standard_xsave64(&template, &saved, 0x1000, geometry),
            Err(X86StateMergeError::UnpreservedActiveComponent)
        );
        set_u64(&mut template, XSAVE64_XSTATE_BV_OFFSET, 0);
        set_u64(&mut template, XSAVE64_KERNEL_XCR0_OFFSET, mask);

        set_u64(&mut saved, XSAVE64_XCOMP_BV_OFFSET, 1 << 63);
        assert_eq!(
            merge_standard_xsave64(&template, &saved, 0x1000, geometry),
            Err(X86StateMergeError::CompactedFormat)
        );
        set_u64(&mut saved, XSAVE64_XCOMP_BV_OFFSET, 0);
        saved[XSAVE64_RESERVED_OFFSET] = 1;
        assert_eq!(
            merge_standard_xsave64(&template, &saved, 0x1000, geometry),
            Err(X86StateMergeError::NonzeroReservedHeader)
        );
        saved[XSAVE64_RESERVED_OFFSET] = 0;

        let larger_components = [XsaveComponentLayout::new(1 << 2, 624, 64)];
        let larger_geometry =
            StandardXsave64Geometry::try_new(704, 688, mask, &larger_components).unwrap();
        let mut larger_saved = vec![0; 688];
        set_u64(&mut larger_saved, XSAVE64_XSTATE_BV_OFFSET, mask);
        assert_eq!(
            merge_standard_xsave64(&template, &larger_saved, 0x1000, larger_geometry),
            Err(X86StateMergeError::UnsupportedComponentLayout)
        );

        template[XSAVE64_RESERVED_OFFSET] = 1;
        assert_eq!(
            merge_standard_xsave64(&template, &saved, 0x1000, geometry),
            Err(X86StateMergeError::NonzeroReservedHeader)
        );
    }

    #[test]
    fn standard_xsave64_geometry_rejects_incomplete_duplicate_and_overlap() {
        let mask = LEGACY_XFEATURES | (1 << 2) | (1 << 3);
        assert_eq!(
            StandardXsave64Geometry::try_new(
                704,
                704,
                mask,
                &[XsaveComponentLayout::new(1 << 2, 576, 64)],
            )
            .unwrap_err(),
            X86StateMergeError::UnsupportedComponentLayout
        );
        assert_eq!(
            StandardXsave64Geometry::try_new(
                704,
                704,
                mask,
                &[
                    XsaveComponentLayout::new(1 << 2, 576, 64),
                    XsaveComponentLayout::new(1 << 2, 640, 64),
                ],
            )
            .unwrap_err(),
            X86StateMergeError::UnsupportedComponentLayout
        );
        assert_eq!(
            StandardXsave64Geometry::try_new(
                704,
                704,
                mask,
                &[
                    XsaveComponentLayout::new(1 << 2, 576, 64),
                    XsaveComponentLayout::new(1 << 3, 608, 64),
                ],
            )
            .unwrap_err(),
            X86StateMergeError::UnsupportedComponentLayout
        );
        assert_eq!(
            StandardXsave64Geometry::try_new(
                704,
                704,
                mask,
                &[
                    XsaveComponentLayout::new(1 << 3, 576, 64),
                    XsaveComponentLayout::new(1 << 2, 640, 64),
                ],
            )
            .unwrap_err(),
            X86StateMergeError::UnsupportedComponentLayout
        );
        assert_eq!(
            StandardXsave64Geometry::try_new(
                640,
                640,
                LEGACY_XFEATURES | (1 << 2),
                &[XsaveComponentLayout::new(1 << 2, 608, 64)],
            )
            .unwrap_err(),
            X86StateMergeError::UnsupportedComponentLayout
        );
        assert_eq!(
            StandardXsave64Geometry::try_new(576, 576, 1 << 2, &[]).unwrap_err(),
            X86StateMergeError::InvalidComponentMask
        );
    }

    #[cfg(all(feature = "memory", feature = "notifier"))]
    fn install_identity(runtime_generation: u64) -> X86SavedStateInstallIdentity {
        X86SavedStateInstallIdentity::new(runtime_generation, 7, 0xa11ce)
    }

    #[cfg(all(feature = "memory", feature = "notifier"))]
    fn standard_layout(
        identity: X86SavedStateInstallIdentity,
        source_address: usize,
        component: (u64, usize, usize),
    ) -> X86SavedStateInstallLayout {
        X86SavedStateInstallLayout::from_untrusted_tracee_metadata(
            identity,
            X86SavedStateInstruction::Xsave64,
            source_address,
            832,
            832,
            LEGACY_XFEATURES | (1 << 2),
            &[component],
        )
        .expect("structurally valid untrusted layout fixture")
    }

    #[cfg(all(feature = "memory", feature = "notifier"))]
    fn standard_template(len: usize, kernel_mask: u64) -> X86ExtendedState {
        let mut bytes = vec![0; len];
        set_u64(&mut bytes, XSAVE64_KERNEL_XCR0_OFFSET, kernel_mask);
        X86ExtendedState::StandardXsave64(XState(bytes))
    }

    #[cfg(all(feature = "memory", feature = "notifier"))]
    #[test]
    fn install_binding_rejects_geometry_address_and_generation_substitution() {
        let identity = install_identity(11);
        let install = standard_layout(identity, 0x1000, (1 << 2, 576, 256));
        // Linux cannot independently expose per-feature offsets. Structural
        // construction accepts this same-mask/same-extent lie, but the unsafe
        // bind must authenticate one exact seal and prevents later substitution.
        let shifted = standard_layout(identity, 0x1000, (1 << 2, 640, 192));
        let wrong_source = standard_layout(identity, 0x2000, (1 << 2, 576, 256));
        assert_eq!(install, install.clone());
        assert_ne!(install, shifted);
        assert_ne!(install, wrong_source);
        let pid = Pid::from_raw(41);
        let task = PhysicalTaskIdentity::direct_child(pid);
        let generation = PhysicalEventGenerationId::allocate();
        let logical_stop = LogicalStopId::from_raw(71).unwrap();
        let physical_status = Some(PhysicalStatusId::from_raw(81).unwrap());
        let current = X86SavedStateInvocationIdentity::new(
            pid,
            generation,
            task,
            logical_stop,
            physical_status,
        );
        let transport = install
            .kernel_transport_binding(&standard_template(832, LEGACY_XFEATURES | (1 << 2)))
            .unwrap();
        let bound = BoundX86SavedStateInstall::new(install.clone(), current, transport);

        assert_eq!(
            bound.validate_invocation(current, &install, identity),
            Ok(())
        );
        assert_eq!(
            bound.validate_invocation(current, &shifted, identity),
            Err(X86SavedStateAccessError::InstallSealMismatch)
        );
        assert_eq!(
            bound.validate_invocation(current, &wrong_source, identity),
            Err(X86SavedStateAccessError::InstallSealMismatch)
        );
        assert_eq!(
            bound.validate_invocation(
                X86SavedStateInvocationIdentity {
                    physical_generation: PhysicalEventGenerationId::allocate(),
                    ..current
                },
                &install,
                identity,
            ),
            Err(X86SavedStateAccessError::PhysicalGenerationMismatch)
        );
        assert_eq!(
            bound.validate_invocation(
                X86SavedStateInvocationIdentity {
                    pid: Pid::from_raw(42),
                    ..current
                },
                &install,
                identity,
            ),
            Err(X86SavedStateAccessError::TaskMismatch)
        );
        assert_eq!(
            bound.validate_invocation(
                X86SavedStateInvocationIdentity {
                    physical_task: PhysicalTaskIdentity::captured(pid, pid, 1, 2, 3),
                    ..current
                },
                &install,
                identity,
            ),
            Err(X86SavedStateAccessError::PhysicalTaskIdentityMismatch)
        );
        assert_eq!(
            bound.validate_invocation(current, &install, install_identity(12)),
            Err(X86SavedStateAccessError::InstallIdentityMismatch)
        );
        assert_eq!(
            bound.validate_invocation(
                X86SavedStateInvocationIdentity {
                    logical_stop: LogicalStopId::from_raw(72).unwrap(),
                    ..current
                },
                &install,
                identity,
            ),
            Err(X86SavedStateAccessError::LogicalStopMismatch)
        );
        assert_eq!(
            bound.validate_invocation(
                X86SavedStateInvocationIdentity {
                    physical_status: Some(PhysicalStatusId::from_raw(82).unwrap()),
                    ..current
                },
                &install,
                identity,
            ),
            Err(X86SavedStateAccessError::PhysicalStatusMismatch)
        );
        assert_eq!(
            bound.validate_invocation(
                X86SavedStateInvocationIdentity {
                    physical_status: None,
                    ..current
                },
                &install,
                identity,
            ),
            Err(X86SavedStateAccessError::PhysicalStatusMismatch)
        );
        assert_eq!(
            bound.validate_transport(X86KernelTransportBinding::StandardXsave64 {
                len: 831,
                os_enabled_xfeatures: LEGACY_XFEATURES | (1 << 2),
            }),
            Err(X86SavedStateAccessError::KernelTransportMismatch)
        );

        let before = standard_template(832, LEGACY_XFEATURES | (1 << 2));
        let same_transport = install.kernel_transport_binding(&before).unwrap();
        assert_eq!(
            bound.validate_post_read_template(&before, &before, same_transport),
            Ok(())
        );

        let changed_xcr0 = standard_template(832, LEGACY_XFEATURES | (1 << 2) | (1 << 3));
        let changed_xcr0_transport = install.kernel_transport_binding(&changed_xcr0).unwrap();
        assert_eq!(
            bound.validate_post_read_template(&before, &changed_xcr0, changed_xcr0_transport),
            Err(X86SavedStateAccessError::KernelTransportMismatch)
        );

        let mut changed_template = before.clone();
        let X86ExtendedState::StandardXsave64(changed_state) = &mut changed_template else {
            unreachable!("standard fixture changed format")
        };
        changed_state.0[160] = 1;
        let unchanged_transport = install.kernel_transport_binding(&changed_template).unwrap();
        assert_eq!(
            bound.validate_post_read_template(&before, &changed_template, unchanged_transport,),
            Err(X86SavedStateAccessError::KernelTemplateChanged)
        );
    }

    #[cfg(all(feature = "memory", feature = "notifier"))]
    #[test]
    fn kernel_template_rejects_install_with_unsupported_component() {
        let identity = install_identity(21);
        let install = standard_layout(identity, 0x1000, (1 << 2, 576, 256));
        assert_eq!(
            install.kernel_transport_binding(&standard_template(832, LEGACY_XFEATURES)),
            Err(X86StateMergeError::KernelComponentMaskMismatch)
        );
    }

    #[cfg(all(feature = "memory", feature = "notifier"))]
    #[test]
    fn fxsave_install_requires_exact_legacy_contract_and_transport() {
        let identity = install_identity(31);
        let install = X86SavedStateInstallLayout::from_untrusted_tracee_metadata(
            identity,
            X86SavedStateInstruction::Fxsave64,
            0x1000,
            FXSAVE64_BYTES,
            FXSAVE64_BYTES,
            LEGACY_XFEATURES,
            &[],
        )
        .expect("exact FXSAVE64 install contract");
        assert_eq!(
            install.kernel_transport_binding(&standard_template(832, LEGACY_XFEATURES)),
            Err(X86StateMergeError::FormatMismatch)
        );
        assert_eq!(
            X86SavedStateInstallLayout::from_untrusted_tracee_metadata(
                identity,
                X86SavedStateInstruction::Fxsave64,
                0x1000,
                FXSAVE64_BYTES,
                FXSAVE64_BYTES,
                LEGACY_XFEATURES | (1 << 2),
                &[(1 << 2, 576, 256)],
            )
            .unwrap_err(),
            X86StateMergeError::InvalidInstructionContract
        );
    }
}

/// Floating point registers.
#[cfg(target_arch = "aarch64")]
#[derive(Debug, Copy, Clone, Eq, PartialEq, Hash)]
#[allow(missing_docs)]
pub struct FpRegs {
    pub vregs: [u128; 32],
    pub fpsr: u32,
    pub fpcr: u32,
    __reserved: [u32; 2],
}
