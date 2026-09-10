use super::*;

const FRAME: usize = 8;
const FLOATING: usize = 768;
const STANDARD: Format = Format::StandardXsave {
    xfeatures: 7,
    xstate_size: 832,
};
const MODELED_PC: u64 = 0x10be;

fn modeled_context(flags: u64) -> ModeledReadContext {
    ModeledReadContext {
        pc: MODELED_PC,
        general: [
            0x0808_0808_0808_0808,
            0x0909_0909_0909_0909,
            0x1010_1010_1010_1010,
            0x1111_1111_1111_1111,
            0x1212_1212_1212_1212,
            0x1313_1313_1313_1313,
            0x1414_1414_1414_1414,
            0x1515_1515_1515_1515,
            0xd1d1_d1d1_d1d1_d1d1,
            0x5151_5151_5151_5151,
            0xbaba_baba_baba_baba,
            0xbbbb_bbbb_bbbb_bbbb,
            0xd2d2_d2d2_d2d2_d2d2,
            0xaaaa_aaaa_aaaa_aaaa,
            0xcccc_cccc_cccc_cccc,
            0x5a5a_5a5a_5a5a_5a5a,
        ],
        flags,
    }
}

fn modeled_image<'destination>(
    actual: ModeledReadContext,
    destination: &'destination mut Storage,
) -> RelocatedFrame<'destination> {
    let mut source = fixture(FRAME, FLOATING, STANDARD);
    for (index, register) in actual.general.into_iter().enumerate() {
        put_u64(&mut source.0, FRAME + GENERAL_OFFSET + index * 8, register);
    }
    put_u64(&mut source.0, FRAME + 176, actual.pc);
    put_u64(&mut source.0, FRAME + 184, actual.flags);
    let address = source.0.as_ptr() as usize + FRAME;
    relocate(
        &source.0,
        address,
        address + 8,
        STANDARD,
        &mut destination.0,
    )
    .unwrap()
}

fn assert_modeled_refusal(
    actual: ModeledReadContext,
    expected: ModeledReadContext,
    operation: ModeledReadOperation,
    value: ModeledReadValue,
    error: Error,
) {
    let mut destination = Storage([0xa7; 2048]);
    let image = modeled_image(actual, &mut destination);
    let before = image.bytes.to_vec();
    assert_eq!(
        image
            .complete_owned_modeled_read(expected, operation, value)
            .unwrap_err(),
        error
    );
    assert_eq!(&destination.0[..before.len()], before);
}

#[test]
fn vdso_getrandom_modeled_ready_compare_covers_every_byte_and_uses_byte_sign() {
    for initial_arithmetic in [0, 0x8d5] {
        for rf in [0, 0x10000] {
            for ready in 0..=u8::MAX {
                let context = modeled_context(0xa5a4_0202 | initial_arithmetic | rf);
                assert_eq!(context.flags & 0x10000, rf);
                let mut destination = Storage([0xa7; 2048]);
                let image = modeled_image(context, &mut destination);
                let mut expected = image.bytes.to_vec();
                let affected = (u64::from(ready.count_ones().is_multiple_of(2)) << 2)
                    | (u64::from(ready == 0) << 6)
                    | (u64::from(ready & 0x80 != 0) << 7);
                put_u64(
                    &mut expected,
                    DEST_FRAME + 176,
                    context.pc + MODELED_READ_LENGTH,
                );
                put_u64(
                    &mut expected,
                    DEST_FRAME + 184,
                    (context.flags & !(0x8d5 | 0x10000)) | affected,
                );
                let image = image
                    .complete_owned_modeled_read(
                        context,
                        ModeledReadOperation::CompareReadyWithZero,
                        ModeledReadValue::Ready(ready),
                    )
                    .unwrap();
                assert_eq!(image.bytes, expected);
                if ready == 0x80 {
                    assert_ne!(read_u64(image.frame_bytes(), 184).unwrap() & 0x80, 0);
                    assert_eq!(read_u64(image.frame_bytes(), 184).unwrap() & 0x800, 0);
                }
            }
        }
    }
}

#[test]
fn vdso_getrandom_modeled_generation_compare_matches_independent_qword_boundary_table() {
    const CASES: &[(u64, u64, u64)] = &[
        (0, 0, 0x044),
        (1, 0, 0x000),
        (4, 1, 0x004),
        (0, 1, 0x095),
        (0x10, 1, 0x014),
        (0x11, 1, 0x000),
        (0x7fff_ffff_ffff_ffff, u64::MAX, 0x885),
        (0x8000_0000_0000_0000, 1, 0x814),
        (u64::MAX, 0, 0x084),
        (u64::MAX, u64::MAX, 0x044),
    ];

    for initial_arithmetic in [0, 0x8d5] {
        for rf in [0, 0x10000] {
            for &(rax, generation, affected) in CASES {
                let mut context = modeled_context(0x5a5a_1202 | initial_arithmetic | rf);
                assert_eq!(context.flags & 0x10000, rf);
                context.general[13] = rax;
                let mut destination = Storage([0xa7; 2048]);
                let image = modeled_image(context, &mut destination);
                let mut expected = image.bytes.to_vec();
                put_u64(
                    &mut expected,
                    DEST_FRAME + 176,
                    context.pc + MODELED_READ_LENGTH,
                );
                put_u64(
                    &mut expected,
                    DEST_FRAME + 184,
                    (context.flags & !(0x8d5 | 0x10000)) | affected,
                );
                let image = image
                    .complete_owned_modeled_read(
                        context,
                        ModeledReadOperation::CompareRaxWithGeneration,
                        ModeledReadValue::Generation(generation),
                    )
                    .unwrap();
                assert_eq!(image.bytes, expected);
            }
        }
    }
}

#[test]
fn vdso_getrandom_modeled_generation_load_writes_full_rcx_and_preserves_every_other_byte() {
    for rf in [0, 0x10000] {
        for generation in [0, 0x1_0000_0000, 0xfedc_ba98_7654_3210, u64::MAX] {
            let context = modeled_context(0x246 | rf);
            let mut destination = Storage([0xa7; 2048]);
            let image = modeled_image(context, &mut destination);
            let mut expected = image.bytes.to_vec();
            put_u64(&mut expected, DEST_FRAME + 160, generation);
            put_u64(
                &mut expected,
                DEST_FRAME + 176,
                context.pc + MODELED_READ_LENGTH,
            );
            put_u64(&mut expected, DEST_FRAME + 184, context.flags & !0x10000);
            let image = image
                .complete_owned_modeled_read(
                    context,
                    ModeledReadOperation::LoadGenerationToRcx,
                    ModeledReadValue::Generation(generation),
                )
                .unwrap();
            assert_eq!(image.bytes, expected);
            assert_eq!(read_u64(image.frame_bytes(), 160).unwrap(), generation);
        }
    }
}

#[test]
fn vdso_getrandom_modeled_read_wrong_value_variants_never_mutate() {
    let context = modeled_context(0x10202);
    for (operation, value) in [
        (
            ModeledReadOperation::CompareReadyWithZero,
            ModeledReadValue::Generation(1),
        ),
        (
            ModeledReadOperation::LoadGenerationToRcx,
            ModeledReadValue::Ready(1),
        ),
        (
            ModeledReadOperation::CompareRaxWithGeneration,
            ModeledReadValue::Ready(1),
        ),
    ] {
        assert_modeled_refusal(context, context, operation, value, Error::Metadata);
    }
}

#[test]
fn vdso_getrandom_modeled_read_binds_pc_every_gpr_full_flags_and_disarmed_tf() {
    let expected = modeled_context(0x10202);

    let mut actual = expected;
    actual.pc += 1;
    assert_modeled_refusal(
        actual,
        expected,
        ModeledReadOperation::CompareReadyWithZero,
        ModeledReadValue::Ready(1),
        Error::InstructionPointer,
    );
    for index in 0..expected.general.len() {
        let mut actual = expected;
        actual.general[index] ^= 1;
        assert_modeled_refusal(
            actual,
            expected,
            ModeledReadOperation::CompareReadyWithZero,
            ModeledReadValue::Ready(1),
            Error::Metadata,
        );
    }
    let mut actual = expected;
    actual.flags ^= 1;
    assert_modeled_refusal(
        actual,
        expected,
        ModeledReadOperation::CompareReadyWithZero,
        ModeledReadValue::Ready(1),
        Error::Flags,
    );
    let tf = modeled_context(0x10302);
    assert_modeled_refusal(
        tf,
        tf,
        ModeledReadOperation::CompareReadyWithZero,
        ModeledReadValue::Ready(1),
        Error::Flags,
    );
}

#[test]
fn vdso_getrandom_modeled_read_refuses_bad_pc_or_successor_without_mutation() {
    for pc in [0, (1 << 47) - MODELED_READ_LENGTH, 1 << 47, u64::MAX] {
        let mut context = modeled_context(0x202);
        context.pc = pc;
        assert_modeled_refusal(
            context,
            context,
            ModeledReadOperation::LoadGenerationToRcx,
            ModeledReadValue::Generation(1),
            Error::InstructionPointer,
        );
    }
}

#[test]
fn vdso_getrandom_modeled_read_repeated_completion_rejects_stale_original_pc() {
    let context = modeled_context(0x10202);
    let mut destination = Storage([0xa7; 2048]);
    let image = modeled_image(context, &mut destination)
        .complete_owned_modeled_read(
            context,
            ModeledReadOperation::LoadGenerationToRcx,
            ModeledReadValue::Generation(0xfedc_ba98_7654_3210),
        )
        .unwrap();
    let completed = image.bytes.to_vec();
    assert_eq!(
        image
            .complete_owned_modeled_read(
                context,
                ModeledReadOperation::LoadGenerationToRcx,
                ModeledReadValue::Generation(0xfedc_ba98_7654_3210),
            )
            .unwrap_err(),
        Error::InstructionPointer
    );
    assert_eq!(&destination.0[..completed.len()], completed);
}

#[cfg(feature = "coordinator-rpc")]
#[test]
fn owned_instruction_retires_only_rf_with_exact_typed_outputs() {
    for value in [0, 0x8000_0001_1234_5678, u64::MAX] {
        for result in [
            InstructionResult::Cpuid(reverie::CpuIdResult {
                eax: value as u32,
                ebx: u32::MAX,
                ecx: 3,
                edx: (value >> 32) as u32,
            }),
            InstructionResult::Rdtsc {
                request: reverie::Rdtsc::Tsc,
                result: reverie::RdtscResult {
                    tsc: value,
                    aux: Some(u32::MAX),
                },
            },
            InstructionResult::Rdtsc {
                request: reverie::Rdtsc::Tscp,
                result: reverie::RdtscResult {
                    tsc: value,
                    aux: None,
                },
            },
            InstructionResult::Rdtsc {
                request: reverie::Rdtsc::Tscp,
                result: reverie::RdtscResult {
                    tsc: value,
                    aux: Some(u32::MAX),
                },
            },
        ] {
            for flags in [0x10202, 0x10ed7, 0x202] {
                let mut source = fixture(FRAME, FLOATING, STANDARD);
                put_u64(&mut source.0, FRAME + 176, 0x4000);
                put_u64(&mut source.0, FRAME + 184, flags);
                let original = source.0;
                let address = source.0.as_ptr() as usize + FRAME;
                let mut destination = Storage([0xa7; 2048]);
                let image = relocate(
                    &source.0,
                    address,
                    address + 8,
                    STANDARD,
                    &mut destination.0,
                )
                .unwrap();
                let mut expected = image.bytes.to_vec();
                let length = match result {
                    InstructionResult::Cpuid(result) => {
                        for (offset, value) in [
                            (152, result.eax),
                            (136, result.ebx),
                            (160, result.ecx),
                            (144, result.edx),
                        ] {
                            put_u64(&mut expected, DEST_FRAME + offset, u64::from(value));
                        }
                        2
                    }
                    InstructionResult::Rdtsc { request, result } => {
                        put_u64(
                            &mut expected,
                            DEST_FRAME + 152,
                            u64::from(result.tsc as u32),
                        );
                        put_u64(&mut expected, DEST_FRAME + 144, result.tsc >> 32);
                        if request == reverie::Rdtsc::Tscp {
                            put_u64(
                                &mut expected,
                                DEST_FRAME + 160,
                                u64::from(result.aux.unwrap_or(0)),
                            );
                            3
                        } else {
                            2
                        }
                    }
                };
                put_u64(&mut expected, DEST_FRAME + 176, 0x4000 + length);
                put_u64(&mut expected, DEST_FRAME + 184, flags & !0x10000);
                let image = image
                    .complete_owned_instruction(0x4000, flags, result)
                    .unwrap();
                assert_eq!(image.bytes, expected);
                assert_eq!(source.0, original);
            }
        }
    }
}

#[cfg(feature = "coordinator-rpc")]
#[test]
fn owned_instruction_rejects_wrong_flags_tf_or_pc_without_mutation() {
    for mode in 0..4 {
        let flags = if mode == 1 { 0x10302 } else { 0x10202 };
        let mut source = fixture(FRAME, FLOATING, STANDARD);
        put_u64(&mut source.0, FRAME + 176, 0x4000);
        put_u64(&mut source.0, FRAME + 184, flags);
        let address = source.0.as_ptr() as usize + FRAME;
        let mut destination = Storage([0xa7; 2048]);
        let image = relocate(
            &source.0,
            address,
            address + 8,
            STANDARD,
            &mut destination.0,
        )
        .unwrap();
        let before = image.bytes.to_vec();
        let expected_flags = flags
            ^ match mode {
                0 => 1,
                3 => 0x10000,
                _ => 0,
            };
        let result = InstructionResult::Cpuid(reverie::CpuIdResult {
            eax: 1,
            ebx: 2,
            ecx: 3,
            edx: 4,
        });
        let error = image
            .complete_owned_instruction(
                if mode == 2 { 0x4001 } else { 0x4000 },
                expected_flags,
                result,
            )
            .unwrap_err();
        assert_eq!(
            error,
            if mode == 2 {
                Error::InstructionPointer
            } else {
                Error::Flags
            }
        );
        assert_eq!(&destination.0[..before.len()], before);
    }
}

#[test]
fn vdso_return_preserves_every_other_register_and_fp_byte() {
    for owned_tf in [false, true] {
        let flags = 0x10202 | if owned_tf { 0x100 } else { 0 };
        let mut source = fixture(FRAME, FLOATING, STANDARD);
        for (offset, value) in [(176, 0x10800), (168, 0x20008), (184, flags)] {
            put_u64(&mut source.0, FRAME + offset, value);
        }
        let address = source.0.as_ptr() as usize + FRAME;
        let mut destination = Storage([0xa7; 2048]);
        let image = relocate(
            &source.0,
            address,
            address + 8,
            STANDARD,
            &mut destination.0,
        )
        .unwrap();
        let mut expected = image.bytes.to_vec();
        for (offset, value) in [
            (176, 0x30000),
            (168, 0x20010),
            (184, 0x202),
            (152, (-22i64) as u64),
        ] {
            put_u64(&mut expected, DEST_FRAME + offset, value);
        }
        let image = image
            .complete_vdso_call(FunctionReturn {
                entry: 0x10800,
                stack: 0x20008,
                target: 0x30000,
                flags,
                result: -22,
                owned_tf,
            })
            .unwrap();
        assert_eq!(image.bytes, expected);
    }
}

#[test]
fn vdso_return_rejects_mismatched_binding_without_any_mutation() {
    for mode in 0..8 {
        let mut source = fixture(FRAME, FLOATING, STANDARD);
        for (offset, value) in [(176, 0x10800), (168, 0x20008), (184, 0x10202)] {
            put_u64(&mut source.0, FRAME + offset, value);
        }
        let address = source.0.as_ptr() as usize + FRAME;
        let mut destination = Storage([0xa7; 2048]);
        let image = relocate(
            &source.0,
            address,
            address + 8,
            STANDARD,
            &mut destination.0,
        )
        .unwrap();
        let before = image.bytes.to_vec();
        let mut call = FunctionReturn {
            entry: 0x10800,
            stack: 0x20008,
            target: 0x30000,
            flags: 0x10202,
            result: 0,
            owned_tf: false,
        };
        match mode {
            0 => call.entry += 1,
            1 => call.stack += 8,
            2 => call.target = 0,
            3 => call.target = 1 << 47,
            4 => call.flags &= !0x10000,
            5 => call.flags |= 0x20000,
            6 => call.owned_tf = true,
            _ => call.stack = u64::MAX,
        }
        assert!(image.complete_vdso_call(call).is_err());
        assert_eq!(&destination.0[..before.len()], before);
    }
}

#[test]
fn private_start_editor_changes_only_bound_pc_and_fault_rf() {
    let mut source = fixture(FRAME, FLOATING, STANDARD);
    put_u64(&mut source.0, FRAME + 176, 0x4000);
    put_u64(&mut source.0, FRAME + 184, 0x10202);
    let address = source.0.as_ptr() as usize + FRAME;
    let mut destination = Storage([0xa7; 2048]);
    let image = relocate(
        &source.0,
        address,
        address + 8,
        STANDARD,
        &mut destination.0,
    )
    .unwrap();
    let mut expected = image.bytes.to_vec();
    put_u64(&mut expected, DEST_FRAME + 176, 0x6000);
    put_u64(&mut expected, DEST_FRAME + 184, 0x202);
    let image = image
        .complete_private_start(0x4000, 0x10202, 0x6000)
        .unwrap();
    assert_eq!(image.bytes, expected);
}

#[test]
fn private_start_editor_refusals_do_not_change_bytes() {
    for (fault, flags, target) in [
        (0x4001, 0x10202, 0x6000),
        (0, 0x10202, 0x6000),
        (0x4000, 0x202, 0x6000),
        (0x4000, 0x10302, 0x6000),
        (0x4000, 0x10202, 0),
        (0x4000, 0x10202, 1 << 47),
    ] {
        let mut source = fixture(FRAME, FLOATING, STANDARD);
        put_u64(&mut source.0, FRAME + 176, 0x4000);
        put_u64(&mut source.0, FRAME + 184, 0x10202);
        let address = source.0.as_ptr() as usize + FRAME;
        let mut destination = Storage([0xa7; 2048]);
        let image = relocate(
            &source.0,
            address,
            address + 8,
            STANDARD,
            &mut destination.0,
        )
        .unwrap();
        let expected = image.bytes.to_vec();
        assert!(image.complete_private_start(fault, flags, target).is_err());
        assert_eq!(&destination.0[..expected.len()], expected);
    }
}

#[test]
fn syscall_result_and_owned_tf_edit_only_checked_fields() {
    let mut source = fixture(FRAME, FLOATING, STANDARD);
    for (offset, value) in [(176, 0x4002), (184, 0x302), (72, 0x302), (160, 0x4002)] {
        put_u64(&mut source.0, FRAME + offset, value);
    }
    let address = source.0.as_ptr() as usize + FRAME;
    let mut destination = Storage([0xa7; 2048]);
    let image = relocate(
        &source.0,
        address,
        address + 8,
        STANDARD,
        &mut destination.0,
    )
    .unwrap();
    let mut expected = image.frame_bytes().to_vec();
    let floating = image.fp_bytes().to_vec();
    let image = image.owned_syscall_tf(0x4002, 0x302, 0x302).unwrap();
    put_u64(&mut expected, 184, 0x202);
    put_u64(&mut expected, 72, 0x202);
    assert_eq!(image.frame_bytes(), expected);
    let image = image.complete_syscall(0x4002, -libc::EIO as i64).unwrap();
    put_u64(&mut expected, 152, (-libc::EIO as i64) as u64);
    assert_eq!(image.frame_bytes(), expected);
    assert_eq!(image.fp_bytes(), floating);
}

#[test]
fn syscall_edit_refuses_wrong_resume_or_unowned_tf_without_mutation() {
    for mode in 0..5 {
        let mut source = fixture(FRAME, FLOATING, STANDARD);
        for (offset, value) in [(176, 0x4002), (184, 0x302), (72, 0x202)] {
            put_u64(&mut source.0, FRAME + offset, value);
        }
        let address = source.0.as_ptr() as usize + FRAME;
        let mut destination = Storage([0xa7; 2048]);
        let image = relocate(
            &source.0,
            address,
            address + 8,
            STANDARD,
            &mut destination.0,
        )
        .unwrap();
        let before = image.bytes.to_vec();
        let result = match mode {
            0 => image.complete_syscall(0x4004, 42),
            1 => image.complete_syscall(0, 42),
            2 => image.owned_syscall_tf(0x4002, 0x302, 0x202),
            3 => image.owned_syscall_tf(0x4002, 0x302, 0x302),
            _ => image.owned_syscall_tf(0x4002, 0x202, 0x202),
        };
        assert!(result.is_err());
        assert_eq!(&destination.0[..before.len()], before);
    }
}

#[test]
fn owned_tf_editor_changes_only_tf_and_keeps_full_fp_image() {
    let mut source = fixture(FRAME, FLOATING, STANDARD);
    put_u64(&mut source.0, FRAME + 176, 0x4000);
    put_u64(&mut source.0, FRAME + 184, 0xed7);
    let address = source.0.as_ptr() as usize + FRAME;
    let mut destination = Storage([0xa7; 2048]);
    let image = relocate(
        &source.0,
        address,
        address + 8,
        STANDARD,
        &mut destination.0,
    )
    .unwrap();
    let original = image.frame_bytes().to_vec();
    let floating = image.fp_bytes().to_vec();
    let mut expected = original.clone();
    put_u64(&mut expected, 184, 0xfd7);
    let image = image.owned_single_step(0x4000, 0xed7, true).unwrap();
    assert_eq!(image.frame_bytes(), expected);
    assert_eq!(image.fp_bytes(), floating);
    let image = image.owned_single_step(0x4000, 0xfd7, false).unwrap();
    assert_eq!(image.frame_bytes(), original);
    assert_eq!(image.fp_bytes(), floating);
    assert_eq!(read_u64(&source.0, FRAME + 184).unwrap(), 0xed7);
}

#[test]
fn owned_tf_editor_refuses_wrong_pc_flags_or_transition_without_mutation() {
    for (pc, expected_flags, enable, error) in [
        (0x4001, 0xed7, true, Error::InstructionPointer),
        (0, 0xed7, true, Error::InstructionPointer),
        (0x4000, 0x202, true, Error::Flags),
        (0x4000, 0xed7, false, Error::Flags),
    ] {
        let mut source = fixture(FRAME, FLOATING, STANDARD);
        put_u64(&mut source.0, FRAME + 176, 0x4000);
        put_u64(&mut source.0, FRAME + 184, 0xed7);
        let address = source.0.as_ptr() as usize + FRAME;
        let mut destination = Storage([0xa7; 2048]);
        let image = relocate(
            &source.0,
            address,
            address + 8,
            STANDARD,
            &mut destination.0,
        )
        .unwrap();
        let before = image.bytes.to_vec();
        assert_eq!(
            image
                .owned_single_step(pc, expected_flags, enable)
                .unwrap_err(),
            error
        );
        assert_eq!(&destination.0[..before.len()], before);
    }
}

#[cfg(feature = "coordinator-rpc")]
#[test]
fn cpuid_completion_changes_only_outputs_and_checked_next_pc() {
    let mut source = fixture(FRAME, FLOATING, STANDARD);
    put_u64(&mut source.0, FRAME + 176, 0x4000);
    let address = source.0.as_ptr() as usize + FRAME;
    let mut destination = Storage([0xa7; 2048]);
    let image = relocate(
        &source.0,
        address,
        address + 8,
        STANDARD,
        &mut destination.0,
    )
    .unwrap();
    let mut expected = image.frame_bytes().to_vec();
    let floating = image.fp_bytes().to_vec();
    let result = reverie::CpuIdResult {
        eax: u32::MAX,
        ebx: 2,
        ecx: 3,
        edx: 4,
    };
    for (offset, value) in [
        (152, u64::from(u32::MAX)),
        (136, 2),
        (160, 3),
        (144, 4),
        (176, 0x4002),
    ] {
        put_u64(&mut expected, offset, value);
    }
    let image = image.complete_cpuid(0x4000, result).unwrap();
    assert_eq!(image.frame_bytes(), expected);
    assert_eq!(image.fp_bytes(), floating);
    assert_eq!(read_u64(&source.0, FRAME + 176).unwrap(), 0x4000);
}

#[cfg(feature = "coordinator-rpc")]
#[test]
fn cpuid_completion_refuses_invalid_pc_without_partial_update() {
    for (captured, supplied) in [
        (0, 0),
        (0x4000, 0x4002),
        (u64::MAX, u64::MAX),
        ((1 << 47) - 2, (1 << 47) - 2),
    ] {
        let mut source = fixture(FRAME, FLOATING, STANDARD);
        put_u64(&mut source.0, FRAME + 176, captured);
        let address = source.0.as_ptr() as usize + FRAME;
        let mut destination = Storage([0xa7; 2048]);
        let image = relocate(
            &source.0,
            address,
            address + 8,
            STANDARD,
            &mut destination.0,
        )
        .unwrap();
        let before = image.frame_bytes().to_vec();
        assert_eq!(
            image
                .complete_cpuid(
                    supplied,
                    reverie::CpuIdResult {
                        eax: 1,
                        ebx: 2,
                        ecx: 3,
                        edx: 4
                    }
                )
                .unwrap_err(),
            Error::InstructionPointer
        );
        assert_eq!(
            &destination.0[DEST_FRAME..DEST_FRAME + PREFIX_BYTES],
            before
        );
    }
}

#[repr(align(64))]
struct Storage([u8; 2048]);

#[cfg(feature = "coordinator-rpc")]
#[test]
fn rdtsc_completion_changes_only_typed_outputs_and_checked_next_pc() {
    for request in [reverie::Rdtsc::Tsc, reverie::Rdtsc::Tscp] {
        for tsc in [0, 0xfedc_ba98_7654_3210, u64::MAX] {
            for aux in [None, Some(0), Some(u32::MAX)] {
                let mut source = fixture(FRAME, FLOATING, STANDARD);
                put_u64(&mut source.0, FRAME + 176, 0x4000);
                put_u64(&mut source.0, FRAME + 160, 0xfedc_ba98_7654_3210);
                let original = source.0;
                let address = source.0.as_ptr() as usize + FRAME;
                let mut destination = Storage([0xa7; 2048]);
                let image = relocate(
                    &source.0,
                    address,
                    address + 8,
                    STANDARD,
                    &mut destination.0,
                )
                .unwrap();
                let mut expected = image.frame_bytes().to_vec();
                let floating = image.fp_bytes().to_vec();
                put_u64(&mut expected, 152, u64::from(tsc as u32));
                put_u64(&mut expected, 144, tsc >> 32);
                let length = if request == reverie::Rdtsc::Tscp {
                    put_u64(&mut expected, 160, u64::from(aux.unwrap_or(0)));
                    3
                } else {
                    2
                };
                put_u64(&mut expected, 176, 0x4000 + length);
                let image = image
                    .complete_rdtsc(0x4000, request, reverie::RdtscResult { tsc, aux })
                    .unwrap();
                assert_eq!(image.frame_bytes(), expected);
                assert_eq!(image.fp_bytes(), floating);
                assert_eq!(source.0, original);
            }
        }
    }
}

#[cfg(feature = "coordinator-rpc")]
#[test]
fn rdtsc_completion_refuses_invalid_pc_without_partial_update() {
    for request in [reverie::Rdtsc::Tsc, reverie::Rdtsc::Tscp] {
        let length = if request == reverie::Rdtsc::Tscp {
            3
        } else {
            2
        };
        for (captured, supplied) in [
            (0, 0),
            (0x4000, 0x4001),
            (u64::MAX, u64::MAX),
            ((1 << 47) - length, (1 << 47) - length),
        ] {
            let mut source = fixture(FRAME, FLOATING, STANDARD);
            put_u64(&mut source.0, FRAME + 176, captured);
            let address = source.0.as_ptr() as usize + FRAME;
            let mut destination = Storage([0xa7; 2048]);
            let image = relocate(
                &source.0,
                address,
                address + 8,
                STANDARD,
                &mut destination.0,
            )
            .unwrap();
            let before = image.frame_bytes().to_vec();
            let floating = image.fp_bytes().to_vec();
            assert_eq!(
                image
                    .complete_rdtsc(
                        supplied,
                        request,
                        reverie::RdtscResult {
                            tsc: 1,
                            aux: Some(2)
                        }
                    )
                    .unwrap_err(),
                Error::InstructionPointer
            );
            assert_eq!(
                &destination.0[DEST_FRAME..DEST_FRAME + PREFIX_BYTES],
                before
            );
            assert_eq!(&destination.0[DEST_FP..DEST_FP + floating.len()], floating);
        }
    }
}

fn put_u32(bytes: &mut [u8], offset: usize, value: u32) {
    bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

fn put_u64(bytes: &mut [u8], offset: usize, value: u64) {
    bytes[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
}

fn fixture(frame: usize, floating: usize, format: Format) -> Box<Storage> {
    let mut source = Box::new(Storage([0x5a; 2048]));
    populate(&mut source.0, frame, floating, format);
    source
}

fn populate(source: &mut [u8], frame: usize, floating: usize, format: Format) {
    let address = source.as_ptr() as usize;
    put_u64(source, frame + FP_POINTER, (address + floating) as u64);
    match format {
        Format::Legacy => {
            put_u64(source, frame + UC_FLAGS, 6);
            put_u32(source, floating + 464, 0);
        }
        Format::StandardXsave {
            xfeatures,
            xstate_size,
        } => {
            put_u64(source, frame + UC_FLAGS, 7);
            put_u32(source, floating + 464, MAGIC1);
            put_u32(source, floating + 468, xstate_size + 4);
            put_u64(source, floating + 472, xfeatures);
            put_u32(source, floating + 480, xstate_size);
            source[floating + 484..floating + 512].fill(0);
            put_u64(source, floating + 512, xfeatures);
            source[floating + 520..floating + XSAVE_HEADER_END].fill(0);
            put_u32(source, floating + xstate_size as usize, MAGIC2);
        }
    }
}

fn reject(source: &[u8], frame: usize, restorer: usize, format: Format, expected: Error) {
    let mut destination = Storage([0xa7; 2048]);
    assert_eq!(
        relocate(source, frame, restorer, format, &mut destination.0).unwrap_err(),
        expected
    );
    assert_eq!(destination.0, [0xa7; 2048]);
}

#[test]
fn standard_copy_preserves_every_byte_except_relocated_pointer() {
    let source = fixture(FRAME, FLOATING, STANDARD);
    let original = source.0;
    let address = source.0.as_ptr() as usize + FRAME;
    let mut destination = Storage([0xa7; 2048]);
    let image = relocate(
        &source.0,
        address,
        address + 8,
        STANDARD,
        &mut destination.0,
    )
    .unwrap();
    let mut expected = original[FRAME..FRAME + PREFIX_BYTES].to_vec();
    put_u64(&mut expected, FP_POINTER, image.fp_bytes().as_ptr() as u64);
    assert_eq!(image.frame_bytes(), expected);
    assert_eq!(image.fp_bytes(), &original[FLOATING..FLOATING + 836]);
    assert_eq!(image.frame_bytes().as_ptr() as usize % 16, 8);
    assert_eq!(image.fp_bytes().as_ptr() as usize % 64, 0);
    assert_eq!(source.0, original);
    assert_eq!(&destination.0[..DEST_FRAME], &[0; DEST_FRAME]);
    assert_eq!(
        &destination.0[DEST_FP + 836..],
        &[0xa7; 2048 - DEST_FP - 836]
    );
}

#[test]
fn output_does_not_borrow_retired_source() {
    let mut source = fixture(FRAME, FLOATING, STANDARD);
    let address = source.0.as_ptr() as usize + FRAME;
    let expected = source.0[FLOATING..FLOATING + 836].to_vec();
    let mut destination = Storage([0xa7; 2048]);
    let image = relocate(
        &source.0,
        address,
        address + 8,
        STANDARD,
        &mut destination.0,
    )
    .unwrap();
    source.0.fill(0xcc);
    drop(source);
    assert_eq!(image.fp_bytes(), expected);
    assert_eq!(
        read_u64(image.frame_bytes(), FP_POINTER).unwrap(),
        image.fp_bytes().as_ptr() as u64
    );
}

#[test]
fn legacy_reserved_bytes_survive_and_only_sixteen_byte_fp_alignment_is_required() {
    let floating = FLOATING + 16;
    let source = fixture(FRAME, floating, Format::Legacy);
    let address = source.0.as_ptr() as usize + FRAME;
    let mut destination = Storage([0xa7; 2048]);
    let image = relocate(
        &source.0,
        address,
        address + 8,
        Format::Legacy,
        &mut destination.0,
    )
    .unwrap();
    assert_eq!(
        image.fp_bytes(),
        &source.0[floating..floating + LEGACY_BYTES]
    );
    assert_eq!(&image.fp_bytes()[468..512], &[0x5a; 44]);
}

#[test]
fn floating_span_can_precede_prefix_without_copying_the_gap() {
    let frame = 1032;
    let floating = 64;
    let source = fixture(frame, floating, STANDARD);
    let address = source.0.as_ptr() as usize + frame;
    let mut destination = Storage([0xa7; 2048]);
    let image = relocate(
        &source.0,
        address,
        address + 8,
        STANDARD,
        &mut destination.0,
    )
    .unwrap();
    assert_eq!(image.fp_bytes(), &source.0[floating..floating + 836]);
    assert_eq!(image.frame_bytes().len(), PREFIX_BYTES);
}

#[test]
fn every_truncated_prefix_or_floating_span_refuses_without_writes() {
    let source = fixture(FRAME, FLOATING, STANDARD);
    let address = source.0.as_ptr() as usize + FRAME;
    for length in 0..FRAME + PREFIX_BYTES {
        reject(
            &source.0[..length],
            address,
            address + 8,
            STANDARD,
            Error::SourceBounds,
        );
    }
    for length in FLOATING..FLOATING + 836 {
        reject(
            &source.0[..length],
            address,
            address + 8,
            STANDARD,
            Error::SourceBounds,
        );
    }
}

#[test]
fn pointed_span_cannot_escape_or_overlap_source_prefix() {
    for (offset, expected) in [
        (0, Error::Overlap),
        (64, Error::Overlap),
        (2048, Error::SourceBounds),
        (2048 - LEGACY_BYTES + 1, Error::SourceBounds),
    ] {
        let mut source = fixture(FRAME, FLOATING, STANDARD);
        let base = source.0.as_ptr() as usize;
        put_u64(&mut source.0, FRAME + FP_POINTER, (base + offset) as u64);
        reject(
            &source.0,
            base + FRAME,
            base + FRAME + 8,
            STANDARD,
            expected,
        );
    }
    let mut source = fixture(FRAME, FLOATING, STANDARD);
    let base = source.0.as_ptr() as usize;
    for (pointer, expected) in [
        ((base - 64) as u64, Error::SourceBounds),
        (u64::MAX - 255, Error::Overflow),
        (0, Error::NullFpUnsupported),
    ] {
        put_u64(&mut source.0, FRAME + FP_POINTER, pointer);
        reject(
            &source.0,
            base + FRAME,
            base + FRAME + 8,
            STANDARD,
            expected,
        );
    }
}

#[test]
fn late_floating_overlap_is_rejected() {
    let frame = 1032;
    let mut source = fixture(frame, 512, STANDARD);
    put_u64(&mut source.0, frame + UC_FLAGS, 7);
    let base = source.0.as_ptr() as usize;
    put_u64(&mut source.0, frame + FP_POINTER, (base + 512) as u64);
    reject(
        &source.0,
        base + frame,
        base + frame + 8,
        STANDARD,
        Error::Overlap,
    );
}

#[test]
fn frame_and_restorer_relation_and_alignment_are_checked() {
    let source = fixture(FRAME, FLOATING, STANDARD);
    let base = source.0.as_ptr() as usize;
    reject(
        &source.0,
        base + FRAME,
        base + FRAME,
        STANDARD,
        Error::RestorerPosition,
    );
    reject(&source.0, base, base + 8, STANDARD, Error::Alignment);
    reject(&source.0, base - 1, base + 7, STANDARD, Error::SourceBounds);
    reject(&source.0, usize::MAX - 7, 0, STANDARD, Error::Overflow);
    let source = fixture(FRAME, FLOATING + 16, STANDARD);
    let address = source.0.as_ptr() as usize + FRAME;
    reject(&source.0, address, address + 8, STANDARD, Error::Alignment);
}

#[test]
fn every_short_or_misaligned_destination_refuses_without_writes() {
    let source = fixture(FRAME, FLOATING, STANDARD);
    let address = source.0.as_ptr() as usize + FRAME;
    for length in 0..DEST_FP + 836 {
        let mut destination = Storage([0xa7; 2048]);
        assert_eq!(
            relocate(
                &source.0,
                address,
                address + 8,
                STANDARD,
                &mut destination.0[..length]
            )
            .unwrap_err(),
            Error::DestinationBounds
        );
        assert_eq!(destination.0, [0xa7; 2048]);
    }
    for offset in 1..64 {
        let mut destination = Storage([0xa7; 2048]);
        assert_eq!(
            relocate(
                &source.0,
                address,
                address + 8,
                STANDARD,
                &mut destination.0[offset..]
            )
            .unwrap_err(),
            Error::Alignment
        );
        assert_eq!(destination.0, [0xa7; 2048]);
    }
}

#[test]
fn metadata_trailer_and_reserved_header_corruption_refuse() {
    for offset in [
        468, 469, 470, 471, 472, 479, 480, 483, 484, 511, 519, 520, 527, 528, 575, 832, 835,
    ] {
        let mut source = fixture(FRAME, FLOATING, STANDARD);
        source.0[FLOATING + offset] ^= 0x80;
        let address = source.0.as_ptr() as usize + FRAME;
        reject(&source.0, address, address + 8, STANDARD, Error::Metadata);
    }
}

#[test]
fn format_flags_features_and_compacted_layout_are_not_silently_repaired() {
    for flags in [0, 1, 2, 3, 4, 5, 6, 15, u64::MAX] {
        let mut source = fixture(FRAME, FLOATING, STANDARD);
        put_u64(&mut source.0, FRAME + UC_FLAGS, flags);
        let address = source.0.as_ptr() as usize + FRAME;
        reject(
            &source.0,
            address,
            address + 8,
            STANDARD,
            Error::UnsupportedFormat,
        );
    }
    let source = fixture(FRAME, FLOATING, STANDARD);
    let address = source.0.as_ptr() as usize + FRAME;
    for format in [
        Format::Legacy,
        Format::StandardXsave {
            xfeatures: 0,
            xstate_size: 832,
        },
        Format::StandardXsave {
            xfeatures: 7 | (1 << 8),
            xstate_size: 832,
        },
        Format::StandardXsave {
            xfeatures: 7 | (1 << 63),
            xstate_size: 832,
        },
        Format::StandardXsave {
            xfeatures: 7,
            xstate_size: 575,
        },
    ] {
        reject(
            &source.0,
            address,
            address + 8,
            format,
            Error::UnsupportedFormat,
        );
    }
    reject(
        &source.0,
        address,
        address + 8,
        Format::StandardXsave {
            xfeatures: 3,
            xstate_size: 832,
        },
        Error::Metadata,
    );
    reject(
        &source.0,
        address,
        address + 8,
        Format::StandardXsave {
            xfeatures: 7,
            xstate_size: u32::MAX,
        },
        Error::Overflow,
    );
    let mut source = source;
    put_u64(&mut source.0, FLOATING + 520, 1 << 63);
    reject(&source.0, address, address + 8, STANDARD, Error::Metadata);
    put_u32(&mut source.0, FLOATING + 464, 0);
    reject(
        &source.0,
        address,
        address + 8,
        STANDARD,
        Error::UnsupportedFormat,
    );
}

#[test]
fn absent_active_components_do_not_shrink_the_copied_payload() {
    let mut source = fixture(FRAME, FLOATING, STANDARD);
    put_u64(&mut source.0, FLOATING + 512, 0);
    let address = source.0.as_ptr() as usize + FRAME;
    let mut destination = Storage([0xa7; 2048]);
    let image = relocate(
        &source.0,
        address,
        address + 8,
        STANDARD,
        &mut destination.0,
    )
    .unwrap();
    assert_eq!(image.fp_bytes().len(), 836);
    assert_eq!(image.fp_bytes(), &source.0[FLOATING..FLOATING + 836]);
}

#[test]
fn checked_range_arithmetic_and_allocation_overlap_do_not_need_aliased_references() {
    assert_eq!(allocation(usize::MAX - 7, 8), Err(Error::Overflow));
    assert_eq!(span(&(64..128), usize::MAX - 1, 4), Err(Error::Overflow));
    assert_eq!(span(&(64..128), 63, 1), Err(Error::SourceBounds));
    assert_eq!(span(&(64..128), 127, 2), Err(Error::SourceBounds));
    assert_eq!(span(&(64..128), 64, 64), Ok(0..64));
    assert!(overlaps(&(64..128), &(64..128)));
    assert!(overlaps(&(64..128), &(127..256)));
    assert!(overlaps(&(64..128), &(0..65)));
    assert!(!overlaps(&(64..128), &(128..256)));
    assert!(!overlaps(&(64..128), &(96..96)));
    assert!(!overlaps(&(96..96), &(64..128)));
}

#[test]
fn supplied_host_sized_synthetic_payload_is_not_capped_to_a_smaller_default() {
    #[repr(align(64))]
    struct LargeStorage([u8; 4096]);
    let format = Format::StandardXsave {
        xfeatures: 0x2e7,
        xstate_size: 2440,
    };
    let mut source = Box::new(LargeStorage([0x5a; 4096]));
    populate(&mut source.0, FRAME, FLOATING, format);
    let address = source.0.as_ptr() as usize + FRAME;
    let mut destination = LargeStorage([0xa7; 4096]);
    let image = relocate(&source.0, address, address + 8, format, &mut destination.0).unwrap();
    assert_eq!(image.fp_bytes().len(), 2444);
    assert_eq!(image.fp_bytes(), &source.0[FLOATING..FLOATING + 2444]);
}
