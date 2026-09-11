//! Finite native stepping vocabulary: predictions, never instruction emulation.
use liteinst2::trampoline::HookContext;

use crate::instruction_event::InstructionEvent;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum Instruction {
    Native(Box<super::native::CpuStepRequest>),
    Nop,
    MovEcx(u32),
    DecEcx,
    Jump(u64),
    Zero {
        target: u64,
        fallthrough: u64,
        zero: bool,
    },
    Fault(InstructionEvent),
    Syscall,
}

pub(crate) struct Plan {
    pub(crate) pc: u64,
    pub(crate) flags: u64,
    pub(crate) general: [u64; 16],
    pub(crate) rcbs: u64,
    pub(crate) fault: Option<InstructionEvent>,
}

impl Instruction {
    pub(crate) fn decode(pc: u64, bytes: &[u8]) -> Option<Self> {
        match bytes {
            [0x90, ..] => Some(Self::Nop),
            [0xb9, low, next, high, top, ..] => {
                Some(Self::MovEcx(u32::from_le_bytes([*low, *next, *high, *top])))
            }
            [0xff, 0xc9, ..] => Some(Self::DecEcx),
            [opcode @ (0x74 | 0x75 | 0xeb), displacement, ..] => {
                let fallthrough = pc.checked_add(2)?;
                let target = fallthrough.checked_add_signed(i64::from(*displacement as i8))?;
                if target == 0 || target >= 1 << 47 {
                    return None;
                }
                Some(if *opcode == 0xeb {
                    Self::Jump(target)
                } else {
                    Self::Zero {
                        target,
                        fallthrough,
                        zero: *opcode == 0x74,
                    }
                })
            }
            [0x0f, opcode @ (0x84 | 0x85), low, next, high, top, ..] => {
                let fallthrough = pc.checked_add(6)?;
                let displacement = i64::from(i32::from_le_bytes([*low, *next, *high, *top]));
                let target = fallthrough.checked_add_signed(displacement)?;
                (target != 0 && target < 1 << 47).then_some(Self::Zero {
                    target,
                    fallthrough,
                    zero: *opcode == 0x84,
                })
            }
            [0xe9, low, next, high, top, ..] => {
                let target =
                    pc.checked_add(5)?
                        .checked_add_signed(i64::from(i32::from_le_bytes([
                            *low, *next, *high, *top,
                        ])))?;
                (target != 0 && target < 1 << 47).then_some(Self::Jump(target))
            }
            _ => InstructionEvent::decode(pc, bytes).map(Self::Fault),
        }
    }

    pub(crate) fn plan(&self, pc: u64, context: &HookContext) -> Option<Plan> {
        let mut result = Plan {
            pc,
            flags: (context.rflags | 0x100) & !0x10000,
            general: [
                context.r8,
                context.r9,
                context.r10,
                context.r11,
                context.r12,
                context.r13,
                context.r14,
                context.r15,
                context.rdi,
                context.rsi,
                context.rbp,
                context.rbx,
                context.rdx,
                context.rax,
                context.rcx,
                context.stack_pointer,
            ],
            rcbs: 0,
            fault: None,
        };
        match *self {
            Self::Nop => result.pc = pc.checked_add(1)?,
            Self::MovEcx(value) => {
                result.pc = pc.checked_add(5)?;
                result.general[14] = u64::from(value);
            }
            Self::DecEcx => {
                result.pc = pc.checked_add(2)?;
                let before = context.rcx as u32;
                let after = before.wrapping_sub(1);
                result.general[14] = u64::from(after);
                result.flags &= !0x8d4;
                result.flags |= (u64::from((after as u8).count_ones().is_multiple_of(2)) * 4)
                    | (u64::from(before & 15 == 0) * 16)
                    | (u64::from(after == 0) * 64)
                    | (u64::from(after >> 31 != 0) * 128)
                    | (u64::from(before == 0x8000_0000) * 2048);
            }
            Self::Jump(target) => result.pc = target,
            Self::Zero {
                target,
                fallthrough,
                zero,
            } => {
                result.pc = if (context.rflags & 64 != 0) == zero {
                    target
                } else {
                    fallthrough
                };
                result.rcbs = 1;
            }
            Self::Fault(event) => {
                result.fault = Some(event);
                result.flags |= 0x10000;
            }
            Self::Syscall | Self::Native(_) => return None,
        }
        (result.pc != 0 && result.pc < 1 << 47).then_some(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn finite_decode_refuses_syscalls_flags_and_repeated_instructions() {
        for bytes in [
            &[0x0f, 0x05][..],
            &[0x9d],
            &[0xf3, 0xa4],
            &[0xff, 0xe0],
            &[0xcc],
            &[0x0f, 0x0b],
        ] {
            assert!(Instruction::decode(0x4000, bytes).is_none());
        }
        assert!(Instruction::decode(0x4000, &[0xb9, 1, 2]).is_none());
    }
    #[test]
    fn decrement_predicts_all_changed_flags_and_zero_extended_ecx() {
        for (before, after, flags) in [
            (0u32, u32::MAX, 0x397),
            (1, 0, 0x347),
            (0x8000_0000, 0x7fff_ffff, 0xb17),
        ] {
            let mut context: HookContext = unsafe { core::mem::zeroed() };
            context.rcx = 0xfeed_0000_0000_0000 | u64::from(before);
            context.rflags = 0x10203;
            let plan = Instruction::DecEcx.plan(0x4000, &context).unwrap();
            assert_eq!(
                (plan.pc, plan.general[14], plan.flags, plan.rcbs),
                (0x4002, u64::from(after), flags, 0)
            );
        }
    }
    #[test]
    fn conditional_counts_taken_or_not_and_jump_can_repeat_pc() {
        let mut context: HookContext = unsafe { core::mem::zeroed() };
        for flags in [0x202, 0x242] {
            context.rflags = flags;
            let plan = Instruction::decode(0x4000, &[0x75, 0xfe])
                .unwrap()
                .plan(0x4000, &context)
                .unwrap();
            assert_eq!(plan.rcbs, 1);
            assert_eq!(plan.pc, if flags == 0x202 { 0x4000 } else { 0x4002 });
        }
        let plan = Instruction::decode(0x4000, &[0xeb, 0xfe])
            .unwrap()
            .plan(0x4000, &context)
            .unwrap();
        assert_eq!((plan.pc, plan.rcbs), (0x4000, 0));
    }

    #[test]
    fn near_conditional_encoding_matches_linked_guest_and_checks_length() {
        let mut context: HookContext = unsafe { core::mem::zeroed() };
        for opcode in [0x84, 0x85] {
            let bytes = [0x0f, opcode, 0xf8, 0xff, 0xff, 0xff];
            for length in 0..bytes.len() {
                assert!(Instruction::decode(0x4002, &bytes[..length]).is_none());
            }
            for flags in [0x202, 0x242] {
                context.rflags = flags;
                let plan = Instruction::decode(0x4002, &bytes)
                    .unwrap()
                    .plan(0x4002, &context)
                    .unwrap();
                let taken = (opcode == 0x84) == (flags & 64 != 0);
                assert_eq!(
                    (plan.pc, plan.rcbs),
                    (if taken { 0x4000 } else { 0x4008 }, 1)
                );
            }
        }
    }
}
