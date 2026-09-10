use std::ops::Range;

use iced_x86::Code;
use iced_x86::Decoder;
use iced_x86::DecoderOptions;
use iced_x86::InstructionInfoFactory;
use iced_x86::OpAccess;
use iced_x86::OpKind;
use iced_x86::Register;
use liteinst2::trampoline::HookContext;

use crate::owned_step::native::CpuStepRequest;

mod digest;

const IMAGE_SHA256: [u8; 32] = [
    0xa8, 0xcc, 0xf4, 0xc1, 0xa8, 0x7d, 0xd3, 0x87, 0x40, 0xac, 0x49, 0xcb, 0xd3, 0x55, 0xd9, 0xe4,
    0x62, 0xb4, 0x36, 0x99, 0xbe, 0xfa, 0xab, 0x26, 0x28, 0x1d, 0xfe, 0x0b, 0x71, 0x06, 0x55, 0x67,
];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Refusal {
    Image,
    Layout,
    Export,
    Owner,
    State,
    Instruction,
    Operand,
    Field,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RngField {
    Ready,
    Generation,
}

impl RngField {
    pub(crate) fn width(self) -> usize {
        match self {
            Self::Ready => 1,
            Self::Generation => 8,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ReadIdentity {
    pub owner: i64,
    pub frame_generation: u64,
    pub cpu_sequence: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct BindingIdentity {
    owner: i64,
    image: [u8; 32],
    text: Range<u64>,
    rng_address: u64,
}

#[derive(Debug)]
pub(crate) struct RngBinding<'image> {
    identity: BindingIdentity,
    bytes: &'image [u8],
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct RngReadRequest {
    binding: BindingIdentity,
    identity: ReadIdentity,
    cpu: CpuStepRequest,
    field: RngField,
    address: u64,
    code: Code,
    register: Option<Register>,
    immediate: Option<u8>,
    next_pc: u64,
}

impl<'image> RngBinding<'image> {
    pub(super) fn bind(active: &'image super::Active) -> Result<Self, Refusal> {
        if active.owner <= 0 {
            return Err(Refusal::Owner);
        }
        if active._image.len() != 8192 || digest::sha256(&active._image) != IMAGE_SHA256 {
            return Err(Refusal::Image);
        }
        let maps = super::selected_maps(&active.kernel_mappings, active.range.start)
            .map_err(|_| Refusal::Layout)?;
        if maps != active.kernel_mappings || maps.len() != 3 {
            return Err(Refusal::Layout);
        }
        let base = active.range.start;
        let vvar_start = base.checked_sub(0x6000).ok_or(Refusal::Layout)?;
        let vclock_start = base.checked_sub(0x2000).ok_or(Refusal::Layout)?;
        let end = base.checked_add(8192).ok_or(Refusal::Layout)?;
        if active.range != (base..end)
            || maps[0].name != "[vvar]"
            || maps[0].range != (vvar_start..vclock_start)
            || maps[1].name != "[vvar_vclock]"
            || maps[1].range != (vclock_start..base)
            || maps[2].name != "[vdso]"
            || maps[2].range != active.range
            || active.vvar != [vvar_start..vclock_start, vclock_start..base]
        {
            return Err(Refusal::Layout);
        }
        let exports = super::elf::exports(&active._image, active.range.clone())
            .map_err(|_| Refusal::Image)?;
        if exports != active.exports
            || ["__vdso_getrandom", "getrandom"].iter().any(|name| {
                !exports.iter().any(|export| {
                    export.name == *name
                        && export.version == "LINUX_2.6"
                        && export.address == base + 0x1050
                })
            })
        {
            return Err(Refusal::Export);
        }
        Ok(Self {
            identity: BindingIdentity {
                owner: active.owner,
                image: IMAGE_SHA256,
                text: active.range.clone(),
                rng_address: vvar_start + 0x2000,
            },
            bytes: &active._image,
        })
    }

    pub(crate) fn decode(
        &self,
        identity: ReadIdentity,
        cpu: CpuStepRequest,
        context: &HookContext,
    ) -> Result<RngReadRequest, Refusal> {
        if identity.owner != self.identity.owner
            || identity.frame_generation == 0
            || identity.cpu_sequence == 0
        {
            return Err(Refusal::Owner);
        }
        if !cpu.starts_at(context) {
            return Err(Refusal::State);
        }
        let offset = cpu
            .pc
            .checked_sub(self.identity.text.start)
            .ok_or(Refusal::Instruction)?;
        let (expected_code, field, register, immediate) = match offset {
            0x10be => (Code::Cmp_rm8_imm8, RngField::Ready, None, Some(0)),
            0x110b => (
                Code::Mov_r64_rm64,
                RngField::Generation,
                Some(Register::RCX),
                None,
            ),
            0x13b3 => (
                Code::Cmp_r64_rm64,
                RngField::Generation,
                Some(Register::RAX),
                None,
            ),
            _ => return Err(Refusal::Field),
        };
        let offset = usize::try_from(offset).map_err(|_| Refusal::Instruction)?;
        let retained = self.bytes.get(offset..).ok_or(Refusal::Instruction)?;
        let (recorded, length) = cpu.retained_instruction();
        if retained.get(..length) != recorded.get(..length) {
            return Err(Refusal::Instruction);
        }
        let instruction = Decoder::with_ip(64, retained, cpu.pc, DecoderOptions::NONE).decode();
        if instruction.code() != expected_code
            || instruction.len() != length
            || instruction.is_invalid()
        {
            return Err(Refusal::Instruction);
        }
        let address = self.identity.rng_address + if field == RngField::Ready { 8 } else { 0 };
        validate_operand(&instruction, field, address, register, immediate)?;
        Ok(RngReadRequest {
            binding: self.identity.clone(),
            identity,
            cpu,
            field,
            address,
            code: expected_code,
            register,
            immediate,
            next_pc: instruction.next_ip(),
        })
    }
}

fn validate_operand(
    instruction: &iced_x86::Instruction,
    field: RngField,
    address: u64,
    register: Option<Register>,
    immediate: Option<u8>,
) -> Result<(), Refusal> {
    let mut factory = InstructionInfoFactory::new();
    let info = factory.info(instruction);
    if !instruction.is_ip_rel_memory_operand()
        || instruction.memory_base() != Register::RIP
        || instruction.memory_index() != Register::None
        || instruction.segment_prefix() != Register::None
        || instruction.has_lock_prefix()
        || instruction.has_rep_prefix()
        || instruction.has_repne_prefix()
        || instruction.memory_size().size() != field.width()
        || instruction.ip_rel_memory_address() != address
        || info.used_memory().len() != 1
        || info.used_memory()[0].access() != OpAccess::Read
        || instruction.op_count() != 2
    {
        return Err(Refusal::Operand);
    }
    let valid = match (instruction.code(), register, immediate) {
        (Code::Cmp_rm8_imm8, None, Some(value)) => {
            field == RngField::Ready
                && instruction.op0_kind() == OpKind::Memory
                && instruction.op1_kind() == OpKind::Immediate8
                && instruction.immediate8() == value
        }
        (Code::Mov_r64_rm64 | Code::Cmp_r64_rm64, Some(expected), None) => {
            field == RngField::Generation
                && instruction.op0_kind() == OpKind::Register
                && instruction.op0_register() == expected
                && instruction.op1_kind() == OpKind::Memory
        }
        _ => false,
    };
    if valid { Ok(()) } else { Err(Refusal::Operand) }
}

impl RngReadRequest {
    #[cfg(test)]
    pub(crate) fn validate(
        &self,
        binding: &RngBinding<'_>,
        identity: ReadIdentity,
        context: &HookContext,
    ) -> Result<(), Refusal> {
        if self.binding != binding.identity || self.identity != identity {
            return Err(Refusal::Owner);
        }
        if !self.cpu.starts_at(context) {
            return Err(Refusal::State);
        }
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn identity(&self) -> ReadIdentity {
        self.identity
    }
    #[cfg(test)]
    pub(crate) fn image_sha256(&self) -> [u8; 32] {
        self.binding.image
    }
    pub(crate) fn field(&self) -> RngField {
        self.field
    }
    pub(crate) fn address(&self) -> u64 {
        self.address
    }
    pub(crate) fn code(&self) -> Code {
        self.code
    }
    pub(crate) fn register(&self) -> Option<Register> {
        self.register
    }
    pub(crate) fn immediate(&self) -> Option<u8> {
        self.immediate
    }
    pub(crate) fn next_pc(&self) -> u64 {
        self.next_pc
    }
    pub(crate) fn cpu_request(&self) -> &CpuStepRequest {
        &self.cpu
    }
}

#[cfg(test)]
pub(crate) mod tests;
