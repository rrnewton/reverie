/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! Register-frame support for syscall events injected by a binary rewriter.

use reverie::Errno;
use reverie::syscalls::Syscall;
use reverie::syscalls::SyscallArgs;
use reverie::syscalls::Sysno;

/// The register frame produced by e9tool's `state` call-trampoline argument.
///
/// The ptrace controller and e9patch's in-process AOT dispatcher share this
/// exact layout. Backends opt into the fallback trap ABI with
/// [`crate::TracerBuilder::injected_syscall_trap`].
// TODO-HUMAN-REVIEW(PR-102): Review the e9tool state-frame syscall ABI.
// TODO-HUMAN-REVIEW(PR-264): Review exposing the existing e9tool state frame
// to the in-process AOT dispatcher.
// AUTONOMOUS-BOT-IMPLEMENTED
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(C)]
pub struct InjectedSyscallFrame {
    flags: u64,
    r15: u64,
    r14: u64,
    r13: u64,
    r12: u64,
    r11: u64,
    r10: u64,
    r9: u64,
    r8: u64,
    rdi: u64,
    rsi: u64,
    rbp: u64,
    rbx: u64,
    rdx: u64,
    rcx: u64,
    rax: u64,
    rsp: u64,
    rip: u64,
}

const _: () = assert!(core::mem::size_of::<InjectedSyscallFrame>() == 18 * 8);
const _: () = assert!(core::mem::offset_of!(InjectedSyscallFrame, flags) == 0);
const _: () = assert!(core::mem::offset_of!(InjectedSyscallFrame, rax) == 15 * 8);
const _: () = assert!(core::mem::offset_of!(InjectedSyscallFrame, rip) == 17 * 8);

/// Stable LiteInst wire tag for a trampoline-owned extended-state image.
///
/// This remains an integer newtype so reading an untrusted tracee record never
/// constructs an invalid Rust enum discriminant.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[repr(transparent)]
pub(crate) struct LiteinstSavedXstateFormat(u64);

impl LiteinstSavedXstateFormat {
    pub(crate) const UNAVAILABLE: Self = Self(0);
    pub(crate) const FXSAVE64: Self = Self(1);
    pub(crate) const XSAVE64_STANDARD: Self = Self(2);

    pub(crate) const fn from_raw(raw: u64) -> Self {
        Self(raw)
    }

    pub(crate) const fn raw(self) -> u64 {
        self.0
    }

    pub(crate) const fn required_alignment(self) -> Option<u64> {
        if self.0 == Self::FXSAVE64.0 {
            Some(16)
        } else if self.0 == Self::XSAVE64_STANDARD.0 {
            Some(64)
        } else {
            None
        }
    }
}

pub(crate) const LITEINST_SAVED_XSTATE_COMPONENT_CAPACITY: usize = 8;

/// One authenticated non-legacy component in a standard XSAVE image.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[repr(C)]
pub(crate) struct LiteinstSavedXstateComponent {
    xfeature: u64,
    offset: u64,
    size: u64,
}

impl LiteinstSavedXstateComponent {
    pub(crate) const UNAVAILABLE: Self = Self {
        xfeature: 0,
        offset: 0,
        size: 0,
    };

    pub(crate) const fn from_raw(xfeature: u64, offset: u64, size: u64) -> Self {
        Self {
            xfeature,
            offset,
            size,
        }
    }

    pub(crate) const fn xfeature(self) -> u64 {
        self.xfeature
    }

    pub(crate) const fn offset(self) -> u64 {
        self.offset
    }

    pub(crate) const fn size(self) -> u64 {
        self.size
    }
}

/// Address-independent saved-state layout authenticated at hook installation.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[repr(C)]
pub(crate) struct LiteinstSavedXstateLayout {
    allocation_len: u64,
    image_len: u64,
    mask: u64,
    format: LiteinstSavedXstateFormat,
    component_count: u64,
    components: [LiteinstSavedXstateComponent; LITEINST_SAVED_XSTATE_COMPONENT_CAPACITY],
}

impl LiteinstSavedXstateLayout {
    pub(crate) const UNAVAILABLE: Self = Self {
        allocation_len: 0,
        image_len: 0,
        mask: 0,
        format: LiteinstSavedXstateFormat::UNAVAILABLE,
        component_count: 0,
        components: [LiteinstSavedXstateComponent::UNAVAILABLE;
            LITEINST_SAVED_XSTATE_COMPONENT_CAPACITY],
    };

    pub(crate) const fn from_raw(
        allocation_len: u64,
        image_len: u64,
        mask: u64,
        format: u64,
        component_count: u64,
        components: [LiteinstSavedXstateComponent; LITEINST_SAVED_XSTATE_COMPONENT_CAPACITY],
    ) -> Self {
        Self {
            allocation_len,
            image_len,
            mask,
            format: LiteinstSavedXstateFormat::from_raw(format),
            component_count,
            components,
        }
    }

    pub(crate) const fn len(self) -> u64 {
        self.allocation_len
    }

    pub(crate) const fn is_empty(self) -> bool {
        self.allocation_len == 0
    }

    pub(crate) const fn image_len(self) -> u64 {
        self.image_len
    }

    pub(crate) const fn mask(self) -> u64 {
        self.mask
    }

    pub(crate) const fn format(self) -> LiteinstSavedXstateFormat {
        self.format
    }

    pub(crate) fn components(&self) -> Option<&[LiteinstSavedXstateComponent]> {
        let count = usize::try_from(self.component_count).ok()?;
        self.components.get(..count)
    }

    /// Rejects malformed wire layouts using only authenticated tracee metadata.
    pub(crate) fn is_well_formed(self) -> bool {
        if self.format.0 == LiteinstSavedXstateFormat::FXSAVE64.0 {
            self.allocation_len == 512
                && self.image_len == 512
                && self.mask == 0b11
                && self.component_count == 0
                && self
                    .components
                    .iter()
                    .all(|component| *component == LiteinstSavedXstateComponent::UNAVAILABLE)
        } else if self.format.0 == LiteinstSavedXstateFormat::XSAVE64_STANDARD.0 {
            let Some(rounded_image_len) = self.image_len.checked_add(63).map(|value| value & !63)
            else {
                return false;
            };
            if self.image_len < 576
                || self.allocation_len != rounded_image_len
                || self.mask & 0b11 != 0b11
            {
                return false;
            }
            let Some(components) = self.components() else {
                return false;
            };
            if self.components[components.len()..]
                .iter()
                .any(|component| *component != LiteinstSavedXstateComponent::UNAVAILABLE)
            {
                return false;
            }
            let mut seen = 0_u64;
            let mut extent = 576_u64;
            let mut previous_xfeature = 0_u64;
            for (index, component) in components.iter().enumerate() {
                if !component.xfeature.is_power_of_two()
                    || component.xfeature & 0b11 != 0
                    || component.xfeature & self.mask == 0
                    || seen & component.xfeature != 0
                    || component.xfeature <= previous_xfeature
                    || component.offset < 576
                    || component.size == 0
                {
                    return false;
                }
                let Some(end) = component.offset.checked_add(component.size) else {
                    return false;
                };
                if end > self.image_len
                    || components[..index].iter().any(|prior| {
                        let prior_end = prior.offset + prior.size;
                        component.offset < prior_end && prior.offset < end
                    })
                {
                    return false;
                }
                seen |= component.xfeature;
                previous_xfeature = component.xfeature;
                extent = extent.max(end);
            }
            seen == self.mask & !0b11 && extent == self.image_len
        } else {
            self.allocation_len == 0
                && self.image_len == 0
                && self.mask == 0
                && self.format.0 == LiteinstSavedXstateFormat::UNAVAILABLE.0
                && self.component_count == 0
                && self
                    .components
                    .iter()
                    .all(|component| *component == LiteinstSavedXstateComponent::UNAVAILABLE)
        }
    }

    /// Computes `align_down(live_r12, alignment) - len` with checked arithmetic.
    pub(crate) fn expected_address(self, live_r12: u64) -> Option<u64> {
        if !self.is_well_formed() || self.is_empty() {
            return None;
        }
        let alignment = self.format.required_alignment()?;
        let aligned = live_r12 & !(alignment - 1);
        aligned.checked_sub(self.allocation_len)
    }
}

/// One LiteInst invocation's tracee-addressed saved extended-state image.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[repr(C)]
pub(crate) struct LiteinstSavedXstateDescriptor {
    address: u64,
    len: u64,
    mask: u64,
    format: LiteinstSavedXstateFormat,
}

impl LiteinstSavedXstateDescriptor {
    pub(crate) const UNAVAILABLE: Self = Self {
        address: 0,
        len: 0,
        mask: 0,
        format: LiteinstSavedXstateFormat::UNAVAILABLE,
    };

    pub(crate) const fn address(self) -> u64 {
        self.address
    }

    pub(crate) const fn len(self) -> u64 {
        self.len
    }

    pub(crate) const fn is_empty(self) -> bool {
        self.len == 0
    }

    pub(crate) const fn mask(self) -> u64 {
        self.mask
    }

    pub(crate) const fn format(self) -> LiteinstSavedXstateFormat {
        self.format
    }

    /// Requires both the authenticated hook layout and the live R12 geometry.
    pub(crate) fn matches(self, expected: LiteinstSavedXstateLayout, live_r12: u64) -> bool {
        expected.is_well_formed()
            && self.len == expected.len()
            && self.mask == expected.mask()
            && self.format == expected.format()
            && expected.expected_address(live_r12) == Some(self.address)
    }
}

/// LiteInst's extension of the legacy 144-byte injected-syscall frame.
///
/// The frame remains at offset zero for e9 ABI compatibility. Only LiteInst
/// callers may read the descriptor tail.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(C)]
pub(crate) struct LiteinstInjectedSyscallEnvelope {
    frame: InjectedSyscallFrame,
    saved_xstate: LiteinstSavedXstateDescriptor,
}

impl LiteinstInjectedSyscallEnvelope {
    pub(crate) const fn frame(&self) -> &InjectedSyscallFrame {
        &self.frame
    }

    pub(crate) fn frame_mut(&mut self) -> &mut InjectedSyscallFrame {
        &mut self.frame
    }

    pub(crate) const fn saved_xstate(&self) -> LiteinstSavedXstateDescriptor {
        self.saved_xstate
    }
}

const _: () = assert!(core::mem::size_of::<LiteinstSavedXstateFormat>() == 8);
const _: () = assert!(core::mem::size_of::<LiteinstSavedXstateComponent>() == 24);
const _: () = assert!(core::mem::offset_of!(LiteinstSavedXstateComponent, xfeature) == 0);
const _: () = assert!(core::mem::offset_of!(LiteinstSavedXstateComponent, offset) == 8);
const _: () = assert!(core::mem::offset_of!(LiteinstSavedXstateComponent, size) == 16);
const _: () = assert!(core::mem::offset_of!(LiteinstSavedXstateLayout, allocation_len) == 0);
const _: () = assert!(core::mem::offset_of!(LiteinstSavedXstateLayout, image_len) == 8);
const _: () = assert!(core::mem::offset_of!(LiteinstSavedXstateLayout, mask) == 16);
const _: () = assert!(core::mem::offset_of!(LiteinstSavedXstateLayout, format) == 24);
const _: () = assert!(core::mem::offset_of!(LiteinstSavedXstateLayout, component_count) == 32);
const _: () = assert!(core::mem::offset_of!(LiteinstSavedXstateLayout, components) == 40);
const _: () = assert!(core::mem::size_of::<LiteinstSavedXstateLayout>() == 232);
const _: () = assert!(core::mem::size_of::<LiteinstSavedXstateDescriptor>() == 32);
const _: () = assert!(core::mem::offset_of!(LiteinstSavedXstateDescriptor, address) == 0);
const _: () = assert!(core::mem::offset_of!(LiteinstSavedXstateDescriptor, len) == 8);
const _: () = assert!(core::mem::offset_of!(LiteinstSavedXstateDescriptor, mask) == 16);
const _: () = assert!(core::mem::offset_of!(LiteinstSavedXstateDescriptor, format) == 24);
const _: () = assert!(core::mem::offset_of!(LiteinstInjectedSyscallEnvelope, frame) == 0);
const _: () = assert!(core::mem::offset_of!(LiteinstInjectedSyscallEnvelope, saved_xstate) == 144);
const _: () = assert!(core::mem::size_of::<LiteinstInjectedSyscallEnvelope>() == 176);

impl InjectedSyscallFrame {
    const FLAGS_OF: u64 = 0x0001;
    const FLAGS_CF: u64 = 0x0100;
    const FLAGS_PF: u64 = 0x0400;
    const FLAGS_AF: u64 = 0x1000;
    const FLAGS_ZF: u64 = 0x4000;
    const FLAGS_SF: u64 = 0x8000;

    const RFLAGS_CF: u64 = 0x0001;
    const RFLAGS_PF: u64 = 0x0004;
    const RFLAGS_AF: u64 = 0x0010;
    const RFLAGS_ZF: u64 = 0x0040;
    const RFLAGS_SF: u64 = 0x0080;
    const RFLAGS_OF: u64 = 0x0800;
    const STATUS_RFLAGS: u64 = Self::RFLAGS_CF
        | Self::RFLAGS_PF
        | Self::RFLAGS_AF
        | Self::RFLAGS_ZF
        | Self::RFLAGS_SF
        | Self::RFLAGS_OF;

    // TODO-HUMAN-REVIEW(PR-264): Review the compact public e9tool-frame
    // syscall-number accessor used by bounded instrumentation stacks.
    /// Returns the syscall number without materializing the full [`Syscall`]
    /// enum. Instrumentation bridges use this compact accessor on bounded
    /// trampoline stacks.
    pub fn syscall_number(&self) -> Sysno {
        Sysno::from(self.rax as i32)
    }

    /// Returns the unvalidated Linux syscall number from the runtime frame.
    ///
    /// Strict all-syscall filters must classify unknown and x32 values before
    /// calling [`Self::syscall_number`], whose legacy typed conversion panics
    /// for values absent from `Sysno`.
    pub fn raw_syscall_number(&self) -> u64 {
        self.rax
    }

    /// Decodes the syscall stored in this e9tool frame.
    pub fn syscall(&self) -> Syscall {
        Syscall::from_raw(
            self.syscall_number(),
            SyscallArgs::new(
                self.rdi as usize,
                self.rsi as usize,
                self.rdx as usize,
                self.r10 as usize,
                self.r8 as usize,
                self.r9 as usize,
            ),
        )
    }

    /// Returns the six raw syscall arguments in Linux x86-64 ABI order.
    pub fn raw_args(&self) -> [u64; 6] {
        [self.rdi, self.rsi, self.rdx, self.r10, self.r8, self.r9]
    }

    /// Returns the address of the syscall instruction replaced by e9tool.
    pub fn instruction_pointer(&self) -> u64 {
        self.rip
    }

    /// Applies the architectural `RCX`/`R11` clobbers of a native syscall.
    pub fn emulate_syscall_entry(&mut self, trap_rflags: u64) {
        // A native x86-64 syscall places the continuation RIP in RCX and the
        // pre-syscall flags in R11 before seccomp delivers its ptrace stop.
        // The replacement trampoline bypasses that instruction, so reproduce
        // those architectural clobbers in the frame restored by e9tool.
        self.rcx = self.rip + 2;
        self.r11 = self.native_rflags(trap_rflags);
    }

    /// Stores a syscall result for e9tool to restore into guest `RAX`.
    pub fn set_result(&mut self, result: i64) {
        self.rax = result as u64;
    }

    pub(crate) fn copy_to_user_regs(&self, regs: &mut libc::user_regs_struct) {
        regs.r15 = self.r15;
        regs.r14 = self.r14;
        regs.r13 = self.r13;
        regs.r12 = self.r12;
        regs.r11 = self.r11;
        regs.r10 = self.r10;
        regs.r9 = self.r9;
        regs.r8 = self.r8;
        regs.rdi = self.rdi;
        regs.rsi = self.rsi;
        regs.rbp = self.rbp;
        regs.rbx = self.rbx;
        regs.rdx = self.rdx;
        regs.rcx = self.rcx;
        regs.rax = self.rax;
        regs.orig_rax = self.rax;
        regs.rsp = self.rsp;
        regs.rip = self.rip + 2;
        regs.eflags = self.native_rflags(regs.eflags);
    }

    // TODO-HUMAN-REVIEW(PR-269): Review exposing the
    // complete AOT register view to the in-process generic Tool host.
    /// Returns the register view a Tool observes at this rewritten syscall.
    ///
    /// `trap_rflags` is the native flags value captured by the AOT call bridge
    /// before its provenance checks modify flags.
    pub fn user_regs(&self, trap_rflags: u64) -> libc::user_regs_struct {
        let mut regs = unsafe { core::mem::zeroed::<libc::user_regs_struct>() };
        regs.eflags = trap_rflags;
        self.copy_to_user_regs(&mut regs);
        regs
    }
    // TODO-HUMAN-REVIEW(PR-103): Review representable rewritten-register updates.
    pub(crate) fn validate_user_regs_update(
        current: &libc::user_regs_struct,
        requested: &libc::user_regs_struct,
    ) -> Result<(), Errno> {
        let unsupported = current.orig_rax != requested.orig_rax
            || current.rip != requested.rip
            || current.cs != requested.cs
            || current.ss != requested.ss
            || current.ds != requested.ds
            || current.es != requested.es
            || current.fs != requested.fs
            || current.gs != requested.gs
            || current.fs_base != requested.fs_base
            || current.gs_base != requested.gs_base
            || (current.eflags ^ requested.eflags) & !Self::STATUS_RFLAGS != 0;
        if unsupported {
            Err(Errno::ENOTSUPP)
        } else {
            Ok(())
        }
    }

    pub(crate) fn copy_from_user_regs(&mut self, regs: &libc::user_regs_struct) {
        self.r15 = regs.r15;
        self.r14 = regs.r14;
        self.r13 = regs.r13;
        self.r12 = regs.r12;
        self.r11 = regs.r11;
        self.r10 = regs.r10;
        self.r9 = regs.r9;
        self.r8 = regs.r8;
        self.rdi = regs.rdi;
        self.rsi = regs.rsi;
        self.rbp = regs.rbp;
        self.rbx = regs.rbx;
        self.rdx = regs.rdx;
        self.rcx = regs.rcx;
        self.rax = regs.rax;
        self.rsp = regs.rsp;
        self.flags = Self::e9_flags(regs.eflags);
        // e9tool documents RIP as read-only in this frame. Control flow still
        // returns to the instruction following the replaced syscall.
    }

    // TODO-HUMAN-REVIEW(PR-269): Review writable AOT
    // register updates from the in-process generic Guest implementation.
    /// Applies a Tool-requested register update when the e9tool frame can
    /// represent it, rejecting control-flow and segment changes.
    pub fn update_user_regs(
        &mut self,
        requested: &libc::user_regs_struct,
        trap_rflags: u64,
    ) -> Result<(), Errno> {
        let current = self.user_regs(trap_rflags);
        Self::validate_user_regs_update(&current, requested)?;
        self.copy_from_user_regs(requested);
        Ok(())
    }

    fn native_rflags(&self, base: u64) -> u64 {
        let mut flags = base & !Self::STATUS_RFLAGS;
        for (e9, native) in [
            (Self::FLAGS_CF, Self::RFLAGS_CF),
            (Self::FLAGS_PF, Self::RFLAGS_PF),
            (Self::FLAGS_AF, Self::RFLAGS_AF),
            (Self::FLAGS_ZF, Self::RFLAGS_ZF),
            (Self::FLAGS_SF, Self::RFLAGS_SF),
            (Self::FLAGS_OF, Self::RFLAGS_OF),
        ] {
            if self.flags & e9 != 0 {
                flags |= native;
            }
        }
        flags
    }

    fn e9_flags(flags: u64) -> u64 {
        let mut e9 = 0;
        for (native, encoded) in [
            (Self::RFLAGS_CF, Self::FLAGS_CF),
            (Self::RFLAGS_PF, Self::FLAGS_PF),
            (Self::RFLAGS_AF, Self::FLAGS_AF),
            (Self::RFLAGS_ZF, Self::FLAGS_ZF),
            (Self::RFLAGS_SF, Self::FLAGS_SF),
            (Self::RFLAGS_OF, Self::FLAGS_OF),
        ] {
            if flags & native != 0 {
                e9 |= encoded;
            }
        }
        e9
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame() -> InjectedSyscallFrame {
        InjectedSyscallFrame {
            flags: InjectedSyscallFrame::FLAGS_ZF | InjectedSyscallFrame::FLAGS_PF,
            r15: 1,
            r14: 2,
            r13: 3,
            r12: 4,
            r11: 5,
            r10: 6,
            r9: 7,
            r8: 8,
            rdi: 9,
            rsi: 10,
            rbp: 11,
            rbx: 12,
            rdx: 13,
            rcx: 14,
            rax: libc::SYS_write as u64,
            rsp: 16,
            rip: 0x401000,
        }
    }

    fn standard_xsave_layout() -> LiteinstSavedXstateLayout {
        let mut components =
            [LiteinstSavedXstateComponent::UNAVAILABLE; LITEINST_SAVED_XSTATE_COMPONENT_CAPACITY];
        for (destination, component) in components.iter_mut().zip([
            LiteinstSavedXstateComponent::from_raw(1 << 2, 576, 256),
            LiteinstSavedXstateComponent::from_raw(1 << 5, 1_088, 64),
            LiteinstSavedXstateComponent::from_raw(1 << 6, 1_152, 512),
            LiteinstSavedXstateComponent::from_raw(1 << 7, 1_664, 1_024),
            LiteinstSavedXstateComponent::from_raw(1 << 9, 2_688, 8),
        ]) {
            *destination = component;
        }
        LiteinstSavedXstateLayout::from_raw(
            2_752,
            2_696,
            0x2e7,
            LiteinstSavedXstateFormat::XSAVE64_STANDARD.raw(),
            5,
            components,
        )
    }

    #[test]
    fn frame_matches_e9tool_state_layout() {
        assert_eq!(core::mem::size_of::<InjectedSyscallFrame>(), 18 * 8);
        assert_eq!(core::mem::offset_of!(InjectedSyscallFrame, rax), 15 * 8);
        assert_eq!(core::mem::offset_of!(InjectedSyscallFrame, rip), 17 * 8);
        assert_eq!(frame().syscall_number(), Sysno::write);
    }

    #[test]
    fn liteinst_envelope_preserves_prefix_and_checked_xstate_geometry() {
        let layout = standard_xsave_layout();
        let descriptor = LiteinstSavedXstateDescriptor {
            address: 0x7fff_e500,
            len: layout.len(),
            mask: layout.mask(),
            format: layout.format(),
        };
        let mut envelope = LiteinstInjectedSyscallEnvelope {
            frame: frame(),
            saved_xstate: descriptor,
        };
        assert_eq!(envelope.frame().instruction_pointer(), 0x401000);
        envelope.frame_mut().set_result(7);
        assert_eq!(envelope.frame().raw_syscall_number(), 7);
        assert_eq!(envelope.saved_xstate(), descriptor);
        assert!(descriptor.matches(layout, 0x7fff_eff8));
        assert!(!descriptor.matches(layout, 0x7fff_efbf));
        assert!(
            !LiteinstSavedXstateDescriptor::UNAVAILABLE
                .matches(LiteinstSavedXstateLayout::UNAVAILABLE, 0x7fff_eff8)
        );
    }

    #[test]
    fn liteinst_xstate_wire_admission_rejects_every_noncanonical_shape() {
        let valid = standard_xsave_layout();
        assert!(valid.is_well_formed());

        let mut out_of_order = valid;
        out_of_order.components.swap(0, 1);
        assert!(!out_of_order.is_well_formed());

        let mut missing_coverage = valid;
        missing_coverage.component_count = 4;
        missing_coverage.components[4] = LiteinstSavedXstateComponent::UNAVAILABLE;
        assert!(!missing_coverage.is_well_formed());

        let mut duplicate = valid;
        duplicate.components[1] = duplicate.components[0];
        assert!(!duplicate.is_well_formed());

        let mut overlap = valid;
        overlap.components[1].offset = 600;
        assert!(!overlap.is_well_formed());

        let mut nonzero_unused_tail = valid;
        nonzero_unused_tail.components[5] =
            LiteinstSavedXstateComponent::from_raw(1 << 10, 2_696, 8);
        assert!(!nonzero_unused_tail.is_well_formed());

        let mut count_overflow = valid;
        count_overflow.component_count = LITEINST_SAVED_XSTATE_COMPONENT_CAPACITY as u64 + 1;
        assert!(!count_overflow.is_well_formed());

        let mut rounded_length_mismatch = valid;
        rounded_length_mismatch.allocation_len -= 64;
        assert!(!rounded_length_mismatch.is_well_formed());

        let mut rounding_overflow = valid;
        rounding_overflow.image_len = u64::MAX;
        assert!(!rounding_overflow.is_well_formed());

        let mut unavailable_contamination = LiteinstSavedXstateLayout::UNAVAILABLE;
        unavailable_contamination.components[0] =
            LiteinstSavedXstateComponent::from_raw(1 << 2, 576, 256);
        assert!(!unavailable_contamination.is_well_formed());

        let descriptor = LiteinstSavedXstateDescriptor {
            address: 0x7fff_e500,
            len: valid.len(),
            mask: valid.mask(),
            format: valid.format(),
        };
        assert!(!descriptor.matches(valid, valid.len() - 1));
        let mut mismatched_address = descriptor;
        mismatched_address.address += 1;
        assert!(!mismatched_address.matches(valid, 0x7fff_eff8));
    }

    #[test]
    fn syscall_entry_clobbers_match_x86_64() {
        let mut frame = frame();
        frame.emulate_syscall_entry(0x202);
        assert_eq!(frame.rcx, 0x401002);
        assert_eq!(frame.r11, 0x246);
    }

    #[test]
    fn user_register_round_trip_preserves_writable_fields() {
        let mut frame = frame();
        frame.emulate_syscall_entry(0x202);
        let mut regs = frame.user_regs(0x202);
        assert_eq!(regs.orig_rax, libc::SYS_write as u64);
        assert_eq!(regs.rip, 0x401002);
        assert_eq!(regs.eflags, 0x246);

        regs.rax = 99;
        regs.rdi = 42;
        regs.eflags = 0x203;
        frame.update_user_regs(&regs, 0x202).unwrap();
        assert_eq!(frame.rax, 99);
        assert_eq!(frame.rdi, 42);
        assert_eq!(frame.rip, 0x401000);
        assert_eq!(frame.flags, InjectedSyscallFrame::FLAGS_CF);
    }

    #[test]
    fn rejects_register_updates_the_e9_frame_cannot_represent() {
        let frame = frame();
        let mut current = unsafe { core::mem::zeroed::<libc::user_regs_struct>() };
        current.eflags = 0x202;
        frame.copy_to_user_regs(&mut current);
        let mut requested = current;
        requested.rip += 4;
        assert_eq!(
            InjectedSyscallFrame::validate_user_regs_update(&current, &requested),
            Err(Errno::ENOTSUPP)
        );
    }

    #[test]
    fn shared_frame_accepts_stack_pointer_updates_for_e9patch() {
        let mut updated = frame();
        let mut current = unsafe { core::mem::zeroed::<libc::user_regs_struct>() };
        current.eflags = 0x202;
        updated.copy_to_user_regs(&mut current);
        let mut requested = current;
        requested.rsp += 8;

        updated.update_user_regs(&requested, 0x202).unwrap();
        assert_eq!(updated.rsp, frame().rsp + 8);
    }
}
