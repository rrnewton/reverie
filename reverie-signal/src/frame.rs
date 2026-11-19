/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! x86-64 signal-frame construction, lifted from the KVM backend's Linux
//! emulation and parameterised by the *presented* xstate description.
//! Register and XSAVE state cross this module's boundary as plain values
//! and byte slices; converting to a backend's native register file (and
//! reading host facts such as the MXCSR mask) is the backend's job.

use std::fmt;

pub const SIGCONTEXT_SIZE: usize = 256;
pub const UCONTEXT_SIZE: usize = 304;
pub const SIGINFO_SIZE: usize = 128;
pub const LEGACY_FPSTATE_SIZE: usize = 512;
pub const RT_SIGFRAME_SIZE: usize = 8 + UCONTEXT_SIZE + SIGINFO_SIZE;

pub const SIGNAL_RED_ZONE: u64 = 128;
pub const SIGNAL_STACK_ALIGNMENT: u64 = 16;
pub const XSAVE_ALIGNMENT: u64 = 64;

pub const USER_CODE_SELECTOR: u16 = 0x23;
pub const USER_DATA_SELECTOR: u16 = 0x1b;
pub const SIGNAL_UCONTEXT_FLAGS: u64 = 0x1 | 0x2 | 0x4;

const FP_XSTATE_MAGIC1: u32 = 0x4650_5853;
const FP_XSTATE_MAGIC2: u32 = 0x4650_5845;
const XFEATURE_X87: u64 = 1 << 0;
const XFEATURE_SSE: u64 = 1 << 1;
const XFEATURE_YMM: u64 = 1 << 2;
/// xfeatures this builder knows how to lay out in standard format.
pub const SUPPORTED_XFEATURES: u64 = XFEATURE_X87 | XFEATURE_SSE | XFEATURE_YMM;
// arch/x86/include/asm/sighandling.h in Linux v7.1, lines 11-14.
const FIX_EFLAGS: u64 = (1 << 18)
    | (1 << 16)
    | (1 << 11)
    | (1 << 10)
    | (1 << 8)
    | (1 << 7)
    | (1 << 6)
    | (1 << 4)
    | (1 << 2)
    | 1;
const DEFAULT_MXCSR_FEATURE_MASK: u32 = 0x0000_ffbf;

const MXCSR_MASK_OFFSET: usize = 28;
const XMM_OFFSET: usize = 160;
const XMM_END: usize = XMM_OFFSET + 16 * 16;
const FP_SW_RESERVED_OFFSET: usize = 464;
const XSTATE_BV_OFFSET: usize = 512;
const XCOMP_BV_OFFSET: usize = 520;
const XSTATE_RESERVED_OFFSET: usize = 528;
const XSTATE_RESERVED_END: usize = 576;

/// The presented XSAVE shape: feature bits and standard-format size in
/// bytes. This comes from the virtual CPU description, never the host.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct XstateDesc {
    pub features: u64,
    pub size: usize,
}

impl XstateDesc {
    /// The fixed x87/SSE/YMM shape the KVM backend presents today.
    pub const X87_SSE_YMM: Self = Self {
        features: SUPPORTED_XFEATURES,
        size: 832,
    };

    /// Signal-frame image size: the xstate plus FP_XSTATE_MAGIC2.
    pub fn signal_size(self) -> usize {
        self.size + 4
    }
    pub fn is_supported(self) -> bool {
        self.features & !SUPPORTED_XFEATURES == 0 && self.size >= XSTATE_RESERVED_END
    }
}

/// A malformed signal frame or unsupported XSAVE feature set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameError {
    AddressOverflow,
    InvalidSelectors,
    InvalidAddress,
    InvalidXsaveMetadata,
    InvalidXsaveFeatures,
    InvalidXsaveReserved,
    InvalidMxcsr,
}

impl fmt::Display for FrameError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::AddressOverflow => "signal stack address overflow",
            Self::InvalidSelectors => "signal frame has an invalid user code or stack selector",
            Self::InvalidAddress => "signal frame has a non-canonical address",
            Self::InvalidXsaveMetadata => "signal XSAVE software metadata is invalid",
            Self::InvalidXsaveFeatures => "signal XSAVE feature mask is unsupported",
            Self::InvalidXsaveReserved => "signal XSAVE reserved fields are nonzero",
            Self::InvalidMxcsr => "signal XSAVE MXCSR contains unsupported bits",
        })
    }
}
impl std::error::Error for FrameError {}

fn write_u16(bytes: &mut [u8], off: usize, v: u16) {
    bytes[off..off + 2].copy_from_slice(&v.to_le_bytes());
}
fn write_u32(bytes: &mut [u8], off: usize, v: u32) {
    bytes[off..off + 4].copy_from_slice(&v.to_le_bytes());
}
fn write_u64(bytes: &mut [u8], off: usize, v: u64) {
    bytes[off..off + 8].copy_from_slice(&v.to_le_bytes());
}
fn read_u16(bytes: &[u8], off: usize) -> u16 {
    u16::from_le_bytes(bytes[off..off + 2].try_into().unwrap())
}
fn read_u32(bytes: &[u8], off: usize) -> u32 {
    u32::from_le_bytes(bytes[off..off + 4].try_into().unwrap())
}
fn read_u64(bytes: &[u8], off: usize) -> u64 {
    u64::from_le_bytes(bytes[off..off + 8].try_into().unwrap())
}
fn align_down(v: u64, align: u64) -> u64 {
    v & !(align - 1)
}
fn align_down_with_remainder(v: u64, align: u64, rem: u64) -> Option<u64> {
    let base = v.checked_sub(rem)?;
    Some(align_down(base, align) + rem)
}
fn is_canonical_user_address(addr: u64) -> bool {
    addr < (1u64 << 47)
}

/// General registers at a signal boundary, in `sigcontext` order groups.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Regs {
    pub r8: u64,
    pub r9: u64,
    pub r10: u64,
    pub r11: u64,
    pub r12: u64,
    pub r13: u64,
    pub r14: u64,
    pub r15: u64,
    pub rdi: u64,
    pub rsi: u64,
    pub rbp: u64,
    pub rbx: u64,
    pub rdx: u64,
    pub rax: u64,
    pub rcx: u64,
    pub rsp: u64,
    pub rip: u64,
    pub rflags: u64,
}

/// The x86-64 kernel `sigcontext` embedded in `ucontext`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Sigcontext {
    pub regs: Regs,
    pub cs: u16,
    pub gs: u16,
    pub fs: u16,
    pub ss: u16,
    pub err: u64,
    pub trapno: u64,
    pub oldmask: u64,
    pub cr2: u64,
    pub fpstate: u64,
    pub reserved: [u64; 8],
}

impl Sigcontext {
    pub fn new(regs: Regs, fpstate: u64, oldmask: u64) -> Self {
        Self {
            regs,
            cs: USER_CODE_SELECTOR,
            gs: 0,
            fs: 0,
            ss: USER_DATA_SELECTOR,
            err: 0,
            trapno: 0,
            oldmask,
            cr2: 0,
            fpstate,
            reserved: [0; 8],
        }
    }

    pub fn encode(self) -> [u8; SIGCONTEXT_SIZE] {
        let mut bytes = [0; SIGCONTEXT_SIZE];
        let r = self.regs;
        for (i, v) in [
            r.r8, r.r9, r.r10, r.r11, r.r12, r.r13, r.r14, r.r15, r.rdi, r.rsi, r.rbp, r.rbx,
            r.rdx, r.rax, r.rcx, r.rsp, r.rip, r.rflags,
        ]
        .into_iter()
        .enumerate()
        {
            write_u64(&mut bytes, i * 8, v);
        }
        write_u16(&mut bytes, 144, self.cs);
        write_u16(&mut bytes, 146, self.gs);
        write_u16(&mut bytes, 148, self.fs);
        write_u16(&mut bytes, 150, self.ss);
        for (i, v) in [self.err, self.trapno, self.oldmask, self.cr2, self.fpstate]
            .into_iter()
            .enumerate()
        {
            write_u64(&mut bytes, 152 + i * 8, v);
        }
        for (i, v) in self.reserved.into_iter().enumerate() {
            write_u64(&mut bytes, 192 + i * 8, v);
        }
        bytes
    }

    pub fn decode(bytes: [u8; SIGCONTEXT_SIZE]) -> Self {
        let mut w = [0; 18];
        for (i, x) in w.iter_mut().enumerate() {
            *x = read_u64(&bytes, i * 8);
        }
        let mut reserved = [0; 8];
        for (i, x) in reserved.iter_mut().enumerate() {
            *x = read_u64(&bytes, 192 + i * 8);
        }
        Self {
            regs: Regs {
                r8: w[0],
                r9: w[1],
                r10: w[2],
                r11: w[3],
                r12: w[4],
                r13: w[5],
                r14: w[6],
                r15: w[7],
                rdi: w[8],
                rsi: w[9],
                rbp: w[10],
                rbx: w[11],
                rdx: w[12],
                rax: w[13],
                rcx: w[14],
                rsp: w[15],
                rip: w[16],
                rflags: w[17],
            },
            cs: read_u16(&bytes, 144),
            gs: read_u16(&bytes, 146),
            fs: read_u16(&bytes, 148),
            ss: read_u16(&bytes, 150),
            err: read_u64(&bytes, 152),
            trapno: read_u64(&bytes, 160),
            oldmask: read_u64(&bytes, 168),
            cr2: read_u64(&bytes, 176),
            fpstate: read_u64(&bytes, 184),
            reserved,
        }
    }

    pub fn validate_for_restore(self) -> Result<(), FrameError> {
        if self.cs != USER_CODE_SELECTOR
            || ![USER_DATA_SELECTOR, USER_DATA_SELECTOR & !3].contains(&self.ss)
        {
            return Err(FrameError::InvalidSelectors);
        }
        if !is_canonical_user_address(self.regs.rip)
            || !is_canonical_user_address(self.regs.rsp)
            || self.fpstate != 0 && !is_canonical_user_address(self.fpstate)
        {
            return Err(FrameError::InvalidAddress);
        }
        Ok(())
    }

    /// Registers after rt_sigreturn: the frame controls only FIX_EFLAGS
    /// bits of RFLAGS (Linux signal_64.c); the rest come from `live`.
    pub fn restored_regs(self, live_rflags: u64) -> Regs {
        let mut regs = self.regs;
        regs.rflags = (live_rflags & !FIX_EFLAGS) | (regs.rflags & FIX_EFLAGS);
        regs
    }
}

/// `stack_t` as embedded in `ucontext` (ss_sp, ss_flags, ss_size + pad).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct GuestStack {
    pub sp: u64,
    pub flags: i32,
    pub size: u64,
}

impl GuestStack {
    pub fn encode(self) -> [u8; 24] {
        let mut bytes = [0; 24];
        write_u64(&mut bytes, 0, self.sp);
        bytes[8..12].copy_from_slice(&self.flags.to_le_bytes());
        write_u64(&mut bytes, 16, self.size);
        bytes
    }
    pub fn decode(bytes: [u8; 24]) -> Self {
        Self {
            sp: read_u64(&bytes, 0),
            flags: i32::from_le_bytes(bytes[8..12].try_into().unwrap()),
            size: read_u64(&bytes, 16),
        }
    }
}

/// The 304-byte kernel `ucontext` used in an x86-64 `rt_sigframe`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Ucontext {
    pub flags: u64,
    pub link: u64,
    pub stack: GuestStack,
    pub mcontext: Sigcontext,
    pub sigmask: u64,
}

impl Ucontext {
    pub fn encode(self) -> [u8; UCONTEXT_SIZE] {
        let mut bytes = [0; UCONTEXT_SIZE];
        write_u64(&mut bytes, 0, self.flags);
        write_u64(&mut bytes, 8, self.link);
        bytes[16..40].copy_from_slice(&self.stack.encode());
        bytes[40..296].copy_from_slice(&self.mcontext.encode());
        write_u64(&mut bytes, 296, self.sigmask);
        bytes
    }
    pub fn decode(bytes: [u8; UCONTEXT_SIZE]) -> Self {
        Self {
            flags: read_u64(&bytes, 0),
            link: read_u64(&bytes, 8),
            stack: GuestStack::decode(bytes[16..40].try_into().unwrap()),
            mcontext: Sigcontext::decode(bytes[40..296].try_into().unwrap()),
            sigmask: read_u64(&bytes, 296),
        }
    }
}

/// The fixed prefix of Linux's x86-64 `rt_sigframe`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RtSigframe {
    pub pretcode: u64,
    pub ucontext: Ucontext,
    pub siginfo: [u8; SIGINFO_SIZE],
}

impl RtSigframe {
    pub fn encode(self) -> [u8; RT_SIGFRAME_SIZE] {
        let mut bytes = [0; RT_SIGFRAME_SIZE];
        write_u64(&mut bytes, 0, self.pretcode);
        bytes[8..312].copy_from_slice(&self.ucontext.encode());
        bytes[312..440].copy_from_slice(&self.siginfo);
        bytes
    }
    pub fn decode(bytes: [u8; RT_SIGFRAME_SIZE]) -> Self {
        Self {
            pretcode: read_u64(&bytes, 0),
            ucontext: Ucontext::decode(bytes[8..312].try_into().unwrap()),
            siginfo: bytes[312..440].try_into().unwrap(),
        }
    }
}

/// Guest addresses for an `rt_sigframe` and its aligned XSAVE image.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameLayout {
    pub frame_address: u64,
    pub xsave_address: u64,
}

impl FrameLayout {
    pub fn below(
        stack_top: u64,
        reserve_red_zone: bool,
        desc: XstateDesc,
    ) -> Result<Self, FrameError> {
        let ceiling = stack_top
            .checked_sub(if reserve_red_zone { SIGNAL_RED_ZONE } else { 0 })
            .ok_or(FrameError::AddressOverflow)?;
        // Pack the XSAVE image from the actual ceiling so an exactly-sized
        // alternate stack is not rejected by unrequired padding.
        let xsave_address = align_down(
            ceiling
                .checked_sub(desc.signal_size() as u64)
                .ok_or(FrameError::AddressOverflow)?,
            XSAVE_ALIGNMENT,
        );
        let frame_address = align_down_with_remainder(
            xsave_address
                .checked_sub(RT_SIGFRAME_SIZE as u64)
                .ok_or(FrameError::AddressOverflow)?,
            SIGNAL_STACK_ALIGNMENT,
            8,
        )
        .ok_or(FrameError::AddressOverflow)?;
        Ok(Self {
            frame_address,
            xsave_address,
        })
    }

    /// Linux's `__on_sig_stack`: strictly above the alternate stack base.
    pub fn fits_above(self, lower_bound: u64) -> bool {
        self.frame_address > lower_bound
    }
}

fn feature_mask(hardware_mask: u32) -> u32 {
    match hardware_mask {
        0 => DEFAULT_MXCSR_FEATURE_MASK,
        mask => mask,
    }
}

/// A signal-frame XSAVE image in standard format, sized by the presented
/// [`XstateDesc`] (+4 for FP_XSTATE_MAGIC2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct XsaveImage(Vec<u8>);

impl XsaveImage {
    pub fn bytes(&self) -> &[u8] {
        &self.0
    }

    /// Build the frame image from standard-format xstate bytes (at least
    /// `desc.size` long). XSTATE_BV gains x87|SSE present, as
    /// `save_xstate_epilog` does. With `compacted`, components the header
    /// reports initial are rewritten to architectural initial values
    /// (their save area may hold stale bytes); see the KVM backend's
    /// history for the MXCSR sharp case.
    pub fn from_standard(
        state: &[u8],
        desc: XstateDesc,
        compacted: bool,
    ) -> Result<Self, FrameError> {
        if !desc.is_supported() || state.len() < desc.size {
            return Err(FrameError::InvalidXsaveFeatures);
        }
        let mut bytes = vec![0u8; desc.signal_size()];
        bytes[..desc.size].copy_from_slice(&state[..desc.size]);
        let xstate_bv = read_u64(&bytes, XSTATE_BV_OFFSET);
        if compacted {
            let mxcsr = read_u32(&bytes, 24);
            let mxcsr_mask = read_u32(&bytes, MXCSR_MASK_OFFSET);
            if xstate_bv & XFEATURE_X87 == 0 {
                bytes[..XMM_OFFSET].fill(0);
                write_u32(&mut bytes, 0, 0x37f);
                if xstate_bv & XFEATURE_SSE != 0 {
                    write_u32(&mut bytes, 24, mxcsr);
                }
                write_u32(&mut bytes, MXCSR_MASK_OFFSET, mxcsr_mask);
            }
            if xstate_bv & XFEATURE_SSE == 0 {
                bytes[XMM_OFFSET..XMM_END].fill(0);
                write_u32(&mut bytes, 24, 0x1f80);
                write_u32(&mut bytes, MXCSR_MASK_OFFSET, mxcsr_mask);
            }
            if xstate_bv & XFEATURE_YMM == 0 && desc.size > XSTATE_RESERVED_END {
                bytes[XSTATE_RESERVED_END..desc.size].fill(0);
            }
        }
        write_u64(
            &mut bytes,
            XSTATE_BV_OFFSET,
            xstate_bv | XFEATURE_X87 | XFEATURE_SSE,
        );
        write_u32(&mut bytes, FP_SW_RESERVED_OFFSET, FP_XSTATE_MAGIC1);
        write_u32(
            &mut bytes,
            FP_SW_RESERVED_OFFSET + 4,
            desc.signal_size() as u32,
        );
        write_u64(&mut bytes, FP_SW_RESERVED_OFFSET + 8, desc.features);
        write_u32(&mut bytes, FP_SW_RESERVED_OFFSET + 16, desc.size as u32);
        bytes[FP_SW_RESERVED_OFFSET + 20..512].fill(0);
        bytes[XSTATE_RESERVED_OFFSET..XSTATE_RESERVED_END].fill(0);
        write_u32(&mut bytes, desc.size, FP_XSTATE_MAGIC2);
        Ok(Self(bytes))
    }

    /// Linux's initial user FPU state as standard-format bytes
    /// (`fpu__clear_user_states`): control 0x37f, MXCSR 0x1f80, zero
    /// registers, XSTATE_BV = x87|SSE. `mxcsr_mask` is the backend-read
    /// hardware mask (0 selects the architectural default).
    pub fn initialized(desc: XstateDesc, mxcsr_mask: u32) -> Vec<u8> {
        let mut state = vec![0u8; desc.size];
        write_u32(&mut state, 0, 0x037f);
        write_u32(&mut state, 24, 0x1f80);
        write_u32(&mut state, MXCSR_MASK_OFFSET, mxcsr_mask);
        write_u64(&mut state, XSTATE_BV_OFFSET, XFEATURE_X87 | XFEATURE_SSE);
        state
    }

    /// Image size advertised by a frame's 512-byte legacy prefix.
    /// A zero magic word is Linux's accepted legacy FXSAVE form.
    pub fn restore_size(
        prefix: &[u8; LEGACY_FPSTATE_SIZE],
        desc: XstateDesc,
    ) -> Result<usize, FrameError> {
        match read_u32(prefix, FP_SW_RESERVED_OFFSET) {
            0 => Ok(LEGACY_FPSTATE_SIZE),
            FP_XSTATE_MAGIC1 => {
                let extended = read_u32(prefix, FP_SW_RESERVED_OFFSET + 4) as usize;
                let xstate = read_u32(prefix, FP_SW_RESERVED_OFFSET + 16) as usize;
                if extended != xstate.saturating_add(4)
                    || !(XSTATE_RESERVED_END..=desc.size).contains(&xstate)
                    || extended > desc.signal_size()
                {
                    return Err(FrameError::InvalidXsaveMetadata);
                }
                Ok(extended)
            }
            _ => Err(FrameError::InvalidXsaveMetadata),
        }
    }

    /// Restore standard-format state from a signal frame, as 64-bit
    /// rt_sigreturn does (XRSTOR of the requested features, then init of
    /// the rest). Returns `desc.size` bytes. Some frames Linux accepts
    /// are refused (metadata failures; YMM in fewer bytes than its
    /// standard extent).
    pub fn restore_from_signal(
        bytes: &[u8],
        desc: XstateDesc,
        mxcsr_mask: u32,
    ) -> Result<Vec<u8>, FrameError> {
        if bytes.len() < LEGACY_FPSTATE_SIZE {
            return Err(FrameError::InvalidXsaveMetadata);
        }
        let expected = Self::restore_size(bytes[..LEGACY_FPSTATE_SIZE].try_into().unwrap(), desc)?;
        if bytes.len() != expected {
            return Err(FrameError::InvalidXsaveMetadata);
        }
        let mask = feature_mask(mxcsr_mask);
        if expected == LEGACY_FPSTATE_SIZE || read_u32(bytes, expected - 4) != FP_XSTATE_MAGIC2 {
            // Legacy FXRSTOR, then YMM initialisation.
            if read_u32(bytes, 24) & !mask != 0 {
                return Err(FrameError::InvalidMxcsr);
            }
            let mut state = Self::initialized(desc, mxcsr_mask);
            state[..LEGACY_FPSTATE_SIZE].copy_from_slice(&bytes[..LEGACY_FPSTATE_SIZE]);
            state[FP_SW_RESERVED_OFFSET..512].fill(0);
            write_u64(&mut state, XSTATE_BV_OFFSET, XFEATURE_X87 | XFEATURE_SSE);
            write_u32(&mut state, MXCSR_MASK_OFFSET, mxcsr_mask);
            return Ok(state);
        }
        let xstate_size = expected - 4;
        // YMM's standard-format extent is architecturally fixed (832),
        // whatever larger shape the presentation describes.
        const YMM_STANDARD_EXTENT: usize = 832;
        let xfeatures = read_u64(bytes, FP_SW_RESERVED_OFFSET + 8);
        if xfeatures & !desc.features != 0
            || xfeatures & XFEATURE_YMM != 0 && xstate_size < YMM_STANDARD_EXTENT
        {
            return Err(FrameError::InvalidXsaveFeatures);
        }
        let xstate_bv = read_u64(bytes, XSTATE_BV_OFFSET);
        if xstate_bv & !desc.features != 0 {
            return Err(FrameError::InvalidXsaveFeatures);
        }
        if read_u64(bytes, XCOMP_BV_OFFSET) != 0
            || bytes[XSTATE_RESERVED_OFFSET..XSTATE_RESERVED_END]
                .iter()
                .any(|b| *b != 0)
        {
            return Err(FrameError::InvalidXsaveReserved);
        }
        if xfeatures & (XFEATURE_SSE | XFEATURE_YMM) != 0 && read_u32(bytes, 24) & !mask != 0 {
            return Err(FrameError::InvalidMxcsr);
        }
        let mut state = Self::initialized(desc, mxcsr_mask);
        state[..xstate_size].copy_from_slice(&bytes[..xstate_size]);
        state[FP_SW_RESERVED_OFFSET..512].fill(0);
        let present = xstate_bv & xfeatures;
        if present & XFEATURE_SSE == 0 {
            state[XMM_OFFSET..XMM_END].fill(0);
        }
        if xfeatures & XFEATURE_SSE == 0 {
            write_u32(&mut state, 24, 0x1f80);
        }
        write_u64(&mut state, XSTATE_BV_OFFSET, present | XFEATURE_SSE);
        write_u32(&mut state, MXCSR_MASK_OFFSET, mxcsr_mask);
        Ok(state)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_round_trip_and_layout() {
        let desc = XstateDesc::X87_SSE_YMM;
        let frame = RtSigframe {
            pretcode: 0x7f00,
            ucontext: Ucontext {
                flags: SIGNAL_UCONTEXT_FLAGS,
                mcontext: Sigcontext::new(
                    Regs {
                        rip: 0x401000,
                        rsp: 0x7fff0000,
                        ..Regs::default()
                    },
                    0x5000,
                    0,
                ),
                sigmask: 0x4,
                ..Ucontext::default()
            },
            siginfo: [7u8; SIGINFO_SIZE],
        };
        assert_eq!(RtSigframe::decode(frame.encode()), frame);
        // Kernel-header-pinned offsets (ucontext 304, sigmask at 296).
        let uc = frame.ucontext.encode();
        assert_eq!(uc.len(), 304);
        assert_eq!(read_u64(&uc, 296), 0x4);

        let top = 0x7fff_0000u64;
        let layout = FrameLayout::below(top, true, desc).unwrap();
        assert_eq!(layout.xsave_address % XSAVE_ALIGNMENT, 0);
        assert_eq!(layout.frame_address % 16, 8);
        assert!(layout.frame_address + RT_SIGFRAME_SIZE as u64 <= layout.xsave_address);
        assert!(layout.fits_above(top - 8192));
        // A larger presented xstate needs strictly more room.
        let big = XstateDesc {
            features: desc.features,
            size: 2700,
        };
        let layout_big = FrameLayout::below(top, true, big).unwrap();
        assert!(layout_big.frame_address < layout.frame_address);
    }

    #[test]
    fn xsave_image_parameterised_by_presented_size() {
        for size in [832usize, 2700] {
            let desc = XstateDesc {
                features: SUPPORTED_XFEATURES,
                size,
            };
            let state = XsaveImage::initialized(desc, 0xffbf);
            assert_eq!(state.len(), size);
            let img = XsaveImage::from_standard(&state, desc, false).unwrap();
            assert_eq!(img.bytes().len(), size + 4);
            assert_eq!(read_u32(img.bytes(), size), FP_XSTATE_MAGIC2);
            assert_eq!(
                XsaveImage::restore_size(img.bytes()[..512].try_into().unwrap(), desc).unwrap(),
                size + 4
            );
            let restored = XsaveImage::restore_from_signal(img.bytes(), desc, 0xffbf).unwrap();
            assert_eq!(restored.len(), size);
            assert_eq!(read_u32(&restored, 24), 0x1f80);
        }
        // Unsupported feature bits refuse by name.
        let bad = XstateDesc {
            features: 1 << 9,
            size: 832,
        };
        assert_eq!(
            XsaveImage::from_standard(&vec![0u8; 832], bad, false),
            Err(FrameError::InvalidXsaveFeatures)
        );
    }

    #[test]
    fn validate_and_fixed_eflags() {
        let mut sc = Sigcontext::new(
            Regs {
                rip: 0x401000,
                rsp: 0x7fff0000,
                ..Regs::default()
            },
            0,
            0,
        );
        assert!(sc.validate_for_restore().is_ok());
        sc.cs = 0;
        assert_eq!(sc.validate_for_restore(), Err(FrameError::InvalidSelectors));
        let sc2 = Sigcontext::new(
            Regs {
                rflags: u64::MAX,
                ..Regs::default()
            },
            0,
            0,
        );
        let restored = sc2.restored_regs(0);
        assert_eq!(restored.rflags, FIX_EFLAGS);
    }
}
