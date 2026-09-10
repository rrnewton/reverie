use super::*;

const BASE: u64 = 0x7000_0000;
const STACK: Range<u64> = 0x7fff_ffe0_0000..0x7fff_fff0_0000;
const BRK: u64 = 0x405000;

fn put16(bytes: &mut [u8], offset: usize, value: u16) {
    bytes[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
}

fn put32(bytes: &mut [u8], offset: usize, value: u32) {
    bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

fn put64(bytes: &mut [u8], offset: usize, value: u64) {
    bytes[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
}

fn interpreter() -> Vec<u8> {
    let mut bytes = vec![0; 0x2100];
    bytes[..9].copy_from_slice(b"\x7fELF\x02\x01\x01\x00\x00");
    put16(&mut bytes, 16, 3);
    put16(&mut bytes, 18, 62);
    put32(&mut bytes, 20, 1);
    put64(&mut bytes, 24, 0x1010);
    put64(&mut bytes, 32, ELF_HEADER as u64);
    put16(&mut bytes, 52, ELF_HEADER as u16);
    put16(&mut bytes, 54, PROGRAM_HEADER as u16);
    put16(&mut bytes, 56, 3);
    for (index, (offset, address, file_size, memory_size, flags)) in [
        (0, 0, 0x200, 0x200, 4),
        (0x1000, 0x1000, 0x180, 0x180, 5),
        (0x2000, 0x3000, 0x100, 0x1800, 6),
    ]
    .into_iter()
    .enumerate()
    {
        let header = ELF_HEADER + index * PROGRAM_HEADER;
        put32(&mut bytes, header, 1);
        put32(&mut bytes, header + 4, flags);
        put64(&mut bytes, header + 8, offset);
        put64(&mut bytes, header + 16, address);
        put64(&mut bytes, header + 32, file_size);
        put64(&mut bytes, header + 40, memory_size);
        put64(&mut bytes, header + 48, PAGE);
    }
    bytes
}

fn auxv_entries() -> Vec<(u64, u64)> {
    vec![
        (3, 0x400040),
        (4, 56),
        (5, 3),
        (6, PAGE),
        (7, BASE),
        (9, 0x401000),
        (25, STACK.start + 0x1000),
        (31, STACK.start + 0x1800),
        (33, 0x7fff_0000_0000),
        (0x1000, 123),
        (0x1000, 456),
        (0, 0),
    ]
}

fn encode(entries: &[(u64, u64)]) -> Vec<u8> {
    entries
        .iter()
        .flat_map(|(tag, value)| tag.to_le_bytes().into_iter().chain(value.to_le_bytes()))
        .collect()
}

fn initial() -> AuxvSnapshot {
    AuxvSnapshot::parse(&encode(&auxv_entries()), STACK, BRK).unwrap()
}

fn changed_auxv(tag: u64, value: u64) -> Vec<u8> {
    let mut entries = auxv_entries();
    entries.iter_mut().find(|entry| entry.0 == tag).unwrap().1 = value;
    encode(&entries)
}

fn image_error(bytes: &[u8]) -> PlanError {
    InterpreterImage::parse(bytes).unwrap_err()
}

#[test]
fn exact_plan_preserves_guest_identity_and_auxv() {
    let bytes = interpreter();
    let image = InterpreterImage::parse(&bytes).unwrap();
    let initial = initial();
    let original = initial.clone();
    let plan = image
        .plan_at_original_base(&initial, BASE..BASE + 0x6000, &[0x400000..0x404000; 1])
        .unwrap();
    assert_eq!(image.required_span(), 0x5000);
    assert_eq!(image.required_alignment(), PAGE);
    assert_eq!(plan.image(), bytes);
    assert_eq!(plan.image().as_ptr(), bytes.as_ptr());
    assert_eq!(plan.initial(), &original);
    assert_eq!(initial, original);
    assert_eq!(plan.initial().bytes(), encode(&auxv_entries()));
    assert_eq!(plan.initial().program_headers(), &(0x400040..0x4000e8));
    assert_eq!(plan.initial().guest_entry(), 0x401000);
    assert_eq!(plan.initial().brk(), BRK);
    assert_eq!(plan.initial().base(), BASE);
    assert_eq!(plan.initial().stack(), &STACK);
    assert_eq!(
        plan.initial().random(),
        &(STACK.start + 0x1000..STACK.start + 0x1010)
    );
    assert_eq!(plan.initial().execfn(), STACK.start + 0x1800);
    assert_eq!(plan.initial().vdso(), 0x7fff_0000_0000);
    assert_eq!(plan.interpreter_entry(), BASE + 0x1010);
    assert_eq!(plan.reservation(), &(BASE..BASE + 0x6000));
    assert_eq!(
        plan.gaps(),
        &[BASE + 0x2000..BASE + 0x3000, BASE + 0x5000..BASE + 0x6000]
    );
    assert_eq!(
        plan.segments(),
        &[
            LoadMapping {
                pages: BASE..BASE + 0x1000,
                memory: BASE..BASE + 0x200,
                file_pages: Some((0, BASE..BASE + 0x1000)),
                anonymous_pages: BASE + 0x1000..BASE + 0x1000,
                zero_fill: BASE + 0x200..BASE + 0x200,
                flags: 4,
            },
            LoadMapping {
                pages: BASE + 0x1000..BASE + 0x2000,
                memory: BASE + 0x1000..BASE + 0x1180,
                file_pages: Some((0x1000, BASE + 0x1000..BASE + 0x2000)),
                anonymous_pages: BASE + 0x2000..BASE + 0x2000,
                zero_fill: BASE + 0x1180..BASE + 0x1180,
                flags: 5,
            },
            LoadMapping {
                pages: BASE + 0x3000..BASE + 0x5000,
                memory: BASE + 0x3000..BASE + 0x4800,
                file_pages: Some((0x2000, BASE + 0x3000..BASE + 0x4000)),
                anonymous_pages: BASE + 0x4000..BASE + 0x5000,
                zero_fill: BASE + 0x3100..BASE + 0x5000,
                flags: 6,
            },
        ]
    );
}

#[test]
fn auxv_copy_is_independent_and_unknown_duplicates_are_preserved() {
    let mut bytes = encode(&auxv_entries());
    let original = bytes.clone();
    let snapshot = AuxvSnapshot::parse(&bytes, STACK, BRK).unwrap();
    bytes.fill(0);
    assert_eq!(snapshot.bytes(), original);
}

#[test]
fn auxv_requires_exact_complete_terminator() {
    let bytes = encode(&auxv_entries());
    for length in 0..bytes.len() {
        assert!(
            AuxvSnapshot::parse(&bytes[..length], STACK, BRK).is_err(),
            "length {length}"
        );
    }
    let mut entries = auxv_entries();
    entries.push((1, 1));
    assert_eq!(
        AuxvSnapshot::parse(&encode(&entries), STACK, BRK).unwrap_err(),
        PlanError::Invalid("auxv terminator")
    );
    entries.pop();
    entries.last_mut().unwrap().1 = 1;
    assert_eq!(
        AuxvSnapshot::parse(&encode(&entries), STACK, BRK).unwrap_err(),
        PlanError::Invalid("auxv terminator")
    );
}

#[test]
fn auxv_requires_unique_identity_fields_including_vdso() {
    for tag in REQUIRED_AUXV {
        let mut entries = auxv_entries();
        entries.retain(|entry| entry.0 != tag);
        assert_eq!(
            AuxvSnapshot::parse(&encode(&entries), STACK, BRK).unwrap_err(),
            PlanError::MissingAuxv(tag)
        );
        let mut entries = auxv_entries();
        entries.insert(0, (tag, 1));
        assert_eq!(
            AuxvSnapshot::parse(&encode(&entries), STACK, BRK).unwrap_err(),
            PlanError::DuplicateAuxv(tag)
        );
    }
}

#[test]
fn auxv_rejects_invalid_coordinates_and_arithmetic() {
    for (tag, value, expected) in [
        (4, 55, PlanError::Invalid("guest program headers")),
        (5, 0, PlanError::Invalid("guest program headers")),
        (5, u64::MAX, PlanError::Overflow("AT_PHNUM")),
        (3, u64::MAX, PlanError::Overflow("guest program headers")),
        (6, 8192, PlanError::Unsupported("AT_PAGESZ")),
        (7, 0, PlanError::Invalid("AT_BASE")),
        (7, BASE + 1, PlanError::Invalid("auxv mapping alignment")),
        (9, USER_END, PlanError::Invalid("AT_ENTRY")),
        (
            25,
            STACK.end - 15,
            PlanError::Invalid("stack auxv pointers"),
        ),
        (25, u64::MAX, PlanError::Overflow("AT_RANDOM")),
        (31, STACK.end, PlanError::Invalid("stack auxv pointers")),
        (33, 0, PlanError::Invalid("vDSO")),
        (33, BASE + 1, PlanError::Invalid("auxv mapping alignment")),
    ] {
        assert_eq!(
            AuxvSnapshot::parse(&changed_auxv(tag, value), STACK, BRK).unwrap_err(),
            expected,
            "tag {tag}"
        );
    }
    let bytes = encode(&auxv_entries());
    for stack in [
        0..1,
        1..1,
        Range { start: 2, end: 1 },
        USER_END - 1..USER_END + 1,
    ] {
        assert_eq!(
            AuxvSnapshot::parse(&bytes, stack, BRK).unwrap_err(),
            PlanError::Invalid("initial stack")
        );
    }
    assert_eq!(
        AuxvSnapshot::parse(&bytes, STACK, 0).unwrap_err(),
        PlanError::Invalid("initial brk")
    );
}

#[test]
fn truncated_interpreter_never_produces_plan() {
    let bytes = interpreter();
    for length in 0..bytes.len() {
        assert!(
            InterpreterImage::parse(&bytes[..length]).is_err(),
            "length {length}"
        );
    }
}

#[test]
fn elf_abi_and_table_refusals_are_explicit() {
    for (offset, value) in [
        (0, 0),
        (4, 1),
        (5, 2),
        (6, 0),
        (7, 9),
        (8, 1),
        (16, 2),
        (18, 3),
        (20, 0),
        (48, 1),
    ] {
        let mut bytes = interpreter();
        bytes[offset] = value;
        assert!(InterpreterImage::parse(&bytes).is_err(), "offset {offset}");
    }
    for (offset, value, expected) in [
        (52, 63, PlanError::Invalid("ELF header sizes")),
        (54, 55, PlanError::Invalid("ELF header sizes")),
        (56, 0, PlanError::Unsupported("ELF program header count")),
        (
            56,
            u16::MAX,
            PlanError::Unsupported("ELF program header count"),
        ),
    ] {
        let mut bytes = interpreter();
        put16(&mut bytes, offset, value);
        assert_eq!(image_error(&bytes), expected);
    }
    for (offset, expected) in [
        (0, PlanError::Invalid("program header offset")),
        (u64::MAX, PlanError::Overflow("program header table")),
        (0x2100, PlanError::Truncated("program header table")),
    ] {
        let mut bytes = interpreter();
        put64(&mut bytes, 32, offset);
        assert_eq!(image_error(&bytes), expected);
    }
}

#[test]
fn nested_interpreter_unmapped_headers_and_nonzero_base_refuse() {
    let mut bytes = interpreter();
    put32(&mut bytes, ELF_HEADER, 3);
    assert_eq!(
        image_error(&bytes),
        PlanError::Unsupported("nested PT_INTERP")
    );
    let mut bytes = interpreter();
    put64(&mut bytes, ELF_HEADER + 32, 64);
    assert_eq!(
        image_error(&bytes),
        PlanError::Unsupported("unmapped interpreter headers")
    );
    let mut bytes = interpreter();
    put64(&mut bytes, ELF_HEADER + 16, PAGE);
    assert_eq!(
        image_error(&bytes),
        PlanError::Unsupported("nonzero first load base")
    );
}

#[test]
fn load_permissions_alignment_and_page_aliasing_refuse() {
    for flags in [0, 1, 2, 3, 7, 8, 12] {
        let mut bytes = interpreter();
        put32(&mut bytes, ELF_HEADER + 4, flags);
        assert_eq!(
            image_error(&bytes),
            PlanError::Unsupported("load permissions")
        );
    }
    for (offset, value) in [(48, 3), (16, 1), (8, 1)] {
        let mut bytes = interpreter();
        put64(&mut bytes, ELF_HEADER + offset, value);
        assert_eq!(image_error(&bytes), PlanError::Invalid("load alignment"));
    }
    let mut bytes = interpreter();
    put64(&mut bytes, ELF_HEADER + PROGRAM_HEADER + 16, 0);
    assert_eq!(
        image_error(&bytes),
        PlanError::Unsupported("overlapping or unordered load pages")
    );
    let mut bytes = interpreter();
    put64(&mut bytes, ELF_HEADER + 2 * PROGRAM_HEADER + 16, 0);
    assert_eq!(
        image_error(&bytes),
        PlanError::Unsupported("overlapping or unordered load pages")
    );
}

#[test]
fn load_extent_overflow_and_file_overrun_refuse() {
    let last = ELF_HEADER + 2 * PROGRAM_HEADER;
    for (field_offset, value, expected) in [
        (
            32,
            0x1801,
            PlanError::Invalid("load file size exceeds memory size"),
        ),
        (8, u64::MAX - (PAGE - 1), PlanError::Truncated("load file")),
        (40, u64::MAX, PlanError::Overflow("load memory")),
    ] {
        let mut bytes = interpreter();
        put64(&mut bytes, last + field_offset, value);
        assert_eq!(image_error(&bytes), expected);
    }
    let mut bytes = interpreter();
    put64(&mut bytes, last + 40, u64::MAX - 0x3000);
    assert_eq!(image_error(&bytes), PlanError::Overflow("page rounding"));
    let mut bytes = interpreter();
    put64(&mut bytes, last + 8, u64::MAX - (PAGE - 1));
    put64(&mut bytes, last + 32, PAGE);
    assert_eq!(image_error(&bytes), PlanError::Overflow("load file"));
}

#[test]
fn entry_requires_actual_file_backed_executable_bytes() {
    for entry in [0, 0x100, 0x1180, 0x2000, 0x3100, u64::MAX] {
        let mut bytes = interpreter();
        put64(&mut bytes, 24, entry);
        assert_eq!(
            image_error(&bytes),
            PlanError::Invalid("entry outside file-backed executable load")
        );
    }
    for entry in [0x1000, 0x117f] {
        let mut bytes = interpreter();
        put64(&mut bytes, 24, entry);
        assert!(InterpreterImage::parse(&bytes).is_ok());
    }
}

#[test]
fn bss_requires_final_writable_nonexecuting_load() {
    let mut bytes = interpreter();
    put64(&mut bytes, ELF_HEADER + PROGRAM_HEADER + 40, 0x200);
    assert_eq!(
        image_error(&bytes),
        PlanError::Unsupported("BSS outside final writable load")
    );
    for flags in [4, 5] {
        let mut bytes = interpreter();
        put32(&mut bytes, ELF_HEADER + 2 * PROGRAM_HEADER + 4, flags);
        assert_eq!(
            image_error(&bytes),
            PlanError::Unsupported("BSS outside final writable load")
        );
    }
}

#[test]
fn partial_file_and_bss_pages_have_exact_coordinates() {
    let mut bytes = interpreter();
    let last = ELF_HEADER + 2 * PROGRAM_HEADER;
    put64(&mut bytes, last + 8, 0x2003);
    put64(&mut bytes, last + 16, 0x3003);
    put64(&mut bytes, last + 32, 0xfd);
    let image = InterpreterImage::parse(&bytes).unwrap();
    let plan = image
        .plan_at_original_base(&initial(), BASE..BASE + 0x5000, &[])
        .unwrap();
    let load = &plan.segments()[2];
    assert_eq!(load.pages(), &(BASE + 0x3000..BASE + 0x5000));
    assert_eq!(load.memory(), &(BASE + 0x3003..BASE + 0x4803));
    assert_eq!(
        load.file_pages(),
        Some(&(0x2000, BASE + 0x3000..BASE + 0x4000))
    );
    assert_eq!(load.anonymous_pages(), &(BASE + 0x4000..BASE + 0x5000));
    assert_eq!(load.zero_fill(), &(BASE + 0x3100..BASE + 0x5000));
    assert_eq!(load.flags(), 6);
}

#[test]
fn entirely_anonymous_final_load_has_no_file_reader() {
    let mut bytes = interpreter();
    put64(&mut bytes, ELF_HEADER + 2 * PROGRAM_HEADER + 32, 0);
    let image = InterpreterImage::parse(&bytes).unwrap();
    let plan = image
        .plan_at_original_base(&initial(), BASE..BASE + 0x5000, &[])
        .unwrap();
    let load = &plan.segments()[2];
    assert_eq!(load.file_pages(), None);
    assert_eq!(load.anonymous_pages(), load.pages());
    assert_eq!(load.zero_fill(), load.pages());
}

#[test]
fn zero_bss_does_not_erase_file_page_tail() {
    let mut bytes = interpreter();
    put64(&mut bytes, ELF_HEADER + 2 * PROGRAM_HEADER + 40, 0x100);
    let image = InterpreterImage::parse(&bytes).unwrap();
    let plan = image
        .plan_at_original_base(&initial(), BASE..BASE + 0x4000, &[])
        .unwrap();
    assert_eq!(
        plan.segments()[2].zero_fill(),
        &(BASE + 0x3100..BASE + 0x3100)
    );
}

#[test]
fn unaligned_zero_file_size_retains_file_backed_prefix() {
    let mut bytes = interpreter();
    let last = ELF_HEADER + 2 * PROGRAM_HEADER;
    put64(&mut bytes, last + 8, 0x2003);
    put64(&mut bytes, last + 16, 0x3003);
    put64(&mut bytes, last + 32, 0);
    let image = InterpreterImage::parse(&bytes).unwrap();
    let plan = image
        .plan_at_original_base(&initial(), BASE..BASE + 0x5000, &[])
        .unwrap();
    let load = &plan.segments()[2];
    assert_eq!(load.memory(), &(BASE + 0x3003..BASE + 0x4803));
    assert_eq!(
        load.file_pages(),
        Some(&(0x2000, BASE + 0x3000..BASE + 0x4000))
    );
    assert_eq!(load.anonymous_pages(), &(BASE + 0x4000..BASE + 0x5000));
    assert_eq!(load.zero_fill(), &(BASE + 0x3003..BASE + 0x5000));
}

#[test]
fn reservation_cannot_change_base_or_exceed_supported_address_space() {
    let bytes = interpreter();
    let image = InterpreterImage::parse(&bytes).unwrap();
    let initial = initial();
    for (reservation, expected) in [
        (
            BASE + PAGE..BASE + 0x6000,
            PlanError::Invalid("AT_BASE reservation alignment"),
        ),
        (
            BASE..BASE + 0x4000,
            PlanError::Invalid("interpreter reservation too small"),
        ),
        (
            BASE..BASE + 0x5001,
            PlanError::Invalid("AT_BASE reservation alignment"),
        ),
        (
            BASE..USER_END + PAGE,
            PlanError::Invalid("interpreter reservation"),
        ),
        (BASE..BASE, PlanError::Invalid("interpreter reservation")),
    ] {
        assert_eq!(
            image
                .plan_at_original_base(&initial, reservation, &[])
                .unwrap_err(),
            expected
        );
    }
    let initial = AuxvSnapshot::parse(&changed_auxv(7, USER_END - PAGE), STACK, BRK).unwrap();
    assert_eq!(
        image
            .plan_at_original_base(&initial, USER_END - PAGE..USER_END, &[])
            .unwrap_err(),
        PlanError::Invalid("interpreter reservation too small")
    );
}

#[test]
fn large_alignment_is_not_satisfied_by_page_alignment_alone() {
    let mut bytes = interpreter();
    put64(&mut bytes, ELF_HEADER + 48, 0x20_0000);
    let image = InterpreterImage::parse(&bytes).unwrap();
    assert_eq!(image.required_alignment(), 0x20_0000);
    assert!(
        image
            .plan_at_original_base(&initial(), BASE..BASE + 0x5000, &[])
            .is_ok()
    );
    let base = BASE + PAGE;
    let initial = AuxvSnapshot::parse(&changed_auxv(7, base), STACK, BRK).unwrap();
    assert_eq!(
        image
            .plan_at_original_base(&initial, base..base + 0x5000, &[])
            .unwrap_err(),
        PlanError::Invalid("AT_BASE reservation alignment")
    );
}

#[test]
fn every_protected_identity_location_blocks_whole_reservation() {
    let bytes = interpreter();
    let image = InterpreterImage::parse(&bytes).unwrap();
    for (tag, value, name) in [
        (3, BASE + 0x2000, "guest program headers"),
        (9, BASE + 0x2000, "guest entry"),
        (33, BASE + 0x5000, "vDSO"),
    ] {
        let initial = AuxvSnapshot::parse(&changed_auxv(tag, value), STACK, BRK).unwrap();
        assert_eq!(
            image
                .plan_at_original_base(&initial, BASE..BASE + 0x6000, &[])
                .unwrap_err(),
            PlanError::Overlap(name)
        );
    }
    let initial = AuxvSnapshot::parse(&encode(&auxv_entries()), STACK, BASE + 0x5000).unwrap();
    assert_eq!(
        image
            .plan_at_original_base(&initial, BASE..BASE + 0x6000, &[])
            .unwrap_err(),
        PlanError::Overlap("initial brk")
    );
    let mut entries = auxv_entries();
    for entry in &mut entries {
        if entry.0 == 25 || entry.0 == 31 {
            entry.1 = BASE + 0x5010;
        }
    }
    let initial =
        AuxvSnapshot::parse(&encode(&entries), BASE + 0x5000..BASE + 0x7000, BRK).unwrap();
    assert_eq!(
        image
            .plan_at_original_base(&initial, BASE..BASE + 0x6000, &[])
            .unwrap_err(),
        PlanError::Overlap("initial stack")
    );
}

#[test]
fn supplied_mappings_cannot_hide_in_gaps_or_tail_and_adjacency_is_allowed() {
    let bytes = interpreter();
    let image = InterpreterImage::parse(&bytes).unwrap();
    for mapping in [
        BASE..BASE + 1,
        BASE + 0x2000..BASE + 0x2001,
        BASE + 0x5fff..BASE + 0x6000,
        BASE - 1..BASE + 0x7000,
    ] {
        assert_eq!(
            image
                .plan_at_original_base(&initial(), BASE..BASE + 0x6000, &[mapping])
                .unwrap_err(),
            PlanError::Overlap("other mapping")
        );
    }
    for mapping in [
        0..1,
        1..1,
        Range { start: 2, end: 1 },
        USER_END..USER_END + 1,
    ] {
        assert_eq!(
            image
                .plan_at_original_base(&initial(), BASE..BASE + 0x6000, &[mapping])
                .unwrap_err(),
            PlanError::Invalid("other mapping")
        );
    }
    assert!(
        image
            .plan_at_original_base(
                &initial(),
                BASE..BASE + 0x6000,
                &[BASE - PAGE..BASE, BASE + 0x6000..BASE + 0x7000]
            )
            .is_ok()
    );
}
