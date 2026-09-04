//! Starting-state binding for an owned CPU step, without predicting ISA results.

use iced_x86::Decoder;
use iced_x86::DecoderOptions;
use iced_x86::FlowControl;
use iced_x86::InstructionInfoFactory;
use iced_x86::Mnemonic;
use iced_x86::OpAccess;
use liteinst2::trampoline::HookContext;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Refusal {
    pub stage: &'static str,
    pub operand: Option<(u64, usize, bool)>,
}

impl Refusal {
    pub(crate) fn new(stage: &'static str) -> Self {
        Self {
            stage,
            operand: None,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct CpuStepRequest {
    pub pc: u64,
    bytes: [u8; 15],
    length: usize,
    general: [u64; 16],
    flags: u64,
}

pub(crate) fn general(context: &HookContext) -> [u64; 16] {
    [
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
    ]
}

fn writes(access: OpAccess) -> bool {
    matches!(
        access,
        OpAccess::Write | OpAccess::CondWrite | OpAccess::ReadWrite | OpAccess::ReadCondWrite
    )
}

impl CpuStepRequest {
    pub(crate) fn retained_instruction(&self) -> (&[u8; 15], usize) {
        (&self.bytes, self.length)
    }

    pub(crate) fn input_registers(&self) -> &[u64; 16] {
        &self.general
    }

    pub(crate) fn input_flags(&self) -> u64 {
        self.flags
    }

    fn form(pc: u64, bytes: &[u8], refusal: &mut Refusal) -> Option<iced_x86::Instruction> {
        *refusal = Refusal::new("native/form");
        if pc == 0 || pc >= 1 << 47 {
            return None;
        }
        let instruction = Decoder::with_ip(64, bytes, pc, DecoderOptions::NONE).decode();
        if instruction.is_invalid()
            || instruction.is_privileged()
            || pc.checked_add(instruction.len() as u64)? >= 1 << 47
        {
            return None;
        }
        *refusal = Refusal::new("native/control-transition");
        let ordinary_flow = match instruction.flow_control() {
            FlowControl::Next
            | FlowControl::UnconditionalBranch
            | FlowControl::IndirectBranch
            | FlowControl::ConditionalBranch => true,
            FlowControl::Call => instruction.is_call_near(),
            FlowControl::IndirectCall => instruction.is_call_near_indirect(),
            FlowControl::Return => instruction.mnemonic() == Mnemonic::Ret,
            _ => false,
        };
        if !ordinary_flow || instruction.is_jmp_far() || instruction.is_jmp_far_indirect() {
            return None;
        }
        *refusal = Refusal::new("native/debug-control");
        if matches!(
            instruction.mnemonic(),
            Mnemonic::Pushf
                | Mnemonic::Pushfd
                | Mnemonic::Pushfq
                | Mnemonic::Popf
                | Mnemonic::Popfd
                | Mnemonic::Popfq
                | Mnemonic::Iret
                | Mnemonic::Iretd
                | Mnemonic::Iretq
                | Mnemonic::Lss
        ) {
            return None;
        }
        *refusal = Refusal::new("native/nondeterministic-or-system-state");
        if matches!(
            instruction.mnemonic(),
            Mnemonic::Cpuid
                | Mnemonic::Rdtsc
                | Mnemonic::Rdtscp
                | Mnemonic::Rdrand
                | Mnemonic::Rdseed
                | Mnemonic::Rdpmc
                | Mnemonic::Rdpid
                | Mnemonic::Rdpru
                | Mnemonic::Rdfsbase
                | Mnemonic::Rdgsbase
                | Mnemonic::Wrfsbase
                | Mnemonic::Wrgsbase
                | Mnemonic::Rdpkru
                | Mnemonic::Wrpkru
                | Mnemonic::Xgetbv
                | Mnemonic::Xsetbv
                | Mnemonic::Xsave
                | Mnemonic::Xsave64
                | Mnemonic::Xsavec
                | Mnemonic::Xsavec64
                | Mnemonic::Xsaveopt
                | Mnemonic::Xsaveopt64
                | Mnemonic::Xsaves
                | Mnemonic::Xsaves64
                | Mnemonic::Xrstor
                | Mnemonic::Xrstor64
                | Mnemonic::Xrstors
                | Mnemonic::Xrstors64
                | Mnemonic::Xbegin
                | Mnemonic::Xend
                | Mnemonic::Xabort
                | Mnemonic::Xtest
                | Mnemonic::Monitor
                | Mnemonic::Monitorx
                | Mnemonic::Mwait
                | Mnemonic::Mwaitx
                | Mnemonic::Umonitor
                | Mnemonic::Umwait
                | Mnemonic::Tpause
                | Mnemonic::Sgdt
                | Mnemonic::Sidt
                | Mnemonic::Sldt
                | Mnemonic::Smsw
                | Mnemonic::Str
                | Mnemonic::Lar
                | Mnemonic::Lsl
                | Mnemonic::Verr
                | Mnemonic::Verw
                | Mnemonic::Enqcmd
                | Mnemonic::Enqcmds
                | Mnemonic::Xstore
                | Mnemonic::Xstore_alt
                | Mnemonic::Rstorssp
                | Mnemonic::Saveprevssp
                | Mnemonic::Setssbsy
                | Mnemonic::Clrssbsy
                | Mnemonic::Incsspd
                | Mnemonic::Incsspq
                | Mnemonic::Wrssd
                | Mnemonic::Wrssq
                | Mnemonic::Wrussd
                | Mnemonic::Wrussq
        ) || instruction.has_xacquire_prefix()
            || instruction.has_xrelease_prefix()
        {
            return None;
        }
        *refusal = Refusal::new("native/segment-state");
        let mut factory = InstructionInfoFactory::new();
        let info = factory.info(&instruction);
        if info
            .used_registers()
            .iter()
            .any(|used| used.register().is_segment_register() && writes(used.access()))
        {
            return None;
        }
        Some(instruction)
    }

    pub(crate) fn eligible(pc: u64, bytes: &[u8]) -> bool {
        Self::form(pc, bytes, &mut Refusal::new("native/form")).is_some()
    }

    pub(crate) fn decode_recorded(
        pc: u64,
        bytes: &[u8],
        context: &HookContext,
        fault_resume: bool,
        refusal: &mut Refusal,
    ) -> Option<Self> {
        let instruction = Self::form(pc, bytes, refusal)?;
        *refusal = Refusal::new("native/flags");
        if context.rflags & 0x100 != 0 || (context.rflags & 0x10000 != 0 && !fault_resume) {
            return None;
        }
        *refusal = Refusal::new("native/start-state");
        if context.instruction_pointer != pc {
            return None;
        }
        let mut fetched = [0; 15];
        fetched[..instruction.len()].copy_from_slice(&bytes[..instruction.len()]);
        Some(Self {
            pc,
            bytes: fetched,
            length: instruction.len(),
            general: general(context),
            flags: context.rflags,
        })
    }

    pub(crate) fn starts_at(&self, context: &HookContext) -> bool {
        self.pc == context.instruction_pointer
            && self.flags == context.rflags
            && self.general == general(context)
    }

    pub(crate) fn trace_control(registers: &[libc::greg_t; 23]) -> bool {
        let pc = registers[libc::REG_RIP as usize] as u64;
        let flags = registers[libc::REG_EFL as usize] as u64;
        pc != 0
            && pc < 1 << 47
            && flags & (0x10000 | 0x100) == 0x100
            && registers[libc::REG_TRAPNO as usize] == 1
            && registers[libc::REG_ERR as usize] == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn context() -> HookContext {
        let mut context: HookContext = unsafe { core::mem::zeroed() };
        context.instruction_pointer = 0x10000;
        context.rflags = 0x202;
        context
    }

    #[test]
    fn ordinary_cpu_effects_are_not_predicted_or_dereferenced() {
        for bytes in [
            &[0xf3, 0x48, 0xab][..],
            &[0xf3, 0xa4],
            &[0xf2, 0xae],
            &[0x48, 0x8b, 0x07],
            &[0x48, 0x89, 0x07],
            &[0xff, 0xe0],
            &[0xc3],
            &[0xf0, 0x48, 0x01, 0x07],
            &[0xf3, 0x0f, 0x1e, 0xfa],
            &[0x48, 0x98],
            &[0x75, 0xfe],
            &[0x48, 0xd3, 0xe0],
            &[0x64, 0x48, 0x8b, 0x07],
            &[0x65, 0x48, 0x89, 0x07],
            &[0x64, 0xf3, 0xa4],
        ] {
            for count in [0, 1, 2, u64::MAX] {
                for direction in [0, 0x400] {
                    let mut context = context();
                    context.rcx = count;
                    context.rdi = u64::MAX;
                    context.rflags |= direction;
                    let request = CpuStepRequest::decode_recorded(
                        0x10000,
                        bytes,
                        &context,
                        false,
                        &mut Refusal::new("test"),
                    )
                    .unwrap();
                    assert!(request.starts_at(&context));
                    assert_eq!(&request.bytes[..request.length], bytes);
                }
            }
        }
    }

    #[test]
    fn request_binds_original_registers_flags_and_pc() {
        let original = context();
        let request = CpuStepRequest::decode_recorded(
            0x10000,
            &[0xf3, 0x48, 0xab],
            &original,
            false,
            &mut Refusal::new("test"),
        )
        .unwrap();
        let mut changed = original;
        changed.rcx = 1;
        assert!(!request.starts_at(&changed));
        changed = original;
        changed.instruction_pointer += 3;
        assert!(!request.starts_at(&changed));
        changed = original;
        changed.rflags ^= 0x400;
        assert!(!request.starts_at(&changed));
    }

    #[test]
    fn real_control_and_nondeterminism_hazards_remain_refused() {
        for bytes in [
            &[0x9c][..],
            &[0x9d],
            &[0x48, 0xcf],
            &[0x8e, 0xd0],
            &[0x0f, 0xb2, 0x07],
            &[0x0f, 0x05],
            &[0xcd, 0x80],
            &[0xcc],
            &[0x0f, 0xa2],
            &[0x0f, 0x31],
            &[0x0f, 0x33],
            &[0x0f, 0x01, 0xf9],
            &[0x48, 0x0f, 0xc7, 0xf0],
            &[0x48, 0x0f, 0xc7, 0xf8],
            &[0x8e, 0xe0],
            &[0x8e, 0xe8],
            &[0xf3, 0x48, 0x0f, 0xae, 0xd0],
            &[0xc7, 0xf8, 0, 0, 0, 0],
            &[0xf0, 0x90],
            &[0x48],
        ] {
            assert!(!CpuStepRequest::eligible(0x10000, bytes), "{bytes:x?}");
        }
    }

    #[test]
    fn near_calls_and_returns_are_not_system_flow_transitions() {
        for bytes in [
            &[0xe8, 0, 0, 0, 0][..],
            &[0xff, 0xd0],
            &[0xff, 0x17],
            &[0xc3],
            &[0xc2, 0x10, 0],
        ] {
            let context = context();
            let request = CpuStepRequest::decode_recorded(
                0x10000,
                bytes,
                &context,
                false,
                &mut Refusal::new("test"),
            )
            .unwrap();
            assert!(request.starts_at(&context));
            assert_eq!(&request.bytes[..request.length], bytes);
        }
        for bytes in [
            &[0x0f, 0x05][..],
            &[0x0f, 0x34],
            &[0x0f, 0x07],
            &[0x48, 0x0f, 0x07],
            &[0x0f, 0x35],
            &[0x48, 0x0f, 0x35],
            &[0x48, 0xcf],
            &[0xf3, 0x0f, 0x01, 0xec],
            &[0x0f, 0x01, 0xc1],
            &[0x0f, 0x01, 0xd9],
            &[0xff, 0x18],
            &[0xcb],
            &[0xca, 0x10, 0],
        ] {
            let instruction = Decoder::with_ip(64, bytes, 0x10000, DecoderOptions::NONE).decode();
            assert!(!instruction.is_invalid(), "{bytes:x?}");
            assert!(!CpuStepRequest::eligible(0x10000, bytes), "{bytes:x?}");
        }
    }

    #[test]
    fn owned_tf_and_fault_resume_are_not_guest_exemptions() {
        let mut context = context();
        context.rflags |= 0x10000;
        assert!(
            CpuStepRequest::decode_recorded(
                0x10000,
                &[0x90],
                &context,
                false,
                &mut Refusal::new("test")
            )
            .is_none()
        );
        assert!(
            CpuStepRequest::decode_recorded(
                0x10000,
                &[0x90],
                &context,
                true,
                &mut Refusal::new("test")
            )
            .is_some()
        );
        context.rflags |= 0x100;
        assert!(
            CpuStepRequest::decode_recorded(
                0x10000,
                &[0x90],
                &context,
                true,
                &mut Refusal::new("test")
            )
            .is_none()
        );
    }
}
