//! One-use owned CPU-step capture plus retained finite fixture/fault transitions.

use reverie_preload::precise_timer::Controller;
use reverie_preload::precise_timer::Decision;
use reverie_preload::precise_timer::Generation;
use reverie_preload::precise_timer::Observation;
mod instruction;
pub(crate) mod native;
pub(crate) use instruction::Instruction;
use liteinst2::trampoline::HookContext;

use crate::instruction_event::InstructionEvent;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Completion {
    generation: Option<Generation>,
    sequence: u64,
    cpu_sequence: Option<u64>,
    frame_generation: u64,
    owner: i64,
    pub(crate) pc: u64,
    pub(crate) clock: u64,
    pub(crate) flags: u64,
    general: Option<[u64; 16]>,
}

impl Completion {
    pub(crate) fn is_cpu(self) -> bool {
        self.cpu_sequence.is_some()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ReadFaultCapture {
    staged: Completion,
    request: native::CpuStepRequest,
    address: u64,
    flags: u64,
    clock: u64,
}

impl ReadFaultCapture {
    pub(crate) fn identity(&self) -> crate::vdso::rng::ReadIdentity {
        crate::vdso::rng::ReadIdentity {
            owner: self.staged.owner,
            frame_generation: self.staged.frame_generation,
            cpu_sequence: self.staged.cpu_sequence.unwrap(),
        }
    }

    pub(crate) fn request(&self) -> native::CpuStepRequest {
        self.request
    }

    pub(crate) fn address(&self) -> u64 {
        self.address
    }

    pub(crate) fn flags(&self) -> u64 {
        self.flags
    }

    pub(crate) fn clock(&self) -> u64 {
        self.clock
    }

    pub(crate) fn ticket(&self) -> Option<crate::timer::Ticket> {
        self.staged
            .generation
            .map(|generation| crate::timer::Ticket {
                generation,
                sequence: self.staged.sequence,
            })
    }

    pub(crate) fn matches_context(&self, context: &HookContext) -> bool {
        context.instruction_pointer == self.request.pc
            && context.rflags == self.flags
            && native::general(context) == *self.request.input_registers()
    }
}

#[derive(Debug, Default)]
pub(crate) struct Stepper {
    cpu_sequence: u64,
    controller: Controller,
    expected: Option<Completion>,
    native: Option<(Completion, native::CpuStepRequest)>,
    captured: Option<Completion>,
    modeled: Option<ReadFaultCapture>,
    fault: Option<(InstructionEvent, Completion)>,
    syscall: Option<(i64, Completion)>,
    call: Option<Completion>,
    pub(crate) completed: u64,
    pub(crate) modeled_completed: u64,
    pub(crate) last: Option<Completion>,
}

impl Stepper {
    pub(crate) fn capture_read_fault(
        &mut self,
        owner: i64,
        frame: u64,
        fault: crate::vdso::Fault<'_>,
        clock: u64,
    ) -> Option<ReadFaultCapture> {
        let (staged, request) = self.native?;
        let registers = fault.registers;
        let flags = registers[libc::REG_EFL as usize] as u64;
        let original_flags = request.input_flags();
        if self.modeled.is_some()
            || self.captured.is_some()
            || owner != staged.owner
            || frame != staged.frame_generation
            || frame.checked_add(1).is_none()
            || staged.cpu_sequence != Some(self.cpu_sequence)
            || fault.signal != libc::SIGSEGV
            || fault.code != 2
            || registers[libc::REG_TRAPNO as usize] != 14
            || registers[libc::REG_ERR as usize] & !1 != 4
            || registers[libc::REG_RIP as usize] as u64 != request.pc
            || flags & 0x100 == 0
            || (flags ^ original_flags) & !0x10100 != 0
            || (original_flags & 0x10000 != 0 && flags & 0x10000 == 0)
            || core::array::from_fn::<_, 16, _>(|index| registers[index] as u64)
                != *request.input_registers()
            || clock < staged.clock
        {
            return None;
        }
        let captured = ReadFaultCapture {
            staged,
            request,
            address: fault.address,
            flags,
            clock,
        };
        self.native = None;
        self.modeled = Some(captured);
        Some(captured)
    }

    pub(crate) fn validate_modeled(
        &self,
        captured: ReadFaultCapture,
        owner: i64,
        frame: u64,
    ) -> Option<()> {
        (self.modeled == Some(captured)
            && captured.identity().owner == owner
            && captured.staged.frame_generation.checked_add(1) == Some(frame)
            && captured.staged.cpu_sequence == Some(self.cpu_sequence))
        .then_some(())
    }

    pub(crate) fn complete_modeled(
        &mut self,
        captured: ReadFaultCapture,
        owner: i64,
        frame: u64,
        successor: u64,
    ) -> Option<Option<Observation>> {
        self.validate_modeled(captured, owner, frame)?;
        let (_, length) = captured.request.retained_instruction();
        if captured.request.pc.checked_add(length as u64) != Some(successor) {
            return None;
        }
        let completed = self.modeled_completed.checked_add(1)?;
        self.modeled = None;
        self.modeled_completed = completed;
        Some(captured.staged.generation.map(|generation| Observation {
            generation,
            sequence: captured.staged.sequence,
            clock: captured.clock,
            rip: successor,
        }))
    }

    pub(crate) fn stage_cpu(
        &mut self,
        frame: (i64, u64),
        ticket: Option<crate::timer::Ticket>,
        request: native::CpuStepRequest,
        context: &HookContext,
        clock: u64,
    ) -> Option<()> {
        if self.pending() || frame.0 <= 0 || frame.1 == 0 || !request.starts_at(context) {
            return None;
        }
        let sequence = self.cpu_sequence.checked_add(1)?;
        self.native = Some((
            Completion {
                generation: ticket.map(|ticket| ticket.generation),
                sequence: ticket.map_or(0, |ticket| ticket.sequence),
                cpu_sequence: Some(sequence),
                frame_generation: frame.1,
                owner: frame.0,
                pc: request.pc,
                clock,
                flags: context.rflags,
                general: Some(native::general(context)),
            },
            request,
        ));
        self.cpu_sequence = sequence;
        Some(())
    }

    pub(crate) fn validate_cpu_fault(
        &self,
        owner: i64,
        frame: u64,
        registers: &[libc::greg_t; 23],
        clock: u64,
    ) -> Option<()> {
        let (expected, request) = self.native.as_ref()?;
        (owner == expected.owner
            && frame == expected.frame_generation
            && expected.cpu_sequence == Some(self.cpu_sequence)
            && registers[libc::REG_RIP as usize] as u64 == request.pc
            && registers[libc::REG_EFL as usize] as u64 & 0x100 != 0
            && clock >= expected.clock)
            .then_some(())
    }

    pub(crate) fn complete_cpu(
        &mut self,
        completion: Completion,
        clock: u64,
    ) -> Option<Option<Observation>> {
        if completion.cpu_sequence != Some(self.cpu_sequence)
            || self.captured != Some(completion)
            || clock != completion.clock
        {
            return None;
        }
        let completed = self.completed.checked_add(1)?;
        self.captured = None;
        self.completed = completed;
        self.last = Some(completion);
        Some(completion.generation.map(|generation| Observation {
            generation,
            sequence: completion.sequence,
            clock,
            rip: completion.pc,
        }))
    }

    pub(crate) fn pending(&self) -> bool {
        self.expected.is_some()
            || self.native.is_some()
            || self.captured.is_some()
            || self.modeled.is_some()
            || self.fault.is_some()
            || self.syscall.is_some()
            || self.call.is_some()
    }

    pub(crate) fn stage_nop(
        &mut self,
        owner: i64,
        frame_generation: u64,
        pc: u64,
        flags: u64,
        clock: u64,
    ) -> Option<()> {
        if self.pending()
            || owner <= 0
            || frame_generation == 0
            || flags & 0x100 != 0
            || pc == 0
            || pc.checked_add(1)? >= 1 << 47
        {
            return None;
        }
        let (generation, decision) = self.controller.replace(clock, 0, 1).ok()?;
        if decision != Decision::Step {
            return None;
        }
        self.expected = Some(Completion {
            generation: Some(generation),
            sequence: 1,
            cpu_sequence: None,
            frame_generation,
            owner,
            pc: pc + 1,
            clock,
            flags: (flags | 0x100) & !0x10000,
            general: None,
        });
        Some(())
    }

    /// NOP preserves arithmetic flags; architectural RF clears after execution.
    /// Compare the full resulting flags, not a masked completion observation.
    pub(crate) fn validate(
        &self,
        owner: i64,
        frame_generation: u64,
        registers: &[libc::greg_t; 23],
    ) -> Option<Completion> {
        let expected = self.expected?;
        (owner == expected.owner
            && frame_generation == expected.frame_generation
            && registers[libc::REG_RIP as usize] as u64 == expected.pc
            && registers[libc::REG_EFL as usize] as u64 == expected.flags
            && registers[libc::REG_TRAPNO as usize] == 1
            && registers[libc::REG_ERR as usize] == 0
            && general_matches(expected, registers))
        .then_some(expected)
    }

    pub(crate) fn capture(
        &mut self,
        owner: i64,
        frame_generation: u64,
        registers: &[libc::greg_t; 23],
        clock: u64,
    ) -> Option<Completion> {
        if self.captured.is_some() {
            return None;
        }
        let Some((staged, _request)) = self.native else {
            return self.validate(owner, frame_generation, registers);
        };
        if owner != staged.owner
            || frame_generation != staged.frame_generation
            || clock < staged.clock
            || !native::CpuStepRequest::trace_control(registers)
        {
            return None;
        }
        let completion = Completion {
            pc: registers[libc::REG_RIP as usize] as u64,
            clock,
            flags: registers[libc::REG_EFL as usize] as u64,
            general: Some(core::array::from_fn(|field| registers[field] as u64)),
            ..staged
        };
        self.native = None;
        self.captured = Some(completion);
        Some(completion)
    }

    pub(crate) fn complete(&mut self, completion: Completion, clock: u64) -> Option<()> {
        if self.expected != Some(completion) || clock != completion.clock {
            return None;
        }
        let completed = self.completed.checked_add(1)?;
        let decision = self
            .controller
            .observe(Observation {
                generation: completion.generation?,
                sequence: 1,
                clock,
                rip: completion.pc,
            })
            .ok()?;
        if decision != Decision::Deliver {
            return None;
        }
        self.expected = None;
        self.completed = completed;
        self.last = Some(completion);
        Some(())
    }

    /// Invalidate an interrupted step without counting it as an instruction.
    pub(crate) fn cancel(&mut self) {
        self.native = None;
        self.captured = None;
        self.modeled = None;
        self.fault = None;
        self.syscall = None;
        self.call = None;
        if let Some(expected) = self.expected.take() {
            if let Some(generation) = expected.generation {
                let _ = self.controller.cancel(generation);
            }
        }
    }

    pub(crate) fn stage_precise(
        &mut self,
        frame: (i64, u64),
        ticket: crate::timer::Ticket,
        instruction: Instruction,
        context: &HookContext,
        clock: u64,
    ) -> Option<()> {
        if self.pending() || frame.0 <= 0 || frame.1 == 0 || context.rflags & 0x100 != 0 {
            return None;
        }
        if let Instruction::Native(request) = instruction {
            return self.stage_cpu(frame, Some(ticket), *request, context, clock);
        }
        if instruction == Instruction::Syscall {
            if !crate::syscall_event::observable(context.rax as i64) {
                return None;
            }
            let pc = context.instruction_pointer.checked_add(2)?;
            if pc >= 1 << 47 {
                return None;
            }
            let mut plan = Instruction::Nop.plan(context.instruction_pointer, context)?;
            plan.general[3] = context.rflags | 0x100;
            plan.general[14] = pc;
            self.syscall = Some((
                context.rax as i64,
                Completion {
                    generation: Some(ticket.generation),
                    sequence: ticket.sequence,
                    cpu_sequence: None,
                    frame_generation: frame.1,
                    owner: frame.0,
                    pc,
                    clock,
                    flags: plan.flags,
                    general: Some(plan.general),
                },
            ));
            return Some(());
        }
        let plan = instruction.plan(context.instruction_pointer, context)?;
        let completion = Completion {
            generation: Some(ticket.generation),
            sequence: ticket.sequence,
            cpu_sequence: None,
            frame_generation: frame.1,
            owner: frame.0,
            pc: plan.pc,
            flags: plan.flags,
            clock: clock.checked_add(plan.rcbs)?,
            general: Some(plan.general),
        };
        if let Some(event) = plan.fault {
            self.fault = Some((event, completion));
        } else {
            self.expected = Some(completion);
        }
        Some(())
    }

    pub(crate) fn stage_call(
        &mut self,
        frame: (i64, u64),
        ticket: crate::timer::Ticket,
        context: &HookContext,
        clock: u64,
    ) -> Option<()> {
        if self.pending()
            || frame.0 <= 0
            || frame.1 == 0
            || context.rflags & 0x100 != 0
            || context.instruction_pointer == 0
            || context.instruction_pointer >= 1 << 47
        {
            return None;
        }
        let plan = Instruction::Nop.plan(context.instruction_pointer, context)?;
        self.call = Some(Completion {
            generation: Some(ticket.generation),
            sequence: ticket.sequence,
            cpu_sequence: None,
            frame_generation: frame.1,
            owner: frame.0,
            pc: context.instruction_pointer,
            clock,
            flags: plan.flags | 0x10000,
            general: Some(plan.general),
        });
        Some(())
    }

    pub(crate) fn validate_call(
        &self,
        owner: i64,
        frame: u64,
        registers: &[libc::greg_t; 23],
    ) -> Option<()> {
        let expected = self.call?;
        (owner == expected.owner
            && frame == expected.frame_generation
            && registers[libc::REG_RIP as usize] as u64 == expected.pc
            && registers[libc::REG_EFL as usize] as u64 == expected.flags
            && registers[libc::REG_TRAPNO as usize] == 14
            && registers[libc::REG_ERR as usize] == 0x15
            && general_matches(expected, registers))
        .then_some(())
    }

    pub(crate) fn cancel_call(&mut self, clock: u64) -> Option<()> {
        if self.call?.clock != clock {
            return None;
        }
        self.call = None;
        Some(())
    }

    pub(crate) fn validate_fault(
        &self,
        owner: i64,
        frame_generation: u64,
        registers: &[libc::greg_t; 23],
    ) -> Option<InstructionEvent> {
        let (event, expected) = self.fault?;
        (owner == expected.owner
            && frame_generation == expected.frame_generation
            && registers[libc::REG_RIP as usize] as u64 == expected.pc
            && registers[libc::REG_EFL as usize] as u64 == expected.flags
            && registers[libc::REG_TRAPNO as usize] == 13
            && registers[libc::REG_ERR as usize] == 0
            && general_matches(expected, registers))
        .then_some(event)
    }

    pub(crate) fn complete_precise(
        &mut self,
        completion: Completion,
        clock: u64,
    ) -> Option<Observation> {
        if completion.generation.is_none()
            || (self.captured != Some(completion) && self.expected != Some(completion))
            || clock != completion.clock
        {
            return None;
        }
        let completed = self.completed.checked_add(1)?;
        self.expected = None;
        self.native = None;
        self.captured = None;
        self.completed = completed;
        self.last = Some(completion);
        Some(Observation {
            generation: completion.generation?,
            sequence: completion.sequence,
            clock,
            rip: completion.pc,
        })
    }

    pub(crate) fn cancel_fault(&mut self, clock: u64) -> Option<()> {
        let (_, expected) = self.fault?;
        if clock != expected.clock {
            return None;
        }
        self.fault = None;
        Some(())
    }

    pub(crate) fn validate_syscall(
        &self,
        owner: i64,
        frame: u64,
        number: i64,
        registers: &[libc::greg_t; 23],
    ) -> Option<()> {
        let (expected_number, expected) = self.syscall?;
        let general = expected.general?;
        (owner == expected.owner
            && frame == expected.frame_generation
            && number == expected_number
            && registers[libc::REG_RIP as usize] as u64 == expected.pc
            && registers[libc::REG_EFL as usize] as u64 == expected.flags
            && general
                .iter()
                .zip(registers)
                .enumerate()
                .all(|(index, (expected, actual))| {
                    index == libc::REG_RAX as usize || *expected == *actual as u64
                }))
        .then_some(())
    }

    pub(crate) fn cancel_syscall(&mut self, clock: u64) -> Option<()> {
        let (_, expected) = self.syscall?;
        if clock != expected.clock {
            return None;
        }
        self.syscall = None;
        Some(())
    }
}

impl Stepper {
    fn capture_refusal_reason(
        &self,
        owner: i64,
        frame_generation: u64,
        registers: &[libc::greg_t; 23],
        clock: u64,
    ) -> &'static str {
        if self.captured.is_some() {
            return "capture/already-captured";
        }
        let pc = registers[libc::REG_RIP as usize] as u64;
        let flags = registers[libc::REG_EFL as usize] as u64;
        if let Some((staged, _)) = self.native {
            if owner != staged.owner {
                "capture/native-owner"
            } else if frame_generation != staged.frame_generation {
                "capture/native-frame"
            } else if clock < staged.clock {
                "capture/native-clock-regression"
            } else if native::CpuStepRequest::trace_control(registers) {
                "capture/unclassified"
            } else if pc == 0 {
                "capture/native-pc-zero"
            } else if pc >= 1 << 47 {
                "capture/native-pc-range"
            } else if flags & (0x10000 | 0x100) != 0x100 {
                "capture/native-trace-flags"
            } else if registers[libc::REG_TRAPNO as usize] != 1 {
                "capture/native-trap"
            } else if registers[libc::REG_ERR as usize] != 0 {
                "capture/native-error"
            } else {
                "capture/unclassified"
            }
        } else if let Some(expected) = self.expected {
            if self.validate(owner, frame_generation, registers).is_some() {
                "capture/unclassified"
            } else if owner != expected.owner {
                "capture/expected-owner"
            } else if frame_generation != expected.frame_generation {
                "capture/expected-frame"
            } else if pc != expected.pc {
                "capture/expected-pc"
            } else if flags != expected.flags {
                "capture/expected-flags"
            } else if registers[libc::REG_TRAPNO as usize] != 1 {
                "capture/expected-trap"
            } else if registers[libc::REG_ERR as usize] != 0 {
                "capture/expected-error"
            } else if !general_matches(expected, registers) {
                "capture/expected-general"
            } else {
                "capture/unclassified"
            }
        } else {
            "capture/no-native-or-expected"
        }
    }

    pub(crate) fn report_capture_refusal(
        &self,
        owner: i64,
        frame_generation: u64,
        registers: &[libc::greg_t; 23],
        clock: u64,
        mut emit: impl FnMut(&'static str, &'static str, Option<i64>),
    ) {
        let reason = self.capture_refusal_reason(owner, frame_generation, registers, clock);
        emit(reason, "reason", None);
        for (field, present) in [
            ("pending-captured", self.captured.is_some()),
            ("pending-native", self.native.is_some()),
            ("pending-expected", self.expected.is_some()),
            ("pending-fault", self.fault.is_some()),
            ("pending-syscall", self.syscall.is_some()),
            ("pending-call", self.call.is_some()),
        ] {
            emit(reason, field, Some(i64::from(present)));
        }
        emit(reason, "observed-owner", Some(owner));
        emit(
            reason,
            "observed-trap",
            Some(registers[libc::REG_TRAPNO as usize]),
        );
        emit(
            reason,
            "observed-error",
            Some(registers[libc::REG_ERR as usize]),
        );
        for (high, low, value) in [
            (
                "observed-frame-high32",
                "observed-frame-low32",
                frame_generation,
            ),
            ("observed-clock-high32", "observed-clock-low32", clock),
            (
                "observed-pc-high32",
                "observed-pc-low32",
                registers[libc::REG_RIP as usize] as u64,
            ),
            (
                "observed-flags-high32",
                "observed-flags-low32",
                registers[libc::REG_EFL as usize] as u64,
            ),
        ] {
            emit(reason, high, Some((value >> 32) as i64));
            emit(reason, low, Some(i64::from(value as u32)));
        }
        let staged = self
            .captured
            .or_else(|| self.native.map(|(completion, _)| completion))
            .or(self.expected)
            .or_else(|| self.fault.map(|(_, completion)| completion))
            .or_else(|| self.syscall.map(|(_, completion)| completion))
            .or(self.call);
        if let Some(staged) = staged {
            emit(reason, "staged-owner", Some(staged.owner));
            emit(
                reason,
                "ticket-present",
                Some(i64::from(staged.generation.is_some())),
            );
            if let Some(generation) = staged.generation {
                emit(
                    reason,
                    "ticket-generation-high32",
                    Some((generation.value() >> 32) as i64),
                );
                emit(
                    reason,
                    "ticket-generation-low32",
                    Some(i64::from(generation.value() as u32)),
                );
            }
            for (high, low, value) in [
                (
                    "staged-frame-high32",
                    "staged-frame-low32",
                    staged.frame_generation,
                ),
                (
                    "ticket-sequence-high32",
                    "ticket-sequence-low32",
                    staged.sequence,
                ),
                ("staged-clock-high32", "staged-clock-low32", staged.clock),
                ("staged-pc-high32", "staged-pc-low32", staged.pc),
                ("staged-flags-high32", "staged-flags-low32", staged.flags),
            ] {
                emit(reason, high, Some((value >> 32) as i64));
                emit(reason, low, Some(i64::from(value as u32)));
            }
        }
        if let Some((_, request)) = &self.native {
            let (bytes, length) = request.retained_instruction();
            emit(reason, "request-pc-high32", Some((request.pc >> 32) as i64));
            emit(
                reason,
                "request-pc-low32",
                Some(i64::from(request.pc as u32)),
            );
            emit(reason, "request-length", Some(length as i64));
            for (field, byte) in [
                "request-byte-0",
                "request-byte-1",
                "request-byte-2",
                "request-byte-3",
                "request-byte-4",
                "request-byte-5",
                "request-byte-6",
                "request-byte-7",
                "request-byte-8",
                "request-byte-9",
                "request-byte-10",
                "request-byte-11",
                "request-byte-12",
                "request-byte-13",
                "request-byte-14",
            ]
            .into_iter()
            .zip(bytes)
            .take(length)
            {
                emit(reason, field, Some(i64::from(*byte)));
            }
        }
        if reason == "capture/expected-general"
            && let Some(general) = self.expected.and_then(|expected| expected.general)
            && let Some((index, expected)) = general
                .iter()
                .enumerate()
                .find(|(index, expected)| **expected != registers[*index] as u64)
        {
            emit(reason, "general-index", Some(index as i64));
            for (high, low, value) in [
                (
                    "general-expected-high32",
                    "general-expected-low32",
                    *expected,
                ),
                (
                    "general-observed-high32",
                    "general-observed-low32",
                    registers[index] as u64,
                ),
            ] {
                emit(reason, high, Some((value >> 32) as i64));
                emit(reason, low, Some(i64::from(value as u32)));
            }
        }
    }
}

fn general_matches(expected: Completion, registers: &[libc::greg_t; 23]) -> bool {
    expected.general.is_none_or(|general| {
        general
            .iter()
            .zip(registers)
            .all(|(expected, actual)| *expected == *actual as u64)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read_fault_fixture(ticket: Option<crate::timer::Ticket>) -> (Stepper, [i64; 23]) {
        let mut context: HookContext = unsafe { core::mem::zeroed() };
        context.instruction_pointer = 0x10110b;
        context.rflags = 0x246;
        context.rax = 0x1122334455667788;
        context.rcx = 99;
        let request = native::CpuStepRequest::decode_recorded(
            context.instruction_pointer,
            &[0x48, 0x8b, 0x0d, 0xee, 0xae, 0xff, 0xff],
            &context,
            false,
            &mut native::Refusal::new("test"),
        )
        .unwrap();
        let mut stepper = Stepper::default();
        stepper
            .stage_cpu((17, 3), ticket, request, &context, 40)
            .unwrap();
        let mut registers = [0; 23];
        for (slot, value) in registers.iter_mut().zip(native::general(&context)) {
            *slot = value as i64;
        }
        registers[libc::REG_RIP as usize] = context.instruction_pointer as i64;
        registers[libc::REG_EFL as usize] = (context.rflags | 0x10100) as i64;
        registers[libc::REG_TRAPNO as usize] = 14;
        registers[libc::REG_ERR as usize] = 4;
        (stepper, registers)
    }

    fn read_fault(registers: &[i64; 23]) -> crate::vdso::Fault<'_> {
        crate::vdso::Fault {
            signal: libc::SIGSEGV,
            code: 2,
            address: 0xfc000,
            registers,
        }
    }

    #[test]
    fn modeled_read_capture_consumes_once_without_trace_or_clock_charge() {
        for armed in [false, true] {
            let mut controller = Controller::default();
            let (generation, _) = controller.replace(40, 2, 1).unwrap();
            let ticket = armed.then_some(crate::timer::Ticket {
                generation,
                sequence: 1,
            });
            let (mut stepper, registers) = read_fault_fixture(ticket);
            assert!(stepper.capture(17, 3, &registers, 42).is_none());
            let captured = stepper
                .capture_read_fault(17, 3, read_fault(&registers), 42)
                .unwrap();
            assert_eq!(captured.clock(), 42);
            assert_eq!(captured.identity().frame_generation, 3);
            assert_eq!(captured.request().input_flags(), 0x246);
            assert_eq!(captured.flags(), 0x10346);
            assert!(stepper.pending());
            assert!(
                stepper
                    .capture_read_fault(17, 3, read_fault(&registers), 42)
                    .is_none()
            );
            assert!(stepper.capture(17, 3, &registers, 42).is_none());
            for (owner, frame, successor) in
                [(18, 4, 0x101112), (17, 3, 0x101112), (17, 4, 0x101113)]
            {
                assert!(
                    stepper
                        .complete_modeled(captured, owner, frame, successor)
                        .is_none()
                );
                assert!(stepper.pending());
            }
            let observed = stepper.complete_modeled(captured, 17, 4, 0x101112).unwrap();
            if armed {
                let observed = observed.unwrap();
                assert_eq!(
                    (observed.sequence, observed.clock, observed.rip),
                    (1, 42, 0x101112)
                );
                assert_eq!(controller.observe(observed).unwrap(), Decision::Step);
            } else {
                assert!(observed.is_none());
            }
            assert!(!stepper.pending());
            assert_eq!(stepper.completed, 0);
            assert!(stepper.last.is_none());
            assert_eq!(stepper.modeled_completed, 1);
            assert!(
                stepper
                    .complete_modeled(captured, 17, 4, 0x101112)
                    .is_none()
            );
        }
    }

    #[test]
    fn modeled_read_capture_requires_exact_owner_fault_and_unmodified_inputs() {
        for mutation in 0..25 {
            let (mut stepper, mut registers) = read_fault_fixture(None);
            let mut owner = 17;
            let mut frame = 3;
            let mut clock = 40;
            let mut signal = libc::SIGSEGV;
            let mut code = 2;
            match mutation {
                0..16 => registers[mutation] ^= 1,
                16 => owner += 1,
                17 => frame += 1,
                18 => clock -= 1,
                19 => registers[libc::REG_RIP as usize] += 1,
                20 => registers[libc::REG_EFL as usize] ^= 0x400,
                21 => registers[libc::REG_EFL as usize] &= !0x100,
                22 => registers[libc::REG_ERR as usize] |= 2,
                23 => signal = libc::SIGBUS,
                24 => code = 1,
                _ => unreachable!(),
            }
            let fault = crate::vdso::Fault {
                signal,
                code,
                address: 0xfc000,
                registers: &registers,
            };
            assert!(
                stepper
                    .capture_read_fault(owner, frame, fault, clock)
                    .is_none(),
                "mutation {mutation}"
            );
            assert!(stepper.pending());
            assert!(stepper.native.is_some());
            assert!(stepper.modeled.is_none());
            assert_eq!(stepper.modeled_completed, 0);
        }
    }

    #[test]
    fn modeled_read_cancel_revokes_pending_input_without_retirement() {
        let (mut stepper, registers) = read_fault_fixture(None);
        let captured = stepper
            .capture_read_fault(17, 3, read_fault(&registers), 44)
            .unwrap();
        stepper.cancel();
        assert!(!stepper.pending());
        assert!(stepper.validate_modeled(captured, 17, 4).is_none());
        assert!(
            stepper
                .complete_modeled(captured, 17, 4, 0x101112)
                .is_none()
        );
        assert_eq!((stepper.completed, stepper.modeled_completed), (0, 0));
    }

    #[test]
    fn modeled_read_keeps_observed_rf_instead_of_constructing_fault_flags() {
        for observed_rf in [0, 0x10000] {
            let (mut stepper, mut registers) = read_fault_fixture(None);
            registers[libc::REG_EFL as usize] = 0x346 | observed_rf;
            let captured = stepper
                .capture_read_fault(17, 3, read_fault(&registers), 40)
                .unwrap();
            assert_eq!(captured.request().input_flags(), 0x246);
            assert_eq!(captured.flags(), (0x346 | observed_rf) as u64);
        }
        let (mut stepper, mut registers) = read_fault_fixture(None);
        let (mut staged, previous) = stepper.native.unwrap();
        let mut original: HookContext = unsafe { core::mem::zeroed() };
        original.instruction_pointer = previous.pc;
        original.rflags = previous.input_flags() | 0x10000;
        original.rax = 0x1122334455667788;
        original.rcx = 99;
        let (bytes, length) = previous.retained_instruction();
        let request = native::CpuStepRequest::decode_recorded(
            previous.pc,
            &bytes[..length],
            &original,
            true,
            &mut native::Refusal::new("test"),
        )
        .unwrap();
        staged.flags = original.rflags;
        stepper.native = Some((staged, request));
        registers[libc::REG_EFL as usize] &= !0x10000;
        assert!(
            stepper
                .capture_read_fault(17, 3, read_fault(&registers), 40)
                .is_none()
        );
        registers[libc::REG_EFL as usize] |= 0x10000;
        let captured = stepper
            .capture_read_fault(17, 3, read_fault(&registers), 40)
            .unwrap();
        assert_eq!(captured.flags(), 0x10346);
        assert_eq!(captured.request().input_flags(), 0x10246);
    }

    #[test]
    fn optional_ticket_cpu_steps_keep_real_counts_and_one_use_after_delivery() {
        use reverie_preload::precise_timer::Decision;
        let mut controller = Controller::default();
        let (generation, _) = controller.replace(40, 2, 0).unwrap();
        let mut stepper = Stepper::default();
        let mut context: HookContext = unsafe { core::mem::zeroed() };
        context.instruction_pointer = 0x4000;
        context.rflags = 0x202;
        let mut previous = None;
        for (index, count) in [40, 41, 42, 47].into_iter().enumerate() {
            let ticket = (index < 3).then_some(crate::timer::Ticket {
                generation,
                sequence: index as u64 + 1,
            });
            let request = native::CpuStepRequest::decode_recorded(
                0x4000,
                &[0xf3, 0x48, 0xab],
                &context,
                false,
                &mut native::Refusal::new("test"),
            )
            .unwrap();
            stepper
                .stage_cpu(
                    (123, 4 + index as u64),
                    ticket,
                    request,
                    &context,
                    previous.map_or(40, |completion: Completion| completion.clock),
                )
                .unwrap();
            let captured = registers(0x4000);
            let completion = stepper
                .capture(123, 4 + index as u64, &captured, count)
                .unwrap();
            if let Some(previous) = previous {
                assert!(stepper.complete_cpu(previous, count).is_none());
            }
            assert!(stepper.complete_cpu(completion, count + 1).is_none());
            let observation = stepper.complete_cpu(completion, count).unwrap();
            assert!(stepper.complete_cpu(completion, count).is_none());
            if index < 3 {
                let observation = observation.unwrap();
                assert_eq!(observation.clock, count);
                assert_eq!(
                    controller.observe(observation),
                    Ok(if index == 2 {
                        Decision::Deliver
                    } else {
                        Decision::Step
                    })
                );
            } else {
                assert!(observation.is_none());
            }
            previous = Some(completion);
        }
        assert_eq!(stepper.completed, 4);
        assert_eq!(stepper.last.unwrap().clock, 47);
    }

    #[test]
    fn unarmed_ret_uses_captured_cpu_registers_without_predicted_return() {
        let mut stepper = Stepper::default();
        let mut context: HookContext = unsafe { core::mem::zeroed() };
        context.instruction_pointer = 0x4000;
        context.stack_pointer = 0x7000;
        context.rflags = 0x202;
        let request = native::CpuStepRequest::decode_recorded(
            0x4000,
            &[0xc3],
            &context,
            false,
            &mut native::Refusal::new("test"),
        )
        .unwrap();
        let mut stale = context;
        stale.stack_pointer += 8;
        assert!(
            stepper
                .stage_cpu((123, 4), None, request, &stale, 90)
                .is_none()
        );
        stepper
            .stage_cpu((123, 4), None, request, &context, 90)
            .unwrap();
        let mut captured = registers(0x9876);
        captured[libc::REG_RSP as usize] = 0x7008;
        assert!(stepper.capture(124, 4, &captured, 91).is_none());
        assert!(stepper.capture(123, 5, &captured, 91).is_none());
        let completion = stepper.capture(123, 4, &captured, 91).unwrap();
        assert_eq!(completion.pc, 0x9876);
        assert_eq!(completion.general.unwrap()[15], 0x7008);
        assert!(stepper.complete_cpu(completion, 91).unwrap().is_none());
        assert_eq!(stepper.completed, 1);
    }

    #[test]
    fn cpu_data_fault_authenticates_pending_request_without_retiring_partial_state() {
        let mut stepper = Stepper::default();
        let mut context: HookContext = unsafe { core::mem::zeroed() };
        context.instruction_pointer = 0x4000;
        context.rflags = 0x202;
        let request = native::CpuStepRequest::decode_recorded(
            0x4000,
            &[0xf3, 0x48, 0xab],
            &context,
            false,
            &mut native::Refusal::new("test"),
        )
        .unwrap();
        stepper
            .stage_cpu((123, 4), None, request, &context, 40)
            .unwrap();
        let mut captured = registers(0x4000);
        captured[libc::REG_TRAPNO as usize] = 14;
        captured[libc::REG_ERR as usize] = 6;
        captured[libc::REG_EFL as usize] |= 0x10000;
        captured[libc::REG_RCX as usize] = 3;
        captured[libc::REG_RDI as usize] = 0x9000;
        assert!(stepper.capture(123, 4, &captured, 43).is_none());
        assert!(stepper.validate_cpu_fault(123, 4, &captured, 43).is_some());
        assert!(stepper.validate_cpu_fault(124, 4, &captured, 43).is_none());
        assert!(stepper.validate_cpu_fault(123, 5, &captured, 43).is_none());
        assert!(stepper.validate_cpu_fault(123, 4, &captured, 39).is_none());
        for field in [libc::REG_RIP, libc::REG_EFL] {
            let mut invalid = captured;
            invalid[field as usize] ^= if field == libc::REG_RIP { 1 } else { 0x100 };
            assert!(stepper.validate_cpu_fault(123, 4, &invalid, 43).is_none());
        }
        assert_eq!(captured[libc::REG_RCX as usize], 3);
        stepper.cancel();
        assert!(!stepper.pending());
        assert_eq!(stepper.completed, 0);
        assert!(stepper.validate_cpu_fault(123, 4, &captured, 43).is_none());
    }

    fn capture_diagnostic_fixture(native: bool) -> Stepper {
        let mut stepper = Stepper::default();
        stepper.stage_nop(123, 4, 0x4000, 0x202, 40).unwrap();
        if native {
            let staged = stepper.expected.take().unwrap();
            let mut context: HookContext = unsafe { core::mem::zeroed() };
            context.instruction_pointer = 0x4000;
            context.rflags = 0x202;
            let request = native::CpuStepRequest::decode_recorded(
                0x4000,
                &[0x90],
                &context,
                false,
                &mut native::Refusal::new("test"),
            )
            .unwrap();
            stepper
                .stage_precise(
                    (123, 4),
                    crate::timer::Ticket {
                        generation: staged.generation.unwrap(),
                        sequence: 7,
                    },
                    Instruction::Native(Box::new(request)),
                    &context,
                    40,
                )
                .unwrap();
        }
        stepper
    }

    fn capture_diagnostic_records(
        stepper: &Stepper,
        owner: i64,
        frame: u64,
        registers: &[libc::greg_t; 23],
        clock: u64,
    ) -> Vec<(&'static str, &'static str, Option<i64>)> {
        let before = format!("{stepper:?}");
        let original_registers = *registers;
        let mut records = Vec::new();
        stepper.report_capture_refusal(owner, frame, registers, clock, |reason, field, value| {
            records.push((reason, field, value));
        });
        assert_eq!(format!("{stepper:?}"), before);
        assert_eq!(*registers, original_registers);
        records
    }

    fn assert_capture_diagnostic(
        stepper: &mut Stepper,
        owner: i64,
        frame: u64,
        registers: &[libc::greg_t; 23],
        clock: u64,
        reason: &'static str,
    ) -> Vec<(&'static str, &'static str, Option<i64>)> {
        let before = format!("{stepper:?}");
        assert!(stepper.capture(owner, frame, registers, clock).is_none());
        assert_eq!(format!("{stepper:?}"), before);
        let records = capture_diagnostic_records(stepper, owner, frame, registers, clock);
        assert_eq!(records[0], (reason, "reason", None));
        assert!(records.iter().all(|record| record.0 == reason));
        records
    }

    #[test]
    fn capture_diagnostics_native_ordered_reasons() {
        for (owner, frame, clock, field, value, reason) in [
            (124, 5, 39, libc::REG_RIP, 0, "capture/native-owner"),
            (123, 5, 39, libc::REG_RIP, 0, "capture/native-frame"),
            (
                123,
                4,
                39,
                libc::REG_RIP,
                0,
                "capture/native-clock-regression",
            ),
            (123, 4, 40, libc::REG_RIP, 0, "capture/native-pc-zero"),
            (
                123,
                4,
                40,
                libc::REG_RIP,
                1 << 47,
                "capture/native-pc-range",
            ),
            (
                123,
                4,
                40,
                libc::REG_EFL,
                0x202,
                "capture/native-trace-flags",
            ),
            (
                123,
                4,
                40,
                libc::REG_EFL,
                0x10302,
                "capture/native-trace-flags",
            ),
            (
                123,
                4,
                40,
                libc::REG_EFL,
                0x10202,
                "capture/native-trace-flags",
            ),
            (123, 4, 40, libc::REG_TRAPNO, 2, "capture/native-trap"),
            (123, 4, 40, libc::REG_ERR, 1, "capture/native-error"),
        ] {
            let mut stepper = capture_diagnostic_fixture(true);
            let mut registers = registers(0x4001);
            registers[field as usize] = value;
            assert_capture_diagnostic(&mut stepper, owner, frame, &registers, clock, reason);
        }
        let mut stepper = capture_diagnostic_fixture(true);
        let registers = registers(0x4001);
        stepper.capture(123, 4, &registers, 41).unwrap();
        assert_capture_diagnostic(
            &mut stepper,
            124,
            5,
            &registers,
            0,
            "capture/already-captured",
        );
    }

    #[test]
    fn capture_diagnostics_expected_reasons_and_each_general_field() {
        for (owner, frame, field, value, reason) in [
            (124, 5, libc::REG_RIP, 0, "capture/expected-owner"),
            (123, 5, libc::REG_RIP, 0, "capture/expected-frame"),
            (123, 4, libc::REG_RIP, 0, "capture/expected-pc"),
            (123, 4, libc::REG_EFL, 0x303, "capture/expected-flags"),
            (123, 4, libc::REG_TRAPNO, 2, "capture/expected-trap"),
            (123, 4, libc::REG_ERR, 1, "capture/expected-error"),
        ] {
            let mut stepper = capture_diagnostic_fixture(false);
            let mut registers = registers(0x4001);
            registers[field as usize] = value;
            assert_capture_diagnostic(&mut stepper, owner, frame, &registers, 40, reason);
        }
        for field in 0..16 {
            let mut stepper = capture_diagnostic_fixture(false);
            stepper.expected.as_mut().unwrap().general = Some([0; 16]);
            let mut registers = registers(0x4001);
            registers[field] = 0x1234_5678_9abc_def0;
            if field < 15 {
                registers[field + 1] = 1;
            }
            let records = assert_capture_diagnostic(
                &mut stepper,
                123,
                4,
                &registers,
                40,
                "capture/expected-general",
            );
            for (name, value) in [
                ("general-index", field as i64),
                ("general-expected-high32", 0),
                ("general-expected-low32", 0),
                ("general-observed-high32", 0x1234_5678),
                ("general-observed-low32", 0x9abc_def0),
            ] {
                assert!(records.contains(&("capture/expected-general", name, Some(value))));
            }
        }
    }

    #[test]
    fn capture_diagnostics_pending_kinds_without_trace_request() {
        for kind in ["none", "pending-fault", "pending-syscall", "pending-call"] {
            let mut stepper = capture_diagnostic_fixture(false);
            let staged = stepper.expected.take().unwrap();
            match kind {
                "pending-fault" => {
                    let Instruction::Fault(event) =
                        Instruction::decode(0x4000, &[0x0f, 0xa2]).unwrap()
                    else {
                        panic!("CPUID fixture");
                    };
                    stepper.fault = Some((event, staged));
                }
                "pending-syscall" => stepper.syscall = Some((libc::SYS_read, staged)),
                "pending-call" => stepper.call = Some(staged),
                _ => (),
            }
            let records = assert_capture_diagnostic(
                &mut stepper,
                123,
                4,
                &registers(0x4001),
                40,
                "capture/no-native-or-expected",
            );
            for field in [
                "pending-captured",
                "pending-native",
                "pending-expected",
                "pending-fault",
                "pending-syscall",
                "pending-call",
            ] {
                assert!(records.contains(&(
                    "capture/no-native-or-expected",
                    field,
                    Some(i64::from(field == kind))
                )));
            }
        }
    }

    #[test]
    fn capture_diagnostics_exact_words_and_no_native_result_prediction() {
        let mut stepper = capture_diagnostic_fixture(true);
        let staged = &mut stepper.native.as_mut().unwrap().0;
        staged.owner = 0x1234_5678_9abc_def0;
        staged.frame_generation = 0xfedc_ba98_7654_3210;
        staged.sequence = 0x9876_5432_1234_5678;
        staged.clock = 0xf123_4567_89ab_cdef;
        staged.pc = 0x0123_4567_89ab_cdef;
        staged.flags = 0x4567_89ab_cdef_0123;
        let snapshot = *staged;
        let mut registers = registers(0x4001);
        registers[libc::REG_RIP as usize] = 0x2345_6789_abcd_ef01;
        registers[libc::REG_EFL as usize] = 0x3456_789a_bcde_f012;
        registers[libc::REG_TRAPNO as usize] = -8;
        registers[libc::REG_ERR as usize] = -9;
        let frame = 0xa234_5678_9abc_def0;
        let clock = 0xb345_6789_abcd_ef01;
        let records = assert_capture_diagnostic(
            &mut stepper,
            -7,
            frame,
            &registers,
            clock,
            "capture/native-owner",
        );
        for (high, low, value) in [
            ("observed-frame-high32", "observed-frame-low32", frame),
            ("observed-clock-high32", "observed-clock-low32", clock),
            (
                "observed-pc-high32",
                "observed-pc-low32",
                registers[libc::REG_RIP as usize] as u64,
            ),
            (
                "observed-flags-high32",
                "observed-flags-low32",
                registers[libc::REG_EFL as usize] as u64,
            ),
            (
                "staged-frame-high32",
                "staged-frame-low32",
                snapshot.frame_generation,
            ),
            (
                "ticket-generation-high32",
                "ticket-generation-low32",
                snapshot.generation.unwrap().value(),
            ),
            (
                "ticket-sequence-high32",
                "ticket-sequence-low32",
                snapshot.sequence,
            ),
            ("staged-clock-high32", "staged-clock-low32", snapshot.clock),
            ("staged-pc-high32", "staged-pc-low32", snapshot.pc),
            ("staged-flags-high32", "staged-flags-low32", snapshot.flags),
        ] {
            assert!(records.contains(&("capture/native-owner", high, Some((value >> 32) as i64))));
            assert!(records.contains(&(
                "capture/native-owner",
                low,
                Some(i64::from(value as u32))
            )));
        }
        for (name, value) in [
            ("observed-owner", -7),
            ("observed-trap", -8),
            ("observed-error", -9),
            ("staged-owner", snapshot.owner),
            ("pending-native", 1),
        ] {
            assert!(records.contains(&("capture/native-owner", name, Some(value))));
        }
        assert!(
            !records
                .iter()
                .any(|(_, field, _)| field.starts_with("general-"))
        );
    }

    #[test]
    fn capture_diagnostics_exact_retained_instruction_without_source_reread() {
        let mut inputs = vec![
            vec![0xf3, 0x48, 0xab],
            vec![0x48, 0x98],
            vec![0xf3, 0x0f, 0x1e, 0xfa],
        ];
        for length in 1..=15 {
            let mut bytes = vec![0x66; length];
            bytes[length - 1] = 0x90;
            inputs.push(bytes);
        }
        for instruction in inputs {
            let mut input = instruction.clone();
            input.extend_from_slice(&[0xcc; 15]);
            let mut context: HookContext = unsafe { core::mem::zeroed() };
            context.instruction_pointer = 0x1234_5678_9000;
            context.rflags = 0x202;
            let request = native::CpuStepRequest::decode_recorded(
                context.instruction_pointer,
                &input,
                &context,
                false,
                &mut native::Refusal::new("test"),
            )
            .unwrap();
            let before = request;
            let (retained, length) = request.retained_instruction();
            assert_eq!(length, instruction.len());
            assert_eq!(&retained[..length], instruction);
            assert_eq!(request, before);
            input.fill(0xcc);
            let mut stepper = capture_diagnostic_fixture(true);
            let (staged, pending) = stepper.native.as_mut().unwrap();
            staged.pc = context.instruction_pointer;
            *pending = request;
            let records = assert_capture_diagnostic(
                &mut stepper,
                124,
                4,
                &registers(0x7777),
                40,
                "capture/native-owner",
            );
            assert!(
                records.contains(&("capture/native-owner", "request-pc-high32", Some(0x1234),))
            );
            assert!(records.contains(&(
                "capture/native-owner",
                "request-pc-low32",
                Some(0x5678_9000),
            )));
            assert!(records.contains(&(
                "capture/native-owner",
                "request-length",
                Some(instruction.len() as i64),
            )));
            let emitted: Vec<_> = records
                .iter()
                .filter(|(_, field, _)| field.starts_with("request-byte-"))
                .collect();
            assert_eq!(emitted.len(), instruction.len());
            for (index, (record, byte)) in emitted.iter().zip(&instruction).enumerate() {
                assert_eq!(record.1, format!("request-byte-{index}"));
                assert_eq!(record.2, Some(i64::from(*byte)));
            }
            assert_eq!(stepper.native.unwrap().1, before);
        }
    }

    #[test]
    fn capture_diagnostics_does_not_invent_bytes_without_pending_request() {
        for captured in [false, true] {
            let mut stepper = capture_diagnostic_fixture(captured);
            if captured {
                stepper.capture(123, 4, &registers(0x7777), 40).unwrap();
            }
            let records = assert_capture_diagnostic(
                &mut stepper,
                124,
                4,
                &registers(0x7777),
                40,
                if captured {
                    "capture/already-captured"
                } else {
                    "capture/expected-owner"
                },
            );
            assert!(
                !records
                    .iter()
                    .any(|(_, field, _)| field.starts_with("request-"))
            );
        }
    }

    #[test]
    fn capture_diagnostics_success_is_silent_and_description_is_not_acceptance() {
        for native in [false, true] {
            for clock in [40, 41, u64::MAX] {
                let mut stepper = capture_diagnostic_fixture(native);
                let mut registers = registers(if native { 0x7777 } else { 0x4001 });
                registers[libc::REG_RAX as usize] = -1;
                let records = capture_diagnostic_records(&stepper, 123, 4, &registers, clock);
                assert_eq!(records[0], ("capture/unclassified", "reason", None));
                let mut failure_output = Vec::new();
                let completion = stepper
                    .capture(123, 4, &registers, clock)
                    .unwrap_or_else(|| {
                        stepper.report_capture_refusal(
                            123,
                            4,
                            &registers,
                            clock,
                            |reason, field, value| failure_output.push((reason, field, value)),
                        );
                        panic!("unexpected refusal");
                    });
                assert!(failure_output.is_empty());
                assert_eq!(completion.clock, if native { clock } else { 40 });
                assert_eq!(completion.pc, registers[libc::REG_RIP as usize] as u64);
            }
        }
        let mut stepper = capture_diagnostic_fixture(false);
        assert!(stepper.capture(123, 4, &registers(0x4001), 39).is_some());
    }

    #[test]
    fn vdso_call_fault_has_one_use_without_instruction_or_clock_retirement() {
        let mut controller = Controller::default();
        let (generation, _) = controller.replace(40, 0, 1).unwrap();
        let mut context: HookContext = unsafe { core::mem::zeroed() };
        context.instruction_pointer = 0x10800;
        context.stack_pointer = 0x20008;
        context.rflags = 0x202;
        context.rdi = 0x21000;
        let mut stepper = Stepper::default();
        let ticket = crate::timer::Ticket {
            generation,
            sequence: 1,
        };
        stepper.stage_call((123, 4), ticket, &context, 40).unwrap();
        assert!(stepper.stage_call((123, 4), ticket, &context, 40).is_none());
        let mut registers = [0; 23];
        registers[libc::REG_RIP as usize] = 0x10800;
        registers[libc::REG_RSP as usize] = 0x20008;
        registers[libc::REG_EFL as usize] = 0x10302;
        registers[libc::REG_RDI as usize] = 0x21000;
        registers[libc::REG_TRAPNO as usize] = 14;
        registers[libc::REG_ERR as usize] = 0x15;
        assert!(stepper.validate_call(123, 4, &registers).is_some());
        assert!(stepper.validate_call(124, 4, &registers).is_none());
        assert!(stepper.validate_call(123, 5, &registers).is_none());
        for index in (0..18).chain([libc::REG_TRAPNO as usize, libc::REG_ERR as usize]) {
            let mut changed = registers;
            changed[index] ^= 1;
            assert!(
                stepper.validate_call(123, 4, &changed).is_none(),
                "field {index}"
            );
        }
        assert!(stepper.cancel_call(41).is_none());
        assert!(stepper.pending());
        stepper.cancel_call(40).unwrap();
        assert_eq!(stepper.completed, 0);
        assert!(stepper.last.is_none());
        assert!(!stepper.pending());
        assert!(stepper.validate_call(123, 4, &registers).is_none());
        assert!(stepper.cancel_call(40).is_none());
    }

    #[test]
    fn native_completion_binds_request_frame_sequence_clock_and_one_use() {
        let mut controller = Controller::default();
        let (generation, _) = controller.replace(40, 0, 1).unwrap();
        let mut context: HookContext = unsafe { core::mem::zeroed() };
        context.instruction_pointer = 0x4000;
        context.rflags = 0x202;
        context.rcx = 7;
        let request = native::CpuStepRequest::decode_recorded(
            0x4000,
            &[0x48, 0x89, 0xc8],
            &context,
            false,
            &mut native::Refusal::new("test"),
        )
        .unwrap();
        let mut stepper = Stepper::default();
        stepper
            .stage_precise(
                (123, 4),
                crate::timer::Ticket {
                    generation,
                    sequence: 1,
                },
                Instruction::Native(Box::new(request)),
                &context,
                40,
            )
            .unwrap();
        let mut registers = registers(0x4003);
        registers[libc::REG_RAX as usize] = 7;
        registers[libc::REG_RCX as usize] = 7;
        assert!(stepper.capture(124, 4, &registers, 43).is_none());
        assert!(stepper.capture(123, 5, &registers, 43).is_none());
        assert!(stepper.capture(123, 4, &registers, 39).is_none());
        for field in [libc::REG_TRAPNO, libc::REG_ERR] {
            let mut invalid = registers;
            invalid[field as usize] ^= 1;
            assert!(stepper.capture(123, 4, &invalid, 43).is_none());
        }
        for flags in [0x202, 0x10302] {
            let mut invalid = registers;
            invalid[libc::REG_EFL as usize] = flags;
            assert!(stepper.capture(123, 4, &invalid, 43).is_none());
        }
        let completion = stepper.capture(123, 4, &registers, 43).unwrap();
        assert!(stepper.capture(123, 4, &registers, 43).is_none());
        assert!(stepper.complete_precise(completion, 44).is_none());
        let mut wrong_sequence = completion;
        wrong_sequence.sequence += 1;
        assert!(stepper.complete_precise(wrong_sequence, 43).is_none());
        for field in 0..16 {
            let mut changed = completion;
            changed.general.as_mut().unwrap()[field] ^= 1;
            assert!(stepper.complete_precise(changed, 43).is_none());
        }
        for changed in [
            Completion {
                pc: completion.pc + 1,
                ..completion
            },
            Completion {
                flags: completion.flags ^ 1,
                ..completion
            },
            Completion {
                owner: 124,
                ..completion
            },
            Completion {
                frame_generation: 5,
                ..completion
            },
            Completion {
                clock: 44,
                ..completion
            },
        ] {
            assert!(stepper.complete_precise(changed, changed.clock).is_none());
        }
        stepper.cancel();
        assert!(stepper.complete_precise(completion, 43).is_none());
        let (generation, _) = controller.replace(40, 0, 1).unwrap();
        stepper
            .stage_precise(
                (123, 5),
                crate::timer::Ticket {
                    generation,
                    sequence: 1,
                },
                Instruction::Native(Box::new(request)),
                &context,
                40,
            )
            .unwrap();
        assert!(stepper.complete_precise(completion, 43).is_none());
        let completion = stepper.capture(123, 5, &registers, 43).unwrap();
        assert_eq!(stepper.complete_precise(completion, 43).unwrap().clock, 43);
        assert!(stepper.complete_precise(completion, 43).is_none());
        assert_eq!(stepper.completed, 1);
    }

    #[test]
    fn same_pc_rep_stops_use_actual_counter_and_each_timer_occurrence() {
        let mut controller = Controller::default();
        let (generation, _) = controller.replace(40, 0, 3).unwrap();
        let mut stepper = Stepper::default();
        let mut context: HookContext = unsafe { core::mem::zeroed() };
        context.instruction_pointer = 0x4000;
        context.rflags = 0x602;
        context.rcx = 3;
        context.rdi = 0x20010;
        context.rax = 0x12345678;
        let mut clock = 40;
        for (index, actual_clock) in [40, 42, 43].into_iter().enumerate() {
            let request = native::CpuStepRequest::decode_recorded(
                0x4000,
                &[0xf3, 0x48, 0xab],
                &context,
                false,
                &mut native::Refusal::new("test"),
            )
            .unwrap();
            stepper
                .stage_precise(
                    (123, index as u64 + 1),
                    crate::timer::Ticket {
                        generation,
                        sequence: index as u64 + 1,
                    },
                    Instruction::Native(Box::new(request)),
                    &context,
                    clock,
                )
                .unwrap();
            let mut registers = registers(if index == 2 { 0x4003 } else { 0x4000 });
            registers[..16].copy_from_slice(&native::general(&context).map(|value| value as i64));
            registers[libc::REG_EFL as usize] = 0x702;
            registers[libc::REG_RCX as usize] -= 1;
            registers[libc::REG_RDI as usize] -= 8;
            let completion = stepper
                .capture(123, index as u64 + 1, &registers, actual_clock)
                .unwrap();
            let observation = stepper.complete_precise(completion, actual_clock).unwrap();
            assert_eq!(observation.clock, actual_clock);
            assert_eq!(
                controller.observe(observation),
                Ok(if index == 2 {
                    Decision::Deliver
                } else {
                    Decision::Step
                })
            );
            context.rcx -= 1;
            context.rdi -= 8;
            clock = actual_clock;
        }
        assert_eq!(stepper.completed, 3);
    }

    #[test]
    fn positive_target_uses_actual_counts_at_same_pc_and_preserves_overshoot() {
        fn observe(counts: &[u64]) -> Vec<Result<Decision, reverie_preload::precise_timer::Error>> {
            let mut controller = Controller::default();
            let (generation, _) = controller.replace(40, 2, 3).unwrap();
            let mut context: HookContext = unsafe { core::mem::zeroed() };
            context.instruction_pointer = 0x4000;
            context.rflags = 0x202;
            context.rcx = counts.len() as u64 + 1;
            let mut stepper = Stepper::default();
            let mut clock = 40;
            counts
                .iter()
                .enumerate()
                .map(|(index, actual)| {
                    let request = native::CpuStepRequest::decode_recorded(
                        0x4000,
                        &[0xf3, 0x48, 0xab],
                        &context,
                        false,
                        &mut native::Refusal::new("test"),
                    )
                    .unwrap();
                    stepper
                        .stage_precise(
                            (123, index as u64 + 1),
                            crate::timer::Ticket {
                                generation,
                                sequence: index as u64 + 1,
                            },
                            Instruction::Native(Box::new(request)),
                            &context,
                            clock,
                        )
                        .unwrap();
                    let mut registers = registers(0x4000);
                    registers[..16]
                        .copy_from_slice(&native::general(&context).map(|value| value as i64));
                    registers[libc::REG_RCX as usize] -= 1;
                    let completion = stepper
                        .capture(123, index as u64 + 1, &registers, *actual)
                        .unwrap();
                    let observation = stepper.complete_precise(completion, *actual).unwrap();
                    assert_eq!(observation.clock, *actual);
                    assert_eq!(observation.rip, 0x4000);
                    clock = *actual;
                    context.rcx -= 1;
                    controller.observe(observation)
                })
                .collect()
        }
        assert_eq!(
            observe(&[40, 40, 41, 42, 42, 43, 44]),
            vec![
                Ok(Decision::Step),
                Ok(Decision::Step),
                Ok(Decision::Step),
                Ok(Decision::Step),
                Ok(Decision::Step),
                Ok(Decision::Step),
                Ok(Decision::Deliver),
            ]
        );
        assert_eq!(
            observe(&[40, 43]),
            vec![
                Ok(Decision::Step),
                Err(reverie_preload::precise_timer::Error::Overshot)
            ]
        );
    }

    #[test]
    fn page_fault_is_not_a_cpu_step_and_cancel_does_not_retire_it() {
        let mut controller = Controller::default();
        let (generation, _) = controller.replace(40, 0, 1).unwrap();
        let mut context: HookContext = unsafe { core::mem::zeroed() };
        context.instruction_pointer = 0x4000;
        context.rflags = 0x202;
        let request = native::CpuStepRequest::decode_recorded(
            0x4000,
            &[0xf3, 0x48, 0xab],
            &context,
            false,
            &mut native::Refusal::new("test"),
        )
        .unwrap();
        let mut stepper = Stepper::default();
        stepper
            .stage_precise(
                (123, 1),
                crate::timer::Ticket {
                    generation,
                    sequence: 1,
                },
                Instruction::Native(Box::new(request)),
                &context,
                40,
            )
            .unwrap();
        let mut registers = registers(0x4000);
        registers[libc::REG_TRAPNO as usize] = 14;
        registers[libc::REG_ERR as usize] = 6;
        registers[libc::REG_EFL as usize] = 0x10302;
        assert!(stepper.capture(123, 1, &registers, 41).is_none());
        assert!(stepper.pending());
        stepper.cancel();
        assert!(!stepper.pending());
        assert_eq!(stepper.completed, 0);
        assert!(stepper.last.is_none());
    }

    #[test]
    fn precise_completion_checks_request_sequence_all_registers_and_exact_branch_count() {
        let mut controller = Controller::default();
        let (generation, _) = controller.replace(40, 2, 3).unwrap();
        let ticket = crate::timer::Ticket {
            generation,
            sequence: 7,
        };
        let mut context: HookContext = unsafe { core::mem::zeroed() };
        context.instruction_pointer = 0x4000;
        context.rflags = 0x202;
        context.rcx = 3;
        let mut stepper = Stepper::default();
        stepper
            .stage_precise(
                (123, 4),
                ticket,
                Instruction::Zero {
                    target: 0x4000,
                    fallthrough: 0x4002,
                    zero: false,
                },
                &context,
                40,
            )
            .unwrap();
        let mut registers = registers(0x4000);
        registers[libc::REG_RCX as usize] = 3;
        for index in 0..16 {
            let mut invalid = registers;
            invalid[index] ^= 1;
            assert!(stepper.validate(123, 4, &invalid).is_none());
        }
        let completion = stepper.validate(123, 4, &registers).unwrap();
        assert!(stepper.complete_precise(completion, 40).is_none());
        let observation = stepper.complete_precise(completion, 41).unwrap();
        assert_eq!(
            (
                observation.generation,
                observation.sequence,
                observation.rip
            ),
            (generation, 7, 0x4000)
        );
        assert!(stepper.complete_precise(completion, 41).is_none());
    }

    #[test]
    fn subscribed_fault_is_not_a_trace_completion_and_cancellation_consumes_it() {
        let mut controller = Controller::default();
        let (generation, _) = controller.replace(40, 2, 3).unwrap();
        let ticket = crate::timer::Ticket {
            generation,
            sequence: 2,
        };
        let mut context: HookContext = unsafe { core::mem::zeroed() };
        context.instruction_pointer = 0x4000;
        context.rflags = 0x202;
        let instruction = Instruction::decode(0x4000, &[0x0f, 0xa2]).unwrap();
        let mut stepper = Stepper::default();
        stepper
            .stage_precise((123, 4), ticket, instruction, &context, 40)
            .unwrap();
        let mut registers = registers(0x4000);
        registers[libc::REG_EFL as usize] = 0x10302;
        registers[libc::REG_TRAPNO as usize] = 13;
        assert!(stepper.validate(123, 4, &registers).is_none());
        assert!(stepper.validate_fault(124, 4, &registers).is_none());
        assert!(stepper.validate_fault(123, 5, &registers).is_none());
        assert!(stepper.validate_fault(123, 4, &registers).is_some());
        assert!(stepper.cancel_fault(41).is_none());
        assert!(stepper.pending());
        stepper.cancel_fault(40).unwrap();
        assert!(!stepper.pending());
        assert!(stepper.validate_fault(123, 4, &registers).is_none());
        assert_eq!(stepper.completed, 0);
    }

    fn registers(pc: u64) -> [libc::greg_t; 23] {
        let mut registers = [0; 23];
        registers[libc::REG_RIP as usize] = pc as i64;
        registers[libc::REG_EFL as usize] = 0x302;
        registers[libc::REG_TRAPNO as usize] = 1;
        registers
    }

    #[test]
    fn syscall_token_cancels_without_retirement_and_requires_exact_profile() {
        assert_syscall_capture(libc::SYS_getpid);
    }

    #[test]
    fn unsupported_effect_capture_preserves_owner_frame_controls_and_clock() {
        for number in [
            libc::SYS_execve,
            libc::SYS_clone,
            libc::SYS_rt_sigreturn,
            0x3fff_ffff,
        ] {
            assert!(!crate::syscall_event::injectable(number));
            assert_syscall_capture(number);
        }
    }

    #[test]
    fn ordinary_file_effect_capture_preserves_owner_frame_controls_and_clock() {
        for number in [
            libc::SYS_lseek,
            libc::SYS_write,
            libc::SYS_writev,
            libc::SYS_pwritev,
            libc::SYS_pwrite64,
            libc::SYS_readv,
            libc::SYS_preadv,
        ] {
            assert!(crate::syscall_event::injectable(number));
            assert!(!crate::syscall_event::backed_returning(number));
            assert_syscall_capture(number);
        }
    }

    fn assert_syscall_capture(number: i64) {
        let mut controller = Controller::default();
        let (generation, _) = controller.replace(40, 2, 3).unwrap();
        let ticket = crate::timer::Ticket {
            generation,
            sequence: 2,
        };
        let mut context: HookContext = unsafe { core::mem::zeroed() };
        context.instruction_pointer = 0x4000;
        context.rflags = 0x202;
        context.rax = number as u64;
        let mut stepper = Stepper::default();
        stepper
            .stage_precise((123, 4), ticket, Instruction::Syscall, &context, 40)
            .unwrap();
        let mut registers = registers(0x4002);
        registers[libc::REG_RCX as usize] = 0x4002;
        registers[libc::REG_R11 as usize] = 0x302;
        registers[libc::REG_RAX as usize] = -999;
        registers[libc::REG_TRAPNO as usize] = 73;
        registers[libc::REG_ERR as usize] = 92;
        assert!(stepper.validate(123, 4, &registers).is_none());
        assert!(
            stepper
                .validate_syscall(123, 4, number, &registers)
                .is_some()
        );
        for (owner, frame, number) in [(124, 4, number), (123, 5, number), (123, 4, libc::SYS_read)]
        {
            assert!(
                stepper
                    .validate_syscall(owner, frame, number, &registers)
                    .is_none()
            );
        }
        for field in [
            libc::REG_RIP,
            libc::REG_RCX,
            libc::REG_R11,
            libc::REG_EFL,
            libc::REG_RDI,
        ] {
            let mut invalid = registers;
            invalid[field as usize] ^= 1;
            assert!(stepper.validate_syscall(123, 4, number, &invalid).is_none());
        }
        assert!(stepper.cancel_syscall(41).is_none());
        assert!(stepper.pending());
        stepper.cancel_syscall(40).unwrap();
        assert_eq!(stepper.completed, 0);
        assert!(stepper.cancel_syscall(40).is_none());
        let (generation, _) = controller.replace(40, 0, 1).unwrap();
        stepper
            .stage_precise(
                (123, 5),
                crate::timer::Ticket {
                    generation,
                    sequence: 1,
                },
                Instruction::Nop,
                &context,
                40,
            )
            .unwrap();
        assert!(
            stepper
                .validate_syscall(123, 4, number, &registers)
                .is_none()
        );
        stepper.cancel();
        assert!(!stepper.pending());
    }

    #[test]
    fn completion_requires_exact_owner_frame_pc_flags_and_clock_once() {
        let mut stepper = Stepper::default();
        stepper.stage_nop(123, 4, 0x4000, 0x10202, 40).unwrap();
        let registers = registers(0x4001);
        assert!(stepper.validate(124, 4, &registers).is_none());
        assert!(stepper.validate(123, 5, &registers).is_none());
        for field in [
            libc::REG_RIP,
            libc::REG_EFL,
            libc::REG_TRAPNO,
            libc::REG_ERR,
        ] {
            let mut invalid = registers;
            invalid[field as usize] ^= 1;
            assert!(stepper.validate(123, 4, &invalid).is_none());
        }
        let completion = stepper.validate(123, 4, &registers).unwrap();
        assert!(stepper.complete(completion, 41).is_none());
        stepper.complete(completion, 40).unwrap();
        assert!(stepper.complete(completion, 40).is_none());
        assert_eq!(stepper.completed, 1);
    }

    #[test]
    fn cancelled_completion_cannot_advance_replacement() {
        let mut stepper = Stepper::default();
        stepper.stage_nop(123, 4, 0x4000, 0x202, 40).unwrap();
        let old = stepper.validate(123, 4, &registers(0x4001)).unwrap();
        stepper.cancel();
        stepper.stage_nop(123, 6, 0x4000, 0x202, 40).unwrap();
        assert!(stepper.complete(old, 40).is_none());
        let current = stepper.validate(123, 6, &registers(0x4001)).unwrap();
        stepper.complete(current, 40).unwrap();
        assert_eq!(stepper.completed, 1);
    }

    #[test]
    fn unsupported_initial_tf_or_reentrant_arm_refuses() {
        let mut stepper = Stepper::default();
        assert!(stepper.stage_nop(123, 4, 0x4000, 0x302, 40).is_none());
        stepper.stage_nop(123, 4, 0x4000, 0x202, 40).unwrap();
        assert!(stepper.stage_nop(123, 4, 0x4000, 0x202, 40).is_none());
        assert_eq!(stepper.completed, 0);
    }
}
