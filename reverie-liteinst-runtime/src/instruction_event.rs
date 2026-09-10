use liteinst2::trampoline::HookContext;
use reverie::Rdtsc;
use reverie::RdtscResult;
use reverie_preload::signal::native_frame::InstructionResult;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Kind {
    Cpuid,
    Rdtsc,
    Rdtscp,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct InstructionEvent {
    pub(crate) kind: Kind,
    pub(crate) fault_pc: u64,
    pub(crate) resume_pc: u64,
}

impl InstructionEvent {
    pub(crate) fn admit(
        fault_pc: u64,
        bytes: &[u8],
        mapping: &std::ops::Range<u64>,
    ) -> Option<Self> {
        let event = Self::decode(fault_pc, bytes)?;
        (fault_pc != 0
            && event.resume_pc < 1 << 47
            && mapping.contains(&fault_pc)
            && mapping.contains(&event.resume_pc))
        .then_some(event)
    }

    pub(crate) fn decode(fault_pc: u64, bytes: &[u8]) -> Option<Self> {
        let kind = match bytes {
            [0x0f, 0xa2, ..] => Kind::Cpuid,
            [0x0f, 0x31, ..] => Kind::Rdtsc,
            [0x0f, 0x01, 0xf9, ..] => Kind::Rdtscp,
            _ => return None,
        };
        Some(Self {
            kind,
            fault_pc,
            resume_pc: fault_pc.checked_add(kind.bytes().len() as u64)?,
        })
    }
}

pub(crate) fn apply_result(context: &mut HookContext, result: InstructionResult) {
    match result {
        InstructionResult::Cpuid(result) => {
            context.rax = u64::from(result.eax);
            context.rbx = u64::from(result.ebx);
            context.rcx = u64::from(result.ecx);
            context.rdx = u64::from(result.edx);
        }
        InstructionResult::Rdtsc { request, result } => apply_rdtsc(context, request, result),
    }
}

impl Kind {
    pub(crate) const fn bytes(self) -> &'static [u8] {
        match self {
            Self::Cpuid => &[0x0f, 0xa2],
            Self::Rdtsc => &[0x0f, 0x31],
            Self::Rdtscp => &[0x0f, 0x01, 0xf9],
        }
    }
}

pub(crate) fn apply_rdtsc(context: &mut HookContext, request: Rdtsc, result: RdtscResult) {
    context.rax = u64::from(result.tsc as u32);
    context.rdx = result.tsc >> 32;
    if request == Rdtsc::Tscp {
        context.rcx = u64::from(result.aux.unwrap_or(0));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn instruction_length_is_independent_of_register_outputs() {
        for kind in [Kind::Cpuid, Kind::Rdtsc, Kind::Rdtscp] {
            let event = InstructionEvent::decode(0x1fff, kind.bytes()).unwrap();
            assert_eq!(event.kind, kind);
            assert_eq!(event.fault_pc, 0x1fff);
            assert_eq!(event.resume_pc, 0x1fff + kind.bytes().len() as u64);
            for end in 0..kind.bytes().len() {
                assert!(InstructionEvent::decode(0x1fff, &kind.bytes()[..end]).is_none());
            }
            assert!(InstructionEvent::decode(u64::MAX - 1, kind.bytes()).is_none());
        }
        for bytes in [&[0x90, 0x90][..], &[0x66, 0x0f, 0xa2], &[0x0f, 0x01, 0xf8]] {
            assert!(InstructionEvent::decode(0x1000, bytes).is_none());
        }
    }

    #[test]
    fn admission_checks_whole_instruction_and_successor_before_dispatch() {
        for kind in [Kind::Cpuid, Kind::Rdtsc, Kind::Rdtscp] {
            let length = kind.bytes().len() as u64;
            let event =
                InstructionEvent::admit(0x4000, kind.bytes(), &(0x4000..0x4000 + length + 1))
                    .unwrap();
            assert_eq!(event.resume_pc, 0x4000 + length);
            assert!(
                InstructionEvent::admit(0x4000, kind.bytes(), &(0x4000..0x4000 + length)).is_none()
            );
            assert!(InstructionEvent::admit(0x4000, kind.bytes(), &(0x4001..0x5000)).is_none());
            assert!(InstructionEvent::admit(0, kind.bytes(), &(0..0x5000)).is_none());
            let pc = (1 << 47) - length;
            assert!(InstructionEvent::admit(pc, kind.bytes(), &(pc..pc + length + 1)).is_none());
        }
    }

    #[test]
    fn actual_result_application_preserves_rdtsc_rcx_and_zeroes_missing_rdtscp_aux() {
        for request in [Rdtsc::Tsc, Rdtsc::Tscp] {
            for aux in [None, Some(0xdead_beef)] {
                let mut context: HookContext = unsafe { core::mem::zeroed() };
                context.rcx = 0xfedc_ba98_7654_3210;
                context.rbx = 0x1_2345;
                context.rflags = 0x246;
                context.instruction_pointer = 0x4000;
                context.stack_pointer = 0x8000;
                apply_result(
                    &mut context,
                    InstructionResult::Rdtsc {
                        request,
                        result: RdtscResult {
                            tsc: 0x1234_5678_9abc_def0,
                            aux,
                        },
                    },
                );
                assert_eq!(context.rax, 0x9abc_def0);
                assert_eq!(context.rdx, 0x1234_5678);
                assert_eq!(
                    context.rcx,
                    if request == Rdtsc::Tsc {
                        0xfedc_ba98_7654_3210
                    } else {
                        u64::from(aux.unwrap_or(0))
                    }
                );
                assert_eq!(context.rbx, 0x1_2345);
                assert_eq!(context.rflags, 0x246);
                assert_eq!(context.instruction_pointer, 0x4000);
                assert_eq!(context.stack_pointer, 0x8000);
            }
        }
    }
}
