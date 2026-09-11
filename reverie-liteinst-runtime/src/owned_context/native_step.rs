//! Preallocated correctness state is independent of optional diagnostic history.

use std::io;
use std::ops::Range;

use liteinst2::trampoline::HookContext;
use reverie_preload::signal::native_frame::RelocatedFrame;

const PREFIX: usize = 440;
const FP: usize = 2444;
const FP_POINTER: usize = 232;
const FLAGS: usize = 184;

pub(super) struct NativeState {
    pub seal: Seal,
    pub history: Option<History>,
    pub fault_resume: Option<u64>,
}

impl NativeState {
    pub fn new(evidence_bytes: usize) -> io::Result<Self> {
        Ok(Self {
            seal: Seal::default(),
            history: if evidence_bytes == 0 {
                None
            } else {
                Some(History::new(evidence_bytes)?)
            },
            fault_resume: None,
        })
    }
}

pub(super) struct History {
    bytes: Box<[u8]>,
    used: usize,
    records: u64,
    failed: bool,
}

impl History {
    pub fn new(capacity: usize) -> io::Result<Self> {
        if !(64..=64 * 1024 * 1024).contains(&capacity) {
            return Err(io::ErrorKind::InvalidInput.into());
        }
        let mut bytes = Vec::new();
        bytes
            .try_reserve_exact(capacity)
            .map_err(io::Error::other)?;
        bytes.resize(capacity, 0);
        Ok(Self {
            bytes: bytes.into_boxed_slice(),
            used: 0,
            records: 0,
            failed: false,
        })
    }

    pub fn append(&mut self, header: [u64; 8], image: &[u8]) -> Option<()> {
        if self.failed {
            return None;
        }
        let Some(end) = self
            .used
            .checked_add(64)
            .and_then(|end| end.checked_add(image.len()))
            .filter(|end| *end <= self.bytes.len())
        else {
            self.failed = true;
            return None;
        };
        let Some(records) = self.records.checked_add(1) else {
            self.failed = true;
            return None;
        };
        for (field, value) in header.into_iter().enumerate() {
            self.bytes[self.used + field * 8..self.used + field * 8 + 8]
                .copy_from_slice(&value.to_le_bytes());
        }
        self.bytes[self.used + 64..end].copy_from_slice(image);
        self.used = end;
        self.records = records;
        Some(())
    }

    pub fn bytes(&self) -> &[u8] {
        &self.bytes[..self.used]
    }
}

pub(super) struct Seal {
    prefix: [u8; PREFIX],
    fp: [u8; FP],
    generation: u64,
    live: bool,
    verified: bool,
}

impl Default for Seal {
    fn default() -> Self {
        Self {
            prefix: [0; PREFIX],
            fp: [0; FP],
            generation: 0,
            live: false,
            verified: false,
        }
    }
}

impl Seal {
    pub fn complete_modeled<'frame>(
        &mut self,
        image: RelocatedFrame<'frame>,
        expected: reverie_preload::signal::native_frame::ModeledReadContext,
        operation: reverie_preload::signal::native_frame::ModeledReadOperation,
        value: reverie_preload::signal::native_frame::ModeledReadValue,
        generation: u64,
    ) -> Result<RelocatedFrame<'frame>, reverie_preload::signal::native_frame::Error> {
        use reverie_preload::signal::native_frame::Error;
        if !self.matches(image.frame_bytes(), image.fp_bytes(), generation, false) {
            return Err(Error::Metadata);
        }
        let image = image.complete_owned_modeled_read(expected, operation, value)?;
        if image.fp_bytes() != self.fp {
            return Err(Error::Metadata);
        }
        self.prefix.copy_from_slice(image.frame_bytes());
        Ok(image)
    }

    pub fn retire_verified(&mut self) -> Option<()> {
        if self.live && !self.verified {
            return None;
        }
        self.live = false;
        Some(())
    }

    pub fn capture(&mut self, source: &[u8], address: usize, generation: u64) -> Option<()> {
        if self.live || generation <= self.generation {
            return None;
        }
        let prefix = source.get(..PREFIX)?;
        let pointer =
            u64::from_le_bytes(prefix[FP_POINTER..FP_POINTER + 8].try_into().ok()?) as usize;
        let offset = pointer.checked_sub(address)?;
        let fp = source.get(offset..offset.checked_add(FP)?)?;
        self.prefix.copy_from_slice(prefix);
        self.fp.copy_from_slice(fp);
        self.generation = generation;
        self.live = true;
        self.verified = false;
        Some(())
    }

    fn matches(&self, frame: &[u8], fp: &[u8], generation: u64, tf: bool) -> bool {
        if !self.live
            || self.verified
            || generation != self.generation
            || frame.len() != PREFIX
            || fp != self.fp
        {
            return false;
        }
        let pointer = (fp.as_ptr() as u64).to_le_bytes();
        let flags = u64::from_le_bytes(self.prefix[FLAGS..FLAGS + 8].try_into().unwrap());
        let expected_flags = ((flags & !0x100) | if tf { 0x100 } else { 0 }).to_le_bytes();
        frame.iter().enumerate().all(|(offset, byte)| {
            *byte
                == if (FP_POINTER..FP_POINTER + 8).contains(&offset) {
                    pointer[offset - FP_POINTER]
                } else if (FLAGS..FLAGS + 8).contains(&offset) {
                    expected_flags[offset - FLAGS]
                } else {
                    self.prefix[offset]
                }
        })
    }

    pub fn relocated(&self, image: &RelocatedFrame<'_>, generation: u64) -> bool {
        self.matches(image.frame_bytes(), image.fp_bytes(), generation, true)
    }

    pub fn finish(
        &mut self,
        image: &RelocatedFrame<'_>,
        context: &HookContext,
        generation: u64,
        tf: bool,
    ) -> Option<()> {
        if !self.live {
            return Some(());
        }
        if !self.matches(image.frame_bytes(), image.fp_bytes(), generation, tf) {
            return None;
        }
        let general = [
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
            context.instruction_pointer,
            context.rflags,
        ];
        for (index, actual) in general.into_iter().enumerate() {
            let offset = 48 + index * 8;
            let expected = u64::from_le_bytes(self.prefix[offset..offset + 8].try_into().ok()?);
            if actual
                != if index == 17 {
                    expected & !0x100
                } else {
                    expected
                }
            {
                return None;
            }
        }
        self.verified = true;
        Some(())
    }
}

pub(super) fn fetch_with(
    ranges: &[Range<u64>],
    pc: u64,
    mut read: impl FnMut(u64) -> u8,
) -> Option<([u8; 15], usize)> {
    if pc == 0 || pc >= 1 << 47 {
        return None;
    }
    let mut bytes = [0; 15];
    let mut length = 0;
    for byte in &mut bytes {
        let address = pc.checked_add(length as u64)?;
        if address >= 1 << 47 || !ranges.iter().any(|range| range.contains(&address)) {
            break;
        }
        *byte = read(address);
        length += 1;
    }
    (length != 0).then_some((bytes, length))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[repr(align(64))]
    struct Buffer([u8; 4096]);

    fn put(bytes: &mut [u8], offset: usize, value: u64) {
        bytes[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
    }

    fn fixture(source: &mut Buffer) {
        let pointer = source.0.as_ptr() as u64;
        put(&mut source.0, 8, 0x7000);
        put(&mut source.0, 16, 7);
        put(&mut source.0, 8 + 176, 0x4001);
        put(&mut source.0, 8 + 184, 0x302);
        put(&mut source.0, 8 + 232, pointer + 512);
        source.0[512 + 464..512 + 468].copy_from_slice(&0x46505853u32.to_le_bytes());
        source.0[512 + 468..512 + 472].copy_from_slice(&(FP as u32).to_le_bytes());
        put(&mut source.0, 512 + 472, 0x2e7);
        source.0[512 + 480..512 + 484].copy_from_slice(&2440u32.to_le_bytes());
        put(&mut source.0, 512 + 512, 0x2e7);
        source.0[512 + 2440..512 + 2444].copy_from_slice(&0x46505845u32.to_le_bytes());
    }

    #[test]
    fn owned_emulation_successor_matches_frame_context_and_rep_requests() {
        use reverie_preload::signal::native_frame::Format;
        use reverie_preload::signal::native_frame::InstructionResult;
        use reverie_preload::signal::native_frame::relocate;

        use crate::instruction_event::InstructionEvent;
        use crate::instruction_event::Kind;
        use crate::instruction_event::apply_result;
        use crate::owned_step::Instruction;
        use crate::owned_step::Stepper;
        use crate::owned_step::native;
        for kind in [Kind::Cpuid, Kind::Rdtsc, Kind::Rdtscp] {
            for count in [0, 1, 3] {
                for flags in [0x10202, 0x10ed7] {
                    let mut context: HookContext = unsafe { core::mem::zeroed() };
                    context.instruction_pointer = 0x4000;
                    context.rcx = count;
                    context.rdi = 0x20000;
                    context.rflags = flags;
                    let event = InstructionEvent::decode(0x4000, kind.bytes()).unwrap();
                    let result = match kind {
                        Kind::Cpuid => InstructionResult::Cpuid(reverie::CpuIdResult {
                            eax: u32::MAX,
                            ebx: 0x80000000,
                            ecx: count as u32,
                            edx: 0x12345678,
                        }),
                        _ => InstructionResult::Rdtsc {
                            request: if kind == Kind::Rdtsc {
                                reverie::Rdtsc::Tsc
                            } else {
                                reverie::Rdtsc::Tscp
                            },
                            result: reverie::RdtscResult {
                                tsc: 0x8000000112345678,
                                aux: Some(count as u32),
                            },
                        },
                    };
                    let mut source = Buffer([0; 4096]);
                    fixture(&mut source);
                    for (field, value) in native::general(&context).into_iter().enumerate() {
                        put(&mut source.0, 8 + 48 + field * 8, value);
                    }
                    put(&mut source.0, 8 + 176, 0x4000);
                    put(&mut source.0, 8 + 184, flags);
                    let original = source.0;
                    let address = source.0.as_ptr() as usize + 8;
                    let mut destination = Buffer([0; 4096]);
                    let image = relocate(
                        &source.0,
                        address,
                        address + 8,
                        Format::StandardXsave {
                            xfeatures: 0x2e7,
                            xstate_size: 2440,
                        },
                        &mut destination.0,
                    )
                    .unwrap();
                    let fp = image.fp_bytes().to_vec();
                    apply_result(&mut context, result);
                    let outputs = native::general(&context);
                    let image = crate::owned_context::complete_owned_emulation(
                        image,
                        &mut context,
                        event,
                        result,
                    )
                    .unwrap();
                    assert_eq!(context.rflags, flags & !0x10000);
                    assert_eq!(context.instruction_pointer, event.resume_pc);
                    assert_eq!(native::general(&context), outputs);
                    for (field, value) in outputs.into_iter().enumerate() {
                        assert_eq!(
                            u64::from_le_bytes(
                                image.frame_bytes()[48 + field * 8..56 + field * 8]
                                    .try_into()
                                    .unwrap()
                            ),
                            value
                        );
                    }
                    assert_eq!(
                        u64::from_le_bytes(image.frame_bytes()[184..192].try_into().unwrap()),
                        context.rflags
                    );
                    assert_eq!(
                        u64::from_le_bytes(image.frame_bytes()[176..184].try_into().unwrap()),
                        context.instruction_pointer
                    );
                    assert_eq!(image.fp_bytes(), fp);
                    assert_eq!(source.0, original);
                    let mut controller = reverie_preload::precise_timer::Controller::default();
                    let (generation, _) = controller.replace(40, 0, count.max(1)).unwrap();
                    let mut stepper = Stepper::default();
                    for index in 0..count.max(1) {
                        let request = native::CpuStepRequest::decode_recorded(
                            event.resume_pc,
                            &[0xf3, 0x48, 0xab],
                            &context,
                            false,
                            &mut native::Refusal::new("test"),
                        )
                        .unwrap();
                        stepper
                            .stage_precise(
                                (123, index + 1),
                                crate::timer::Ticket {
                                    generation,
                                    sequence: index + 1,
                                },
                                Instruction::Native(Box::new(request)),
                                &context,
                                40,
                            )
                            .unwrap();
                        let final_stop = index + 1 == count.max(1);
                        let mut registers = [0i64; 23];
                        registers[..16]
                            .copy_from_slice(&native::general(&context).map(|value| value as i64));
                        registers[libc::REG_RIP as usize] =
                            (event.resume_pc + if final_stop { 3 } else { 0 }) as i64;
                        registers[libc::REG_RCX as usize] = count.saturating_sub(index + 1) as i64;
                        registers[libc::REG_EFL as usize] = (context.rflags | 0x100) as i64;
                        registers[libc::REG_TRAPNO as usize] = 1;
                        let mut invalid = registers;
                        invalid[libc::REG_EFL as usize] |= 0x10000;
                        assert!(stepper.capture(123, index + 1, &invalid, 40).is_none());
                        let completion = stepper.capture(123, index + 1, &registers, 40).unwrap();
                        let observation = stepper.complete_precise(completion, 40).unwrap();
                        assert_eq!(
                            controller.observe(observation).unwrap(),
                            if final_stop {
                                reverie_preload::precise_timer::Decision::Deliver
                            } else {
                                reverie_preload::precise_timer::Decision::Step
                            }
                        );
                        context.rcx = registers[libc::REG_RCX as usize] as u64;
                    }
                }
            }
        }
    }

    #[test]
    fn owned_emulation_rejects_unbound_flags_tf_kind_or_pc_without_context_mutation() {
        use reverie_preload::signal::native_frame::Error;
        use reverie_preload::signal::native_frame::Format;
        use reverie_preload::signal::native_frame::InstructionResult;
        use reverie_preload::signal::native_frame::relocate;

        use crate::instruction_event::InstructionEvent;
        use crate::instruction_event::Kind;
        for mode in 0..6 {
            let flags = if mode == 1 { 0x10302 } else { 0x10202 };
            let mut context: HookContext = unsafe { core::mem::zeroed() };
            context.instruction_pointer = 0x4000;
            context.rflags = flags;
            context.rax = 0x1234;
            let mut source = Buffer([0; 4096]);
            fixture(&mut source);
            put(&mut source.0, 8 + 176, 0x4000);
            put(&mut source.0, 8 + 184, flags);
            let original = source.0;
            let address = source.0.as_ptr() as usize + 8;
            let mut destination = Buffer([0; 4096]);
            let image = relocate(
                &source.0,
                address,
                address + 8,
                Format::StandardXsave {
                    xfeatures: 0x2e7,
                    xstate_size: 2440,
                },
                &mut destination.0,
            )
            .unwrap();
            let mut event = InstructionEvent::decode(0x4000, Kind::Cpuid.bytes()).unwrap();
            match mode {
                0 => context.rflags ^= 1,
                1 => {}
                2 => event.kind = Kind::Rdtsc,
                3 => context.rflags ^= 0x10000,
                4 => context.instruction_pointer += 1,
                5 => event.resume_pc += 1,
                _ => unreachable!(),
            }
            let before = context;
            let result = InstructionResult::Cpuid(reverie::CpuIdResult {
                eax: 1,
                ebx: 2,
                ecx: 3,
                edx: 4,
            });
            let error =
                crate::owned_context::complete_owned_emulation(image, &mut context, event, result)
                    .unwrap_err();
            assert_eq!(
                error,
                if matches!(mode, 0 | 1 | 3) {
                    Error::Flags
                } else {
                    Error::InstructionPointer
                }
            );
            assert_eq!(
                crate::owned_step::native::general(&context),
                crate::owned_step::native::general(&before)
            );
            assert_eq!(context.rflags, before.rflags);
            assert_eq!(context.instruction_pointer, before.instruction_pointer);
            assert_eq!(source.0, original);
        }
    }

    #[test]
    fn every_postimage_byte_including_written_registers_and_trailer_is_sealed() {
        let mut source = Buffer([0; 4096]);
        fixture(&mut source);
        let address = source.0.as_ptr() as usize + 8;
        let mut seal = Seal::default();
        seal.capture(&source.0[8..], address, 1).unwrap();
        let mut frame = seal.prefix;
        let fp = seal.fp;
        put(&mut frame, FP_POINTER, fp.as_ptr() as u64);
        assert!(seal.matches(&frame, &fp, 1, true));
        for offset in 0..PREFIX {
            frame[offset] ^= 1;
            assert!(!seal.matches(&frame, &fp, 1, true), "frame byte {offset}");
            frame[offset] ^= 1;
        }
        let mut fp = fp;
        put(&mut frame, FP_POINTER, fp.as_ptr() as u64);
        for offset in 0..FP {
            fp[offset] ^= 1;
            assert!(
                !seal.matches(&frame, &fp, 1, true),
                "FP/trailer byte {offset}"
            );
            fp[offset] ^= 1;
        }
        assert!(seal.retire_verified().is_none());
        assert!(seal.capture(&source.0[8..], address, 2).is_none());
    }

    #[test]
    fn verified_steps_cross_old_cumulative_archive_without_reusing_unverified_state() {
        use reverie_preload::precise_timer::Controller;
        use reverie_preload::precise_timer::Decision;
        use reverie_preload::signal::native_frame::Format;
        use reverie_preload::signal::native_frame::relocate;

        use crate::owned_step::Instruction;
        use crate::owned_step::Stepper;
        let mut native = NativeState::new(64 * (4096 + 64)).unwrap();
        let mut source = Buffer([0; 4096]);
        fixture(&mut source);
        let address = source.0.as_ptr() as usize + 8;
        let mut destination = Buffer([0; 4096]);
        let clock = 40;
        let mut controller = Controller::default();
        let (generation, _) = controller.replace(clock, 0, 64).unwrap();
        let mut stepper = Stepper::default();
        for sequence in 1..=64 {
            native.seal.retire_verified().unwrap();
            let pc = 0x4000 + (sequence - 1) * 3;
            let mut context: HookContext = unsafe { core::mem::zeroed() };
            context.rax = sequence - 1;
            context.rcx = sequence;
            context.instruction_pointer = pc;
            context.rflags = 0x202;
            let request = crate::owned_step::native::CpuStepRequest::decode_recorded(
                pc,
                &[0x48, 0x89, 0xc8],
                &context,
                false,
                &mut crate::owned_step::native::Refusal::new("test"),
            )
            .unwrap();
            stepper
                .stage_precise(
                    (123, sequence),
                    crate::timer::Ticket {
                        generation,
                        sequence,
                    },
                    Instruction::Native(Box::new(request)),
                    &context,
                    clock,
                )
                .unwrap();
            put(&mut source.0, 8 + 152, sequence);
            put(&mut source.0, 8 + 160, sequence);
            put(&mut source.0, 8 + 176, pc + 3);
            put(&mut source.0, 8 + 48 + libc::REG_TRAPNO as usize * 8, 1);
            let registers: [i64; 23] = core::array::from_fn(|field| {
                i64::from_le_bytes(
                    source.0[8 + 48 + field * 8..8 + 56 + field * 8]
                        .try_into()
                        .unwrap(),
                )
            });
            let completion = stepper.capture(123, sequence, &registers, clock).unwrap();
            assert!(stepper.capture(123, sequence, &registers, clock).is_none());
            let observation = stepper.complete_precise(completion, clock).unwrap();
            assert_eq!(observation.sequence, sequence);
            assert_eq!(
                controller.observe(observation).unwrap(),
                if sequence == 64 {
                    Decision::Deliver
                } else {
                    Decision::Step
                }
            );
            assert!(stepper.complete_precise(completion, clock).is_none());
            native
                .seal
                .capture(&source.0[8..], address, sequence)
                .unwrap();
            native
                .history
                .as_mut()
                .unwrap()
                .append([sequence, clock, 0, 0, 0, 0, 0, 0], &source.0)
                .unwrap();
            let image = relocate(
                &source.0,
                address,
                address + 8,
                Format::StandardXsave {
                    xfeatures: 0x2e7,
                    xstate_size: 2440,
                },
                &mut destination.0,
            )
            .unwrap();
            assert!(native.seal.relocated(&image, sequence));
            let image = image.owned_single_step(pc + 3, 0x302, false).unwrap();
            context.rax = sequence;
            context.instruction_pointer = pc + 3;
            context.rflags = 0x202;
            native
                .seal
                .finish(&image, &context, sequence, false)
                .unwrap();
            assert!(
                native
                    .seal
                    .finish(&image, &context, sequence, false)
                    .is_none()
            );
        }
        assert_eq!(clock, 40);
        assert_eq!(stepper.completed, 64);
        let history = native.history.unwrap();
        assert_eq!(history.records, 64);
        assert_eq!(history.bytes().len(), 64 * (4096 + 64));
        assert!(history.bytes().len() > 133120);
        assert!(!history.failed);
        assert!(NativeState::new(0).unwrap().history.is_none());
    }

    #[test]
    fn fetch_reads_only_admitted_bytes_and_joins_adjacent_ranges() {
        for (ranges, expected) in [
            (vec![0x1000..0x1001], 1),
            (vec![0x1000..0x1001, 0x1002..0x1003], 1),
            (vec![0x1000..0x1001, 0x1001..0x100f], 15),
        ] {
            let mut observed = Vec::new();
            let (_, length) = fetch_with(&ranges, 0x1000, |address| {
                observed.push(address);
                0x90
            })
            .unwrap();
            assert_eq!(length, expected);
            assert_eq!(
                observed,
                (0x1000..0x1000 + expected as u64).collect::<Vec<_>>()
            );
        }
        assert!(fetch_with(&[0..u64::MAX], u64::MAX, |_| panic!("unadmitted read")).is_none());
    }

    #[test]
    fn insufficient_history_is_terminal_and_preserves_the_prefix() {
        let mut history = History::new(80).unwrap();
        history.append([1; 8], &[2; 16]).unwrap();
        let prefix = history.bytes().to_vec();
        assert!(history.append([3; 8], &[4]).is_none());
        assert!(history.failed);
        assert!(history.append([5; 8], &[]).is_none());
        assert_eq!(history.bytes(), prefix);
        assert_eq!(history.records, 1);
    }
}
