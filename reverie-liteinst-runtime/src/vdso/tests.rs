use super::*;

fn put(bytes: &mut [u8], offset: usize, value: u64, width: usize) {
    bytes[offset..offset + width].copy_from_slice(&value.to_le_bytes()[..width]);
}

fn image() -> Vec<u8> {
    let mut bytes = vec![0; 4096];
    bytes[..7].copy_from_slice(b"\x7fELF\x02\x01\x01");
    for (offset, value, width) in [
        (16, 3, 2),
        (18, 62, 2),
        (20, 1, 4),
        (32, 64, 8),
        (52, 64, 2),
        (54, 56, 2),
        (56, 2, 2),
        (64, 1, 4),
        (68, 5, 4),
        (96, 4096, 8),
        (104, 4096, 8),
        (112, 4096, 8),
        (120, 2, 4),
        (124, 4, 4),
        (128, 256, 8),
        (136, 256, 8),
        (152, 144, 8),
        (160, 144, 8),
    ] {
        put(&mut bytes, offset, value, width);
    }
    for (index, (tag, value)) in [
        (4, 512),
        (5, 640),
        (6, 768),
        (10, 64),
        (11, 24),
        (0x6ffffff0, 896),
        (0x6ffffffc, 960),
        (0x6ffffffd, 1),
    ]
    .into_iter()
    .enumerate()
    {
        put(&mut bytes, 256 + index * 16, tag, 8);
        put(&mut bytes, 264 + index * 16, value, 8);
    }
    for (offset, value, width) in [
        (512, 1, 4),
        (516, 2, 4),
        (520, 1, 4),
        (792, 1, 4),
        (796, 0x12, 1),
        (798, 1, 2),
        (800, 2048, 8),
        (808, 32, 8),
        (898, 2, 2),
        (960, 1, 2),
        (964, 2, 2),
        (966, 1, 2),
        (968, 0x3ae75f6, 4),
        (972, 20, 4),
        (980, 16, 4),
    ] {
        put(&mut bytes, offset, value, width);
    }
    bytes[641..653].copy_from_slice(b"__vdso_time\0");
    bytes[656..666].copy_from_slice(b"LINUX_2.6\0");
    bytes
}

fn active() -> Active {
    let bytes = image();
    Active {
        owner: 0,
        kernel_mappings: Vec::new(),
        range: 0x10000..0x11000,
        vvar: vec![0xf000..0x10000; 1],
        exports: elf::exports(&bytes, 0x10000..0x11000).unwrap(),
        writable: vec![0x20000..0x21000; 1],
        changes: Vec::new(),
        _image: bytes,
    }
}

fn data_fault_records(owner: &Active, pc: u64, address: u64) -> Vec<(String, String, Option<i64>)> {
    let mut records = Vec::new();
    owner.report_data_fault_with(pc, address, |stage, field, value| {
        records.push((stage.into(), field.into(), value));
    });
    records
}

#[test]
fn data_fault_diagnostic_retained_byte_window_bounds_and_unavailable() {
    let mut owner = active();
    owner._image = (0..4096).map(|index| index as u8).collect();
    let before = owner._image.clone();
    for (pc, offset, length) in [
        (0x10000, 0, 15),
        (0x10021, 0x21, 15),
        (0x10ff1, 0xff1, 15),
        (0x10fff, 0xfff, 1),
    ] {
        let records = data_fault_records(&owner, pc, 0);
        let bytes: Vec<_> = records
            .iter()
            .filter(|record| record.0 == "vdso/retained-pc-byte")
            .map(|record| (record.1.as_str(), record.2))
            .collect();
        let expected: Vec<_> = before[offset..offset + length]
            .iter()
            .enumerate()
            .flat_map(|(index, byte)| {
                [
                    ("index", Some(index as i64)),
                    ("value", Some(i64::from(*byte))),
                ]
            })
            .collect();
        assert_eq!(bytes, expected);
        assert!(records.contains(&(
            "vdso/retained-pc-bytes".into(),
            "length".into(),
            Some(length as i64)
        )));
    }
    for pc in [0, 0xffff, 0x11000, u64::MAX] {
        let records = data_fault_records(&owner, pc, 0);
        assert!(records.contains(&(
            "vdso/retained-pc-bytes".into(),
            "pc-outside-image".into(),
            None
        )));
        assert!(
            !records
                .iter()
                .any(|record| record.0 == "vdso/retained-pc-byte")
        );
    }
    assert_eq!(owner._image, before);
    owner._image.truncate(1);
    let records = data_fault_records(&owner, 0x10000, 0);
    assert!(records.contains(&("vdso/retained-pc-bytes".into(), "length".into(), Some(1))));
    let records = data_fault_records(&owner, 0x10001, 0);
    assert!(records.contains(&(
        "vdso/retained-pc-bytes".into(),
        "image-bytes-unavailable".into(),
        None
    )));
    assert!(
        !records
            .iter()
            .any(|record| record.0 == "vdso/retained-pc-byte")
    );
    assert_eq!(owner._image, before[..1]);
}

#[test]
fn data_fault_diagnostic_interval_edges_high_bits_and_no_match() {
    let mut owner = active();
    owner.range = 0x1234_0001_0000..0x1234_0001_1000;
    owner.vvar = vec![0x1234_0000_e000..0x1234_0001_0000; 1];
    for (address, kind, start, end) in [
        (owner.range.start, "vdso", 0x10000, 0x11000),
        (owner.range.end - 1, "vdso", 0x10000, 0x11000),
        (owner.vvar[0].start, "vvar-family", 0xe000, 0x10000),
        (owner.vvar[0].end - 1, "vvar-family", 0xe000, 0x10000),
    ] {
        let records = data_fault_records(&owner, owner.range.start, address);
        let image: Vec<_> = records
            .iter()
            .filter(|record| record.0 == "vdso/retained-image")
            .map(|record| (record.1.as_str(), record.2))
            .collect();
        assert_eq!(
            image,
            [
                ("base-high32", Some(0x1234)),
                ("base-low32", Some(0x10000)),
                ("end-high32", Some(0x1234)),
                ("end-low32", Some(0x11000))
            ]
        );
        let interval: Vec<_> = records
            .iter()
            .filter(|record| record.0 == "vdso/fault-interval")
            .map(|record| (record.1.as_str(), record.2))
            .collect();
        assert_eq!(
            interval,
            [
                (kind, None),
                ("start-high32", Some(0x1234)),
                ("start-low32", Some(start)),
                ("end-high32", Some(0x1234)),
                ("end-low32", Some(end))
            ]
        );
    }
    for address in [0, owner.vvar[0].start - 1, owner.range.end, u64::MAX] {
        let records = data_fault_records(&owner, owner.range.start, address);
        let interval: Vec<_> = records
            .iter()
            .filter(|record| record.0 == "vdso/fault-interval")
            .map(|record| (record.1.as_str(), record.2))
            .collect();
        assert_eq!(interval, [("no-match", None)]);
    }
}

#[test]
fn data_fault_diagnostic_retained_aliases_versions_and_absence() {
    let mut owner = active();
    let records = data_fault_records(&owner, 0x10800, 0);
    assert!(records.contains(&("vdso/retained-getrandom".into(), "count".into(), Some(0))));
    assert!(records.contains(&("vdso/retained-getrandom".into(), "no-match".into(), None)));
    for (name, version) in [
        ("__vdso_getrandom", "LINUX_2.6"),
        ("getrandom", "UNKNOWN_VERSION"),
    ] {
        owner.exports.push(elf::Export {
            address: 0x1234_5678_1050,
            name: name.into(),
            version: version.into(),
            operation: None,
        });
    }
    let records = data_fault_records(&owner, 0x100be, 0xf008);
    let exports: Vec<_> = records
        .iter()
        .filter(|record| record.0.starts_with("vdso/retained-getrandom"))
        .map(|record| (record.0.as_str(), record.1.as_str(), record.2))
        .collect();
    assert_eq!(
        exports,
        [
            ("vdso/retained-getrandom", "index", Some(0)),
            ("vdso/retained-getrandom-name", "__vdso_getrandom", None),
            ("vdso/retained-getrandom-version", "LINUX_2.6", None),
            ("vdso/retained-getrandom", "entry-high32", Some(0x1234)),
            ("vdso/retained-getrandom", "entry-low32", Some(0x5678_1050)),
            ("vdso/retained-getrandom", "index", Some(1)),
            ("vdso/retained-getrandom-name", "getrandom", None),
            ("vdso/retained-getrandom-version", "UNKNOWN_VERSION", None),
            ("vdso/retained-getrandom", "entry-high32", Some(0x1234)),
            ("vdso/retained-getrandom", "entry-low32", Some(0x5678_1050)),
            ("vdso/retained-getrandom", "count", Some(2)),
        ]
    );
    assert_eq!(owner.operation(0x10800), Some(Operation::Time));
    assert!(owner.native_pc(0x100be));
    assert!(!owner.accessible(&(0xf000..0x10000)));
}

#[test]
fn owned_stepping_uses_actual_execute_fault_not_recognized_call_or_data_fault() {
    let active = active();
    let pc = active.range.start + 16;
    let mut registers = [0; 23];
    registers[libc::REG_RIP as usize] = pc as i64;
    registers[libc::REG_EFL as usize] = 0x10202;
    registers[libc::REG_TRAPNO as usize] = 14;
    registers[libc::REG_ERR as usize] = 0x15;
    let check = |registers: &[i64; 23], code, address| {
        active.native_entry(&Fault {
            signal: libc::SIGSEGV,
            code,
            address,
            registers,
        })
    };
    assert!(check(&registers, 2, pc));
    assert!(!check(&registers, 1, pc));
    assert!(!check(&registers, 2, pc + 1));
    for (field, value) in [
        (libc::REG_TRAPNO, 13),
        (libc::REG_ERR, 6),
        (libc::REG_EFL, 0x202),
        (libc::REG_RIP, 0x10800),
    ] {
        let mut changed = registers;
        changed[field as usize] = value;
        assert!(!check(&changed, 2, changed[libc::REG_RIP as usize] as u64));
    }
    assert!(!active.native_pc(0x10800));
    assert_eq!(active.operation(0x10800), Some(Operation::Time));
    assert!(!active.native_pc(active.range.end));
    assert!(!active.native_pc(active.range.start - 1));
    assert!(!active.accessible(&(active.range.start - 1..active.range.start)));
}

fn exclusion_records(active: &Active, pc: u64) -> Vec<(String, String, Option<i64>)> {
    let mut records = Vec::new();
    active.report_exclusion_with(pc, |stage, detail, value| {
        records.push((stage.to_owned(), detail.to_owned(), value));
    });
    records
}

#[test]
fn exclusion_diagnostic_interval_edges_and_no_match() {
    let mut active = active();
    active.vvar.push(0xd000..0xe000);
    for (pc, kind, start, end) in [
        (0xd000, "vvar", 0xd000, 0xe000),
        (0xdfff, "vvar", 0xd000, 0xe000),
        (0xf000, "vvar", 0xf000, 0x10000),
        (0xffff, "vvar", 0xf000, 0x10000),
        (0x10000, "vdso", 0x10000, 0x11000),
        (0x10fff, "vdso", 0x10000, 0x11000),
    ] {
        assert!(active.concerns(pc, pc));
        assert_eq!(
            exclusion_records(&active, pc),
            [
                ("step/vdso-interval", kind, None),
                ("step/vdso-interval", "start-high32", Some(0)),
                ("step/vdso-interval", "start-low32", Some(start)),
                ("step/vdso-interval", "end-high32", Some(0)),
                ("step/vdso-interval", "end-low32", Some(end)),
                ("step/vdso-interval", "offset-high32", Some(0)),
                (
                    "step/vdso-interval",
                    "offset-low32",
                    Some(pc as i64 - start)
                ),
                ("step/vdso-export", "match-count", Some(0)),
                ("step/vdso-export", "no-match", None),
            ]
            .map(|(stage, detail, value)| (stage.to_owned(), detail.to_owned(), value))
        );
    }
    for pc in [0xcfff, 0xe000, 0xefff, 0x11000] {
        assert!(!active.concerns(pc, pc));
        assert_eq!(
            exclusion_records(&active, pc),
            [
                ("step/vdso-interval", "no-match", None),
                ("step/vdso-export", "match-count", Some(0)),
                ("step/vdso-export", "no-match", None),
            ]
            .map(|(stage, detail, value)| (stage.to_owned(), detail.to_owned(), value))
        );
    }
}

#[test]
fn exclusion_diagnostic_all_exact_export_aliases_without_mutation() {
    let mut active = active();
    active.exports = vec![
        elf::Export {
            address: 0x10800,
            name: "__vdso_getrandom".into(),
            version: "LINUX_2.6".into(),
            operation: None,
        },
        elf::Export {
            address: 0x10800,
            name: "getrandom".into(),
            version: "LINUX_2.6".into(),
            operation: None,
        },
    ];
    let exports = active.exports.clone();
    let image = active._image.clone();
    let range = active.range.clone();
    let vvar = active.vvar.clone();
    let writable = active.writable.clone();
    let records = exclusion_records(&active, 0x10800);
    assert_eq!(
        &records[7..],
        &[
            ("step/vdso-export", "match-index", Some(0)),
            ("step/vdso-export-name", "__vdso_getrandom", Some(16)),
            ("step/vdso-export-version", "LINUX_2.6", Some(9)),
            ("step/vdso-export", "match-index", Some(1)),
            ("step/vdso-export-name", "getrandom", Some(9)),
            ("step/vdso-export-version", "LINUX_2.6", Some(9)),
            ("step/vdso-export", "match-count", Some(2)),
        ]
        .map(|(stage, detail, value)| (stage.to_owned(), detail.to_owned(), value))
    );
    assert_eq!(active.operation(0x10800), None);
    assert_eq!(active.exports, exports);
    assert_eq!(active._image, image);
    assert_eq!(active.range, range);
    assert_eq!(active.vvar, vvar);
    assert_eq!(active.writable, writable);
    assert!(active.changes.is_empty());
    assert_eq!(
        exclusion_records(&active, 0x10801).last().unwrap().1,
        "no-match"
    );
}

#[test]
fn exclusion_diagnostic_preserves_high_bits_and_supported_export_identity() {
    let mut active = active();
    active.range = 0x1234_0000_0000..0x1236_0000_0000;
    let pc = 0x1235_ffff_ffff;
    let records = exclusion_records(&active, pc);
    for (detail, value) in [
        ("start-high32", 0x1234),
        ("end-high32", 0x1236),
        ("offset-high32", 1),
        ("offset-low32", 0xffff_ffff),
    ] {
        assert!(records.contains(&("step/vdso-interval".into(), detail.into(), Some(value))));
    }
    let active = self::active();
    let records = exclusion_records(&active, 0x10800);
    assert!(records.contains(&(
        "step/vdso-export-name".into(),
        "__vdso_time".into(),
        Some(11)
    )));
    assert_eq!(active.operation(0x10800), Some(Operation::Time));
}

#[test]
fn exports_are_bound_to_version_and_mapping_not_elf_entry() {
    let bytes = image();
    let exports = elf::exports(&bytes, 0x10000..0x11000).unwrap();
    assert_eq!(exports.len(), 1);
    assert_eq!(exports[0].address, 0x10800);
    assert_eq!(exports[0].operation, Some(Operation::Time));
    let mut unknown = bytes.clone();
    unknown[664] = b'7';
    put(&mut unknown, 968, 0x3ae75f7, 4);
    assert_eq!(
        elf::exports(&unknown, 0x10000..0x11000).unwrap()[0].operation,
        None
    );
    let mut interior = bytes;
    put(&mut interior, 800, 4090, 8);
    assert!(elf::exports(&interior, 0x10000..0x11000).is_err());
}

#[test]
fn malformed_elf_and_dynamic_ownership_are_rejected() {
    for (offset, value, width) in [
        (4, 1, 1),
        (16, 2, 2),
        (18, 3, 2),
        (32, u64::MAX, 8),
        (56, 33, 2),
        (68, 7, 4),
        (80, 4096, 8),
        (104, 8192, 8),
        (112, 1, 8),
        (136, 257, 8),
        (160, 128, 8),
        (256, 7, 8),
        (272, 4, 8),
        (312, 0, 8),
        (520, 2, 4),
        (528, 1, 4),
        (796, 0x11, 1),
        (797, 3, 1),
        (798, 0xff01, 2),
        (808, 0, 8),
        (898, 0x8002, 2),
        (960, 2, 2),
        (968, 0, 4),
        (966, 2, 2),
        (972, u32::MAX as u64, 4),
        (976, 4, 4),
    ] {
        let mut bytes = image();
        put(&mut bytes, offset, value, width);
        assert!(
            elf::exports(&bytes, 0x10000..0x11000).is_err(),
            "offset {offset}"
        );
    }
}

#[test]
fn original_auxv_and_maps_require_unique_actual_kernel_mapping() {
    let mut auxv = vec![0; 32];
    put(&mut auxv, 0, 33, 8);
    put(&mut auxv, 8, 0x10000, 8);
    assert_eq!(auxv_vdso(&auxv).unwrap(), 0x10000);
    assert!(auxv_vdso(&auxv[..16]).is_err());
    put(&mut auxv, 16, 33, 8);
    assert!(auxv_vdso(&auxv).is_err());
    let text = "0000f000-00010000 r--p 00000000 00:00 0 [vvar]\n00010000-00011000 r-xp 00000000 00:00 0 [vdso]\n";
    let maps = mappings(text).unwrap();
    assert_eq!(selected_maps(&maps, 0x10000).unwrap().len(), 2);
    assert!(selected_maps(&maps, 0x10001).is_err());
    for (old, new) in [
        ("r-xp", "rwxp"),
        ("00:00 0 [vdso]", "00:01 7 [vdso]"),
        ("[vvar]", "[anon]"),
        ("00010000-00011000", "0000f000-00011000"),
    ] {
        let result =
            mappings(&text.replace(old, new)).and_then(|maps| selected_maps(&maps, 0x10000));
        assert!(result.is_err(), "{new}");
    }
}

#[test]
fn only_exact_execute_fault_and_supported_function_abi_are_calls() {
    let owner = active();
    let mut registers = [0; 23];
    registers[libc::REG_RIP as usize] = 0x10800;
    registers[libc::REG_RSP as usize] = 0x20008;
    registers[libc::REG_TRAPNO as usize] = 14;
    registers[libc::REG_ERR as usize] = 0x15;
    registers[libc::REG_EFL as usize] = 0x10202;
    let executable = [0x30000..0x31000, 0x10000..0x11000];
    let admit = |registers: &[libc::greg_t; 23], signal, code, address, target| {
        owner.call(
            Fault {
                signal,
                code,
                address,
                registers,
            },
            target,
            &executable,
        )
    };
    assert!(
        admit(
            &registers,
            libc::SIGSEGV,
            EXECUTE_ACCESS_ERROR,
            0x10800,
            0x30000
        )
        .is_some()
    );
    for (signal, code, address, target) in [
        (libc::SIGTRAP, 2, 0x10800, 0x30000),
        (libc::SIGSEGV, libc::SI_KERNEL, 0x10800, 0x30000),
        (libc::SIGSEGV, EXECUTE_ACCESS_ERROR, 0xf000, 0x30000),
        (libc::SIGSEGV, EXECUTE_ACCESS_ERROR, 0x10800, 0xf000),
        (libc::SIGSEGV, EXECUTE_ACCESS_ERROR, 0x10800, 0),
    ] {
        assert!(admit(&registers, signal, code, address, target).is_none());
    }
    for (index, value) in [
        (libc::REG_RIP, 0x10801),
        (libc::REG_TRAPNO, 13),
        (libc::REG_ERR, 4),
        (libc::REG_EFL, 0x202),
        (libc::REG_EFL, 0x30202),
        (libc::REG_RSP, 0x20000),
    ] {
        let mut changed = registers;
        changed[index as usize] = value;
        assert!(
            admit(
                &changed,
                libc::SIGSEGV,
                EXECUTE_ACCESS_ERROR,
                changed[libc::REG_RIP as usize] as u64,
                0x30000
            )
            .is_none()
        );
    }
    assert!(owner.concerns(0x30000, 0xf008));
    assert!(owner.concerns(0x10801, 0));
    assert!(!owner.accessible(&(0xf000..0x10000)));
}

#[test]
fn f1_recognized_call_returns_to_owned_text_armed_and_unarmed() {
    use liteinst2::trampoline::HookContext;
    use reverie_preload::precise_timer::Controller;
    let owner = active();
    let mut controller = Controller::default();
    let (generation, _) = controller.replace(40, 2, 0).unwrap();
    for ticket in [
        None,
        Some(crate::timer::Ticket {
            generation,
            sequence: 1,
        }),
    ] {
        let mut context: HookContext = unsafe { core::mem::zeroed() };
        context.instruction_pointer = 0x10800;
        context.stack_pointer = 0x20008;
        context.rflags = 0x202;
        context.rdi = 0x20080;
        let mut stepper = crate::owned_step::Stepper::default();
        if let Some(ticket) = ticket {
            stepper.stage_call((7, 5), ticket, &context, 40).unwrap();
        }
        let mut registers = [0; 23];
        registers[..16].copy_from_slice(
            &crate::owned_step::native::general(&context).map(|value| value as i64),
        );
        registers[libc::REG_RIP as usize] = context.instruction_pointer as i64;
        registers[libc::REG_RSP as usize] = context.stack_pointer as i64;
        registers[libc::REG_TRAPNO as usize] = 14;
        registers[libc::REG_ERR as usize] = 0x15;
        registers[libc::REG_EFL as usize] = 0x10202 | if ticket.is_some() { 0x100 } else { 0 };
        let original = registers;
        if ticket.is_some() {
            assert!(stepper.validate_call(7, 5, &registers).is_some());
            assert!(stepper.validate_call(8, 5, &registers).is_none());
            assert!(stepper.validate_call(7, 6, &registers).is_none());
            let mut invalid = registers;
            invalid[libc::REG_RDI as usize] ^= 1;
            assert!(stepper.validate_call(7, 5, &invalid).is_none());
        } else {
            assert!(!stepper.pending());
        }
        for target in [0x10010, 0x10800] {
            let fault = || Fault {
                signal: libc::SIGSEGV,
                code: EXECUTE_ACCESS_ERROR,
                address: 0x10800,
                registers: &registers,
            };
            assert!(
                owner
                    .call(fault(), target, &[0x30000..0x31000; 1])
                    .is_none()
            );
            let call = owner.call(fault(), target, core::slice::from_ref(&owner.range));
            assert!(
                call.is_some(),
                "F1 recognized-call retained return membership"
            );
            let call = call.unwrap();
            assert_eq!(call.target, target);
            assert_eq!(call.entry, 0x10800);
            assert_eq!(call.stack, 0x20008);
            assert_eq!(call.operation, Operation::Time);
            assert!(!owner.native_entry(&fault()));
        }
        for target in [0, 0xf000, 0x11000, 0x40000, 1 << 47, u64::MAX] {
            let fault = Fault {
                signal: libc::SIGSEGV,
                code: EXECUTE_ACCESS_ERROR,
                address: 0x10800,
                registers: &registers,
            };
            assert!(
                owner
                    .call(fault, target, core::slice::from_ref(&owner.range))
                    .is_none()
            );
        }
        assert_eq!(registers, original);
        if ticket.is_some() {
            stepper.cancel_call(40).unwrap();
        }
        assert!(!stepper.pending());
        assert_eq!(stepper.completed, 0);
    }
    assert!(!owner.native_pc(0x10800));
    assert!(owner.native_pc(0x10010));
}

#[test]
fn buffers_refuse_unsupported_memory_without_access_or_guest_errno() {
    let owner = active();
    let call = Call {
        entry: 0x10800,
        target: 0x30000,
        stack: 0x20008,
        operation: Operation::Time,
    };
    assert!(!owner.call_buffers_supported(call, [0x20008, 0, 0, 0, 0, 0]));
    assert!(!owner.call_buffers_supported(call, [0x20004, 0, 0, 0, 0, 0]));
    assert!(owner.call_buffers_supported(call, [0x20010, 0, 0, 0, 0, 0]));
    assert!(owner.buffers_supported(Operation::Time, [0; 6]));
    assert!(owner.buffers_supported(Operation::Time, [0x20ff8, 0, 0, 0, 0, 0]));
    assert!(!owner.buffers_supported(Operation::Time, [0x20ff9, 0, 0, 0, 0, 0]));
    assert!(!owner.buffers_supported(Operation::Gettimeofday, [0x20000, 0xf000, 0, 0, 0, 0]));
    assert!(!owner.buffers_supported(Operation::ClockGettime, [0, u64::MAX, 0, 0, 0, 0]));
    assert!(owner.buffers_supported(Operation::ClockGetres, [0, 0x20000, 0, 0, 0, 0]));
    assert!(owner.buffers_supported(Operation::Getcpu, [0x20000, 0x20ffc, 0, 0, 0, 0]));
}

fn changes() -> Vec<protection::Change> {
    vec![
        protection::Change {
            range: 0xf000..0x10000,
            before: 1,
            after: 0,
        },
        protection::Change {
            range: 0x10000..0x11000,
            before: 5,
            after: 1,
        },
    ]
}

#[test]
fn protection_transaction_preserves_failed_range_and_rollback_errors() {
    let changes = changes();
    let mut calls = Vec::new();
    protection::apply(&changes, |range, mode| {
        calls.push((range.clone(), mode));
        0
    })
    .unwrap();
    assert_eq!(calls, [(0xf000..0x10000, 0), (0x10000..0x11000, 1)]);
    calls.clear();
    let failure = protection::apply(&changes, |range, mode| {
        calls.push((range.clone(), mode));
        match calls.len() {
            2 => -libc::ENOMEM as i64,
            3 => -libc::EACCES as i64,
            _ => 0,
        }
    })
    .unwrap_err();
    assert_eq!(failure.cause, -libc::ENOMEM as i64);
    assert_eq!(failure.applied, 1);
    assert_eq!(
        failure.rollback,
        [
            (0x10000..0x11000, -libc::EACCES as i64),
            (0xf000..0x10000, 0)
        ]
    );
    assert_eq!(calls.len(), 4);
    let mut invalid = changes;
    invalid[1].range.start = 0xf000;
    assert!(protection::apply(&invalid, |_, _| panic!("effect on invalid plan")).is_err());
}

#[test]
fn anonymous_host_provider_changes_permissions_without_executing_mapping() {
    let pointer = unsafe {
        libc::mmap(
            std::ptr::null_mut(),
            8192,
            libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
            -1,
            0,
        )
    };
    assert_ne!(pointer, libc::MAP_FAILED);
    let begin = pointer as u64;
    unsafe {
        std::ptr::write_bytes(pointer, 0x5a, 8192);
    }
    assert_eq!(unsafe { libc::mprotect(pointer, 8192, libc::PROT_READ) }, 0);
    let plan = [
        protection::Change {
            range: begin..begin + 4096,
            before: 1,
            after: 0,
        },
        protection::Change {
            range: begin + 4096..begin + 8192,
            before: 1,
            after: 1,
        },
    ];
    protection::apply(&plan, |range, mode| unsafe {
        let result = libc::mprotect(
            range.start as *mut _,
            (range.end - range.start) as usize,
            mode,
        );
        if result == 0 {
            0
        } else {
            -i64::from(*libc::__errno_location())
        }
    })
    .unwrap();
    let maps = mappings(&std::fs::read_to_string("/proc/self/maps").unwrap()).unwrap();
    assert_eq!(
        maps.iter()
            .find(|map| map.range.contains(&begin))
            .unwrap()
            .protection,
        0
    );
    assert_eq!(unsafe { *((begin + 4096) as *const u8) }, 0x5a);
    assert_eq!(unsafe { libc::munmap(pointer, 8192) }, 0);
}

#[test]
fn current_host_vdso_is_read_only_input_not_activated() {
    let address = auxv_vdso(&std::fs::read("/proc/self/auxv").unwrap()).unwrap();
    let maps = selected_maps(
        &mappings(&std::fs::read_to_string("/proc/self/maps").unwrap()).unwrap(),
        address,
    )
    .unwrap();
    let range = maps
        .iter()
        .find(|map| map.name == "[vdso]")
        .unwrap()
        .range
        .clone();
    let bytes = read_image(&range).unwrap();
    let exports = elf::exports(&bytes, range).unwrap();
    assert!(
        exports
            .iter()
            .any(|export| export.operation == Some(Operation::ClockGettime))
    );
    assert!(PREPARED.get().is_none());
}
