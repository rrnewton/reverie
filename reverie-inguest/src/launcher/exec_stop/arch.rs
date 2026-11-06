// Copyright (c) Meta Platforms, Inc. and affiliates.
// Licensed under the BSD-style license in the LICENSE file.

//! Architecture seam for exec-stop mechanics: the injected-syscall
//! stub encodings, register accessors, and extended-state capture
//! that differ between x86-64 and aarch64. Everything above this
//! module (drain, capture, injector transaction, supervisor) is
//! arch-generic; everything below it that is still x86-shaped
//! (register wire codec, physical preparation, filter recipe) is
//! explicitly out of this seam's scope and stays x86-gated at its
//! own modules.

use safeptrace::Regs;

#[cfg(target_arch = "x86_64")]
mod imp {
    use safeptrace::Regs;

    /// `syscall; int3`, written over the first three saved entry bytes.
    pub const PATCH_BYTES: &[u8] = &[0x0f, 0x05, 0xcc];
    /// PC offset of the int3 marker from the patched instruction.
    pub const MARKER_PC_OFFSET: u64 = 3;
    /// `AUDIT_ARCH_X86_64`.
    pub const SYSCALL_AUDIT_ARCH: u32 = 0xc000_003e;

    pub fn instruction_pointer(regs: &Regs) -> u64 {
        regs.rip
    }

    pub fn stack_pointer(regs: &Regs) -> u64 {
        regs.rsp
    }

    pub fn syscall_number(regs: &Regs) -> u64 {
        regs.orig_rax
    }

    pub fn prepare_syscall_regs(entry: &Regs, number: u64, args: [u64; 6]) -> Regs {
        let mut regs = *entry;
        regs.rax = number;
        regs.orig_rax = u64::MAX;
        regs.rdi = args[0];
        regs.rsi = args[1];
        regs.rdx = args[2];
        regs.r10 = args[3];
        regs.r8 = args[4];
        regs.r9 = args[5];
        regs
    }

    pub fn syscall_result(regs: &Regs) -> u64 {
        regs.rax
    }

    pub fn regs_equal(left: &Regs, right: &Regs) -> bool {
        super::super::register_words(left) == super::super::register_words(right)
    }

    /// Complete XSAVE state (`NT_X86_XSTATE`), opaque save/restore bytes.
    #[derive(Clone, Debug, Eq, PartialEq)]
    pub struct ExtendedState(pub safeptrace::XState);

    impl ExtendedState {
        /// The wrapped XSAVE state (PKRU extraction lives there).
        pub fn xstate(&self) -> &safeptrace::XState {
            &self.0
        }
    }

    pub fn capture_extended(task: &safeptrace::Stopped) -> super::super::Result<ExtendedState> {
        Ok(ExtendedState(task.getxstate()?))
    }

    pub fn restore_extended(
        task: &safeptrace::Stopped,
        state: &ExtendedState,
    ) -> super::super::Result<()> {
        Ok(task.setxstate(&state.0)?)
    }
}

#[cfg(target_arch = "aarch64")]
mod imp {
    use safeptrace::Regs;

    /// `svc #0; brk #0`, two aligned A64 instructions filling the
    /// whole 8-byte saved entry window.
    pub const PATCH_BYTES: &[u8] = &[0x01, 0x00, 0x00, 0xd4, 0x00, 0x00, 0x20, 0xd4];
    /// PC offset of the brk marker: svc returns to the next insn.
    pub const MARKER_PC_OFFSET: u64 = 4;
    /// `AUDIT_ARCH_AARCH64`.
    pub const SYSCALL_AUDIT_ARCH: u32 = 0xc000_00b7;

    pub fn instruction_pointer(regs: &Regs) -> u64 {
        regs.pc
    }

    pub fn stack_pointer(regs: &Regs) -> u64 {
        regs.sp
    }

    pub fn syscall_number(regs: &Regs) -> u64 {
        regs.regs[8]
    }

    pub fn prepare_syscall_regs(entry: &Regs, number: u64, args: [u64; 6]) -> Regs {
        let mut regs = *entry;
        regs.regs[8] = number;
        regs.regs[..6].copy_from_slice(&args);
        regs
    }

    pub fn syscall_result(regs: &Regs) -> u64 {
        regs.regs[0]
    }

    pub fn regs_equal(left: &Regs, right: &Regs) -> bool {
        left.regs == right.regs
            && left.sp == right.sp
            && left.pc == right.pc
            && left.pstate == right.pstate
    }

    /// FPSIMD state (`NT_PRFPREG`). SVE/SME streaming state is not
    /// captured by this seam; that is a named follow-up, not a
    /// silent truncation claim.
    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    pub struct ExtendedState(pub safeptrace::FpRegs);

    pub fn capture_extended(task: &safeptrace::Stopped) -> super::super::Result<ExtendedState> {
        Ok(ExtendedState(task.getfpregs()?))
    }

    pub fn restore_extended(
        task: &safeptrace::Stopped,
        state: &ExtendedState,
    ) -> super::super::Result<()> {
        Ok(task.setfpregs(&state.0)?)
    }
}

#[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
compile_error!("exec-stop mechanics support only x86_64 and aarch64");

pub use imp::ExtendedState;
pub use imp::MARKER_PC_OFFSET;
pub use imp::PATCH_BYTES;
pub use imp::SYSCALL_AUDIT_ARCH;
pub use imp::capture_extended;
pub use imp::instruction_pointer;
pub use imp::prepare_syscall_regs;
pub use imp::regs_equal;
pub use imp::restore_extended;
pub use imp::stack_pointer;
pub use imp::syscall_number;
pub use imp::syscall_result;

/// Bytes of original entry code saved before patching, both arches.
pub const SAVED_ENTRY_BYTES: usize = 8;

/// Assemble the patched entry image: stub bytes over the saved prefix.
pub fn patched_entry(original: &[u8]) -> Vec<u8> {
    let mut injected = original.to_vec();
    injected[..PATCH_BYTES.len()].copy_from_slice(PATCH_BYTES);
    injected
}

/// Whether the stopped task sits exactly on the injection marker.
pub fn at_marker(regs: &Regs, entry: &Regs) -> bool {
    instruction_pointer(regs) == instruction_pointer(entry) + MARKER_PC_OFFSET
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stub_encodings_and_marker_offsets() {
        #[cfg(target_arch = "x86_64")]
        {
            assert_eq!(PATCH_BYTES, [0x0f, 0x05, 0xcc]);
            assert_eq!(MARKER_PC_OFFSET, 3);
            assert_eq!(SYSCALL_AUDIT_ARCH, 0xc000_003e);
        }
        #[cfg(target_arch = "aarch64")]
        {
            assert_eq!(
                PATCH_BYTES,
                [0x01, 0x00, 0x00, 0xd4, 0x00, 0x00, 0x20, 0xd4]
            );
            assert_eq!(MARKER_PC_OFFSET, 4);
            assert_eq!(SYSCALL_AUDIT_ARCH, 0xc000_00b7);
        }
        assert!(PATCH_BYTES.len() <= SAVED_ENTRY_BYTES);
        let original = [0xaa; SAVED_ENTRY_BYTES];
        let patched = patched_entry(&original);
        assert_eq!(&patched[..PATCH_BYTES.len()], PATCH_BYTES);
        assert_eq!(
            &patched[PATCH_BYTES.len()..],
            &original[PATCH_BYTES.len()..]
        );
    }
}
