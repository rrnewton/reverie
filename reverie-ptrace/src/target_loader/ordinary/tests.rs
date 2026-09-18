use super::*;

#[test]
fn isolated_runtime_projection_is_one_exact_no_access_page() {
    let isolated = TargetIsolatedRxPage::new(0x4000, 0x5000, 0x2000, (8, 1, 7)).unwrap();
    for invalid in [
        TargetIsolatedRxPage::new(0x4001, 0x5001, 0x2000, (8, 1, 7)),
        TargetIsolatedRxPage::new(0x4000, 0x6000, 0x2000, (8, 1, 7)),
        TargetIsolatedRxPage::new(0x4000, 0x5000, 0x2001, (8, 1, 7)),
        TargetIsolatedRxPage::new(0x4000, 0x5000, 0x2000, (8, 1, 0)),
    ] {
        assert!(invalid.is_none());
    }
    let exact = Map {
        start: 0x4000,
        end: 0x5000,
        offset: 0x2000,
        identity: (8, 1, 7),
        read: false,
        write: false,
        execute: false,
        private: true,
    };
    let mut maps = vec![exact.clone()];
    project_isolated_rx_page(&mut maps, isolated).unwrap();
    assert!(maps[0].read);
    assert!(!maps[0].write);
    assert!(maps[0].execute);
    assert!(maps[0].private);

    for mutate in 0..9 {
        let mut changed = exact.clone();
        match mutate {
            0 => changed.start += 0x1000,
            1 => changed.end += 0x1000,
            2 => changed.offset += 0x1000,
            3 => changed.identity.0 += 1,
            4 => changed.identity.2 += 1,
            5 => changed.read = true,
            6 => changed.write = true,
            7 => changed.execute = true,
            8 => changed.private = false,
            _ => unreachable!(),
        }
        let before = changed.clone();
        assert!(project_isolated_rx_page(std::slice::from_mut(&mut changed), isolated).is_err());
        assert_eq!(changed.start, before.start);
        assert_eq!(changed.end, before.end);
        assert_eq!(changed.offset, before.offset);
        assert_eq!(changed.identity, before.identity);
        assert_eq!(changed.read, before.read);
        assert_eq!(changed.write, before.write);
        assert_eq!(changed.execute, before.execute);
        assert_eq!(changed.private, before.private);
    }
    let mut duplicate = vec![exact.clone(), exact];
    assert!(project_isolated_rx_page(&mut duplicate, isolated).is_err());
    assert!(
        duplicate
            .iter()
            .all(|mapping| !mapping.read && !mapping.execute)
    );
}

fn put16(bytes: &mut [u8], at: usize, value: u16) {
    bytes[at..at + 2].copy_from_slice(&value.to_le_bytes());
}
fn put32(bytes: &mut [u8], at: usize, value: u32) {
    bytes[at..at + 4].copy_from_slice(&value.to_le_bytes());
}
fn put64(bytes: &mut [u8], at: usize, value: u64) {
    bytes[at..at + 8].copy_from_slice(&value.to_le_bytes());
}
fn runtime() -> Vec<u8> {
    let mut bytes = vec![0; 0x3000];
    bytes[..7].copy_from_slice(b"\x7fELF\x02\x01\x01");
    put16(&mut bytes, 16, header::ET_DYN);
    put16(&mut bytes, 18, header::EM_X86_64);
    put32(&mut bytes, 20, 1);
    put64(&mut bytes, 32, 64);
    put16(&mut bytes, 52, 64);
    put16(&mut bytes, 54, 56);
    put16(&mut bytes, 56, 4);
    for (i, (kind, flags, at, size)) in [
        (ph::PT_LOAD, ph::PF_R, 0, 0x1000),
        (ph::PT_LOAD, ph::PF_R | ph::PF_X, 0x1000, 0x1000),
        (ph::PT_LOAD, ph::PF_R | ph::PF_W, 0x2000, 0x1000),
        (ph::PT_DYNAMIC, ph::PF_R | ph::PF_W, 0x2100, 7 * 16),
    ]
    .into_iter()
    .enumerate()
    {
        let p = 64 + i * 56;
        put32(&mut bytes, p, kind);
        put32(&mut bytes, p + 4, flags);
        put64(&mut bytes, p + 8, at);
        put64(&mut bytes, p + 16, at);
        put64(&mut bytes, p + 32, size);
        put64(&mut bytes, p + 40, size);
        put64(&mut bytes, p + 48, 8);
    }
    let strings = b"\0reverie_liteinst_initialize_host\0";
    bytes[0x500..0x500 + strings.len()].copy_from_slice(strings);
    put32(&mut bytes, 0x618, 1);
    bytes[0x61c] = (sym::STB_GLOBAL << 4) | sym::STT_FUNC;
    put16(&mut bytes, 0x61e, 1);
    put64(&mut bytes, 0x620, 0x1100);
    put64(&mut bytes, 0x628, 16);
    for (i, value) in [1, 2, 1, 0, 0].into_iter().enumerate() {
        put32(&mut bytes, 0x700 + i * 4, value);
    }
    put16(&mut bytes, 0x742, 1);
    bytes[0x1100..0x1110].fill(0x90);
    bytes[0x110f] = 0xc3;
    for (i, (tag, value)) in [
        (dynamic::DT_STRTAB, 0x500),
        (dynamic::DT_STRSZ, strings.len() as u64),
        (dynamic::DT_SYMTAB, 0x600),
        (dynamic::DT_SYMENT, 24),
        (dynamic::DT_HASH, 0x700),
        (dynamic::DT_VERSYM, 0x740),
        (dynamic::DT_NULL, 0),
    ]
    .into_iter()
    .enumerate()
    {
        put64(&mut bytes, 0x2100 + i * 16, tag);
        put64(&mut bytes, 0x2108 + i * 16, value);
    }
    bytes
}

fn append_runtime_load(
    bytes: &mut [u8],
    index: usize,
    flags: u32,
    offset: u64,
    address: u64,
    size: u64,
) {
    put16(bytes, 56, (index + 1) as u16);
    let header = 64 + index * 56;
    put32(bytes, header, ph::PT_LOAD);
    put32(bytes, header + 4, flags);
    put64(bytes, header + 8, offset);
    put64(bytes, header + 16, address);
    put64(bytes, header + 32, size);
    put64(bytes, header + 40, size);
    put64(bytes, header + 48, 8);
}

#[test]
fn accepts_exact_ordinary_unversioned_initializer_with_or_without_versym() {
    let bytes = runtime();
    let provider = parse_runtime(&bytes).unwrap();
    assert_eq!(provider.symbol.st_value, 0x1100);
    assert_eq!(provider.version, "unversioned");
    let mut without_versions = bytes.clone();
    put64(&mut without_versions, 0x2150, dynamic::DT_NULL);
    assert!(parse_runtime(&without_versions).is_ok());
}

#[test]
fn runtime_parser_rejects_unused_unreadable_and_incompatible_page_loads() {
    let mut readable_extra = runtime();
    append_runtime_load(&mut readable_extra, 4, ph::PF_R, 0, 0x4000, 0x100);
    assert!(parse_runtime(&readable_extra).is_ok());

    for flags in [0, ph::PF_X] {
        let mut unreadable_extra = runtime();
        append_runtime_load(&mut unreadable_extra, 4, flags, 0, 0x4000, 0x100);
        assert!(parse_runtime(&unreadable_extra).is_err());
    }

    let mut compatible_shared_page = runtime();
    append_runtime_load(&mut compatible_shared_page, 4, ph::PF_R, 0, 0x4000, 0x800);
    append_runtime_load(
        &mut compatible_shared_page,
        5,
        ph::PF_R,
        0x800,
        0x4800,
        0x100,
    );
    assert!(parse_runtime(&compatible_shared_page).is_ok());

    let mut incompatible_projection = compatible_shared_page.clone();
    put64(&mut incompatible_projection, 64 + 5 * 56 + 8, 0x1800);
    assert!(parse_runtime(&incompatible_projection).is_err());

    let mut incompatible_shared_page = compatible_shared_page;
    put32(
        &mut incompatible_shared_page,
        64 + 5 * 56 + 4,
        ph::PF_R | ph::PF_X,
    );
    assert!(parse_runtime(&incompatible_shared_page).is_err());
}

#[test]
fn refuses_versioned_hidden_local_ifunc_object_weak_undefined_and_reserved_exports() {
    for (offset, value) in [
        (0x742, 2),
        (0x743, 0x80),
        (0x61c, (sym::STB_GLOBAL << 4) | sym::STT_GNU_IFUNC),
        (0x61c, (sym::STB_GLOBAL << 4) | sym::STT_OBJECT),
        (0x61c, (sym::STB_WEAK << 4) | sym::STT_FUNC),
        (0x61c, sym::STT_FUNC),
        (0x61d, sym::STV_HIDDEN),
        (0x61e, 0),
        (0x61f, 0xff),
    ] {
        let mut bytes = runtime();
        bytes[offset] = value;
        assert!(
            parse_runtime(&bytes).is_err(),
            "offset {offset:#x} value {value}"
        );
    }
}

#[test]
fn refuses_duplicate_named_initializer_even_when_both_are_ordinary() {
    let mut bytes = runtime();
    bytes.copy_within(0x618..0x630, 0x630);
    put32(&mut bytes, 0x704, 3);
    put16(&mut bytes, 0x744, 1);
    assert!(parse_runtime(&bytes).is_err());
}

#[test]
fn refuses_missing_initializer_and_invalid_extent_or_executable_mapping() {
    let mut absent = runtime();
    absent[0x501] = b'x';
    assert!(parse_runtime(&absent).is_err());
    for (at, value) in [
        (0x628, 0),
        (0x628, 1024 * 1024 + 1),
        (0x620, 0x2000),
        (0x620, u64::MAX - 1),
        (0x628, 0x1000),
    ] {
        let mut bytes = runtime();
        put64(&mut bytes, at, value);
        assert!(parse_runtime(&bytes).is_err());
    }
    let mut writable = runtime();
    put32(&mut writable, 64 + 56 + 4, ph::PF_R | ph::PF_W | ph::PF_X);
    assert!(parse_runtime(&writable).is_err());
}

#[test]
fn refuses_duplicate_dynamic_metadata_and_raw_symbol_metadata_outside_ro_load() {
    let mut duplicate = runtime();
    put64(&mut duplicate, 0x2150, dynamic::DT_SYMENT);
    assert!(parse_runtime(&duplicate).is_err());
    let mut writable = runtime();
    put64(&mut writable, 0x2128, 0x2400);
    assert!(parse_runtime(&writable).is_err());
    let mut wrong_width = runtime();
    put64(&mut wrong_width, 0x2138, 16);
    assert!(parse_runtime(&wrong_width).is_err());
    let mut textrel = runtime();
    put64(&mut textrel, 0x2150, dynamic::DT_TEXTREL);
    assert!(parse_runtime(&textrel).is_err());
}

#[test]
fn runtime_only_accepts_exact_64_mib_and_refuses_one_byte_over_without_changing_libc_bound() {
    assert_eq!(MAX_FILE, 32 * 1024 * 1024);
    assert!(runtime_file_bound(MAX_RUNTIME_FILE).is_ok());
    assert!(runtime_file_bound(MAX_RUNTIME_FILE + 1).is_err());
    // One allocation capped at 64 MiB + one byte. Unmapped padding represents
    // runtime debug data; all executable/load metadata is the small valid ELF.
    let mut bytes = Vec::with_capacity(MAX_RUNTIME_FILE + 1);
    bytes.extend_from_slice(&runtime());
    bytes.resize(MAX_RUNTIME_FILE, 0);
    assert!(parse_runtime(&bytes).is_ok());
    bytes.push(0);
    assert!(parse_runtime(&bytes).is_err());
}
