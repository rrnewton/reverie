/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

use super::*;

fn put16(bytes: &mut [u8], at: usize, value: u16) {
    bytes[at..at + 2].copy_from_slice(&value.to_le_bytes());
}

fn put32(bytes: &mut [u8], at: usize, value: u32) {
    bytes[at..at + 4].copy_from_slice(&value.to_le_bytes());
}

fn put64(bytes: &mut [u8], at: usize, value: u64) {
    bytes[at..at + 8].copy_from_slice(&value.to_le_bytes());
}

fn elf_header(bytes: &mut [u8], kind: u16, count: u16) {
    bytes[..7].copy_from_slice(b"\x7fELF\x02\x01\x01");
    put16(bytes, 16, kind);
    put16(bytes, 18, header::EM_X86_64);
    put32(bytes, 20, 1);
    put64(bytes, 32, 64);
    put16(bytes, 52, 64);
    put16(bytes, 54, 56);
    put16(bytes, 56, count);
}

fn phdr(
    bytes: &mut [u8],
    index: usize,
    kind: u32,
    flags: u32,
    offset: u64,
    address: u64,
    size: u64,
) {
    let at = 64 + index * 56;
    put32(bytes, at, kind);
    put32(bytes, at + 4, flags);
    put64(bytes, at + 8, offset);
    put64(bytes, at + 16, address);
    put64(bytes, at + 32, size);
    put64(bytes, at + 40, size);
    put64(bytes, at + 48, 8);
}

fn runtime() -> Vec<u8> {
    let mut bytes = vec![0; 0x3000];
    elf_header(&mut bytes, header::ET_DYN, 4);
    phdr(&mut bytes, 0, ph::PT_LOAD, ph::PF_R, 0, 0, 0x1000);
    phdr(
        &mut bytes,
        1,
        ph::PT_LOAD,
        ph::PF_R | ph::PF_X,
        0x1000,
        0x1000,
        0x1000,
    );
    phdr(
        &mut bytes,
        2,
        ph::PT_LOAD,
        ph::PF_R | ph::PF_W,
        0x2000,
        0x2000,
        0x1000,
    );

    let strings = b"\0reverie_liteinst_initialize_host\0";
    bytes[0x500..0x500 + strings.len()].copy_from_slice(strings);
    put32(&mut bytes, 0x618, 1);
    bytes[0x61c] = (sym::STB_GLOBAL << 4) | sym::STT_FUNC;
    put16(&mut bytes, 0x61e, 1);
    put64(&mut bytes, 0x620, 0x1100);
    put64(&mut bytes, 0x628, 16);
    for (index, value) in [1, 2, 1, 0, 0].into_iter().enumerate() {
        put32(&mut bytes, 0x700 + index * 4, value);
    }
    put16(&mut bytes, 0x742, 1);
    bytes[0x1100..0x1110].fill(0x90);
    bytes[0x110f] = 0xc3;

    let tags = [
        (dynamic::DT_STRTAB, 0x500),
        (dynamic::DT_STRSZ, strings.len() as u64),
        (dynamic::DT_SYMTAB, 0x600),
        (dynamic::DT_SYMENT, 24),
        (dynamic::DT_HASH, 0x700),
        (dynamic::DT_VERSYM, 0x740),
        (dynamic::DT_NULL, 0),
    ];
    phdr(
        &mut bytes,
        3,
        ph::PT_DYNAMIC,
        ph::PF_R | ph::PF_W,
        0x2100,
        0x2100,
        (tags.len() * 16) as u64,
    );
    for (index, (tag, value)) in tags.into_iter().enumerate() {
        put64(&mut bytes, 0x2100 + index * 16, tag);
        put64(&mut bytes, 0x2108 + index * 16, value);
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
    phdr(bytes, index, ph::PT_LOAD, flags, offset, address, size);
}

fn resolve_synthetic_runtime(
    target_runtime: &[u8],
    expected_runtime: &[u8],
) -> io::Result<TargetDlopen> {
    let mut main = vec![0; 0x2000];
    elf_header(&mut main, header::ET_EXEC, 4);
    phdr(&mut main, 0, ph::PT_LOAD, ph::PF_R, 0, 0x400000, 0x1000);
    phdr(&mut main, 1, ph::PT_PHDR, ph::PF_R, 64, 0x400040, 4 * 56);
    phdr(
        &mut main,
        2,
        ph::PT_LOAD,
        ph::PF_R | ph::PF_W,
        0x1000,
        0x401000,
        0x1000,
    );
    phdr(
        &mut main,
        3,
        ph::PT_DYNAMIC,
        ph::PF_R | ph::PF_W,
        0x1000,
        0x401000,
        32,
    );
    put64(&mut main, 0x1000, dynamic::DT_DEBUG);
    put64(&mut main, 0x1008, 0x900000);

    let mut interpreter = vec![0; 0x1000];
    elf_header(&mut interpreter, header::ET_DYN, 0);
    let mut links = vec![0; 0x1000];
    put32(&mut links, 0, 1);
    put64(&mut links, 8, 0x900100);
    put64(&mut links, 16, 0x800100);
    put64(&mut links, 32, 0x800000);
    put64(&mut links, 0x110, 0x401000);
    put64(&mut links, 0x118, 0x900140);
    put64(&mut links, 0x140, 0x700000);
    put64(&mut links, 0x150, 0x702100);
    put64(&mut links, 0x160, 0x900100);

    let maps = parse_maps(
        b"400000-401000 r--p 0 00:01 1 /main\n\
          401000-402000 rw-p 1000 00:01 1 /main\n\
          700000-701000 r--p 0 00:02 2 /runtime\n\
          701000-702000 r-xp 1000 00:02 2 /runtime\n\
          702000-703000 rw-p 2000 00:02 2 /runtime\n\
          800000-801000 r-xp 0 00:03 3 /loader\n\
          900000-901000 rw-p 0 00:00 0\n",
    )?;
    let regions = [
        (0x400000, main),
        (0x700000, target_runtime.to_vec()),
        (0x800000, interpreter),
        (0x900000, links),
    ];
    let aux = BTreeMap::from([
        (libc::AT_PHDR, 0x400040),
        (libc::AT_PHNUM, 4),
        (libc::AT_PHENT, 56),
        (libc::AT_BASE, 0x800000),
    ]);
    let provider = parse_runtime(expected_runtime)?;
    let mut memory = Memory::new(&maps, |address, bytes: &mut [u8]| {
        for (base, region) in &regions {
            if address >= *base
                && address
                    .checked_add(bytes.len() as u64)
                    .is_some_and(|end| end <= base + region.len() as u64)
            {
                let offset = (address - base) as usize;
                bytes.copy_from_slice(&region[offset..offset + bytes.len()]);
                return Ok(());
            }
        }
        Err(invalid("synthetic target read outside region"))
    });
    let resolved = resolve(&mut memory, &aux, &provider)?;
    memory.recheck()?;
    Ok(resolved)
}

#[test]
fn accepts_exact_ordinary_unversioned_initializer_with_or_without_versym() {
    let bytes = runtime();
    let provider = parse_runtime(&bytes).unwrap();
    assert_eq!(provider.symbol.st_value, 0x1100);
    assert_eq!(provider.version, "unversioned");

    let mut without_versions = bytes;
    put64(&mut without_versions, 0x2150, dynamic::DT_NULL);
    assert!(parse_runtime(&without_versions).is_ok());
}

#[test]
fn ordinary_provider_composes_with_link_map_resolution_and_exact_bytes() {
    let target = runtime();
    assert_eq!(
        resolve_synthetic_runtime(&target, &target).unwrap(),
        TargetDlopen {
            tid: 0,
            start_ticks: 0,
            executable_phdr: 0x400040,
            link_map: 0x900140,
            load_bias: 0x700000,
            address: 0x701100,
            version: "unversioned".to_owned(),
            mapping_identity: (0, 2, 2),
        }
    );

    let mut mismatched = target.clone();
    mismatched[0x1100] ^= 1;
    assert!(resolve_synthetic_runtime(&target, &mismatched).is_err());
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

    let mut incompatible_permissions = compatible_shared_page;
    put32(
        &mut incompatible_permissions,
        64 + 5 * 56 + 4,
        ph::PF_R | ph::PF_X,
    );
    assert!(parse_runtime(&incompatible_permissions).is_err());
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

    // Unmapped padding models non-loadable file data. The small valid ELF
    // remains within the target read budget.
    let mut bytes = Vec::with_capacity(MAX_RUNTIME_FILE + 1);
    bytes.extend_from_slice(&runtime());
    bytes.resize(MAX_RUNTIME_FILE, 0);
    assert!(parse_runtime(&bytes).is_ok());
    bytes.push(0);
    assert!(parse_runtime(&bytes).is_err());
}
