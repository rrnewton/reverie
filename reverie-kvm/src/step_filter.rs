/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! Which user instruction a precise timer event's single steps may step (see
//! `preemption_timer.rs`), decoded with iced-x86.
//!
//! The trap flag (TF) that a step sets is in the guest's real RFLAGS, so an
//! instruction that saves RFLAGS where the guest can read them, or loads
//! them, sees or drops it: PUSHF pushes it, POPF and IRETQ replace it, a
//! syscall's R11 and an exception or interrupt frame save it. This filter
//! names those instructions, and the ones that leave user mode or may be
//! completed by KVM without an exit, so that the engine runs them unstepped
//! or repairs what they did.

use iced_x86::Decoder;
use iced_x86::DecoderOptions;
use iced_x86::Mnemonic;
use iced_x86::Register;

/// The longest x86-64 instruction.
pub(crate) const MAX_INSTRUCTION_BYTES: usize = 15;

/// The bytes of one instruction, in a buffer whose alignment keeps iced-x86
/// 1.21.0 from panicking: it panics on an instruction whose bytes end at a
/// multiple of 4 GiB of host memory
/// (<https://github.com/rrnewton/reverie/issues/858>), and the first
/// [`MAX_INSTRUCTION_BYTES`] bytes of a 16-byte aligned buffer never do.
#[repr(align(16))]
#[derive(Default)]
pub(crate) struct InstructionBytes(pub(crate) [u8; 16]);

/// How the engine runs the user instruction at RIP.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum StepClass {
    /// Step it.
    Step,
    /// Step it, then clear TF in the flags image it pushed at the new RSP.
    PushFlags,
    /// Step it after checking that the flags image it loads, `offset` bytes
    /// above RSP, does not set TF, then set stepping again: it replaced the
    /// trap flag with that image's.
    PopFlags { offset: u64 },
    /// Run it unstepped to the next instruction, `length` bytes on. KVM may
    /// complete it without an exit (RDTSC, RDTSCP, CPUID, VMCALL, VMMCALL),
    /// so a breakpoint there takes the guest back. Never stepped: an
    /// intercepted RDTSC or CPUID faults, and its frame would carry TF.
    RunOver { length: u64 },
    /// Run it unstepped: it leaves user mode with an exit of its own (the
    /// syscall's hypercall, or an exception stub's halt) before any further
    /// user instruction, or it holds off traps for the instruction after it
    /// (MOV SS), or it cannot be decoded.
    Unstepped,
}

/// Classifies the instruction whose bytes `code` holds, fetched at `rip`.
pub(crate) fn classify(code: &[u8], rip: u64) -> StepClass {
    let code = &code[..code.len().min(MAX_INSTRUCTION_BYTES)];
    let instruction = Decoder::with_ip(64, code, rip, DecoderOptions::NONE).decode();
    if instruction.is_invalid() {
        return StepClass::Unstepped;
    }
    match instruction.mnemonic() {
        Mnemonic::Pushf | Mnemonic::Pushfq => StepClass::PushFlags,
        Mnemonic::Popf | Mnemonic::Popfq => StepClass::PopFlags { offset: 0 },
        // The flags follow the return RIP and CS, each one operand wide.
        Mnemonic::Iret => StepClass::PopFlags { offset: 4 },
        Mnemonic::Iretd => StepClass::PopFlags { offset: 8 },
        Mnemonic::Iretq => StepClass::PopFlags { offset: 16 },
        Mnemonic::Rdtsc
        | Mnemonic::Rdtscp
        | Mnemonic::Cpuid
        | Mnemonic::Vmcall
        | Mnemonic::Vmmcall => StepClass::RunOver {
            length: instruction.len() as u64,
        },
        Mnemonic::Syscall
        | Mnemonic::Sysenter
        | Mnemonic::Sysexit
        | Mnemonic::Sysret
        | Mnemonic::Int
        | Mnemonic::Int1
        | Mnemonic::Int3
        | Mnemonic::Into
        | Mnemonic::Ud0
        | Mnemonic::Ud1
        | Mnemonic::Ud2
        | Mnemonic::Hlt
        | Mnemonic::In
        | Mnemonic::Out
        | Mnemonic::Insb
        | Mnemonic::Insw
        | Mnemonic::Insd
        | Mnemonic::Outsb
        | Mnemonic::Outsw
        | Mnemonic::Outsd => StepClass::Unstepped,
        Mnemonic::Mov | Mnemonic::Pop if instruction.op0_register() == Register::SS => {
            StepClass::Unstepped
        }
        _ => StepClass::Step,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn instructions_that_save_load_or_leave_with_rflags_are_not_stepped_plainly() {
        let rip = 0x40_1000;
        for (code, class) in [
            (&[0x90][..], StepClass::Step),                          // nop
            (&[0x75, 0xfe], StepClass::Step),                        // jnz .
            (&[0xff, 0xc9], StepClass::Step),                        // dec ecx
            (&[0x9c], StepClass::PushFlags),                         // pushfq
            (&[0x66, 0x9c], StepClass::PushFlags),                   // pushf
            (&[0x9d], StepClass::PopFlags { offset: 0 }),            // popfq
            (&[0x66, 0x9d], StepClass::PopFlags { offset: 0 }),      // popf
            (&[0x48, 0xcf], StepClass::PopFlags { offset: 16 }),     // iretq
            (&[0xcf], StepClass::PopFlags { offset: 8 }),            // iretd
            (&[0x0f, 0x31], StepClass::RunOver { length: 2 }),       // rdtsc
            (&[0x0f, 0x01, 0xf9], StepClass::RunOver { length: 3 }), // rdtscp
            (&[0x0f, 0xa2], StepClass::RunOver { length: 2 }),       // cpuid
            (&[0x0f, 0x01, 0xc1], StepClass::RunOver { length: 3 }), // vmcall
            (&[0x0f, 0x01, 0xd9], StepClass::RunOver { length: 3 }), // vmmcall
            (&[0x0f, 0x05], StepClass::Unstepped),                   // syscall
            (&[0xcd, 0x80], StepClass::Unstepped),                   // int 0x80
            (&[0xcc], StepClass::Unstepped),                         // int3
            (&[0xf1], StepClass::Unstepped),                         // int1
            (&[0x0f, 0x0b], StepClass::Unstepped),                   // ud2
            (&[0xf4], StepClass::Unstepped),                         // hlt
            (&[0xe4, 0x60], StepClass::Unstepped),                   // in al, 0x60
            (&[0xed], StepClass::Unstepped),                         // in eax, dx
            (&[0xee], StepClass::Unstepped),                         // out dx, al
            (&[0x6c], StepClass::Unstepped),                         // insb
            (&[0x8e, 0xd0], StepClass::Unstepped),                   // mov ss, ax
            (&[0x06], StepClass::Unstepped),                         // invalid in 64-bit mode
            (&[0x0f], StepClass::Unstepped),                         // truncated
        ] {
            assert_eq!(classify(code, rip), class, "{code:02x?}");
        }
    }
}
