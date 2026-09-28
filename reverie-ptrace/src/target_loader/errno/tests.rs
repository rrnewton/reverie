use super::*;

fn put16(bytes: &mut [u8], at: usize, v: u16) {
    bytes[at..at + 2].copy_from_slice(&v.to_le_bytes());
}
fn put32(bytes: &mut [u8], at: usize, v: u32) {
    bytes[at..at + 4].copy_from_slice(&v.to_le_bytes());
}
fn put64(bytes: &mut [u8], at: usize, v: u64) {
    bytes[at..at + 8].copy_from_slice(&v.to_le_bytes());
}
fn libc_image() -> Vec<u8> {
    let mut b = vec![0; 0x3000];
    b[..7].copy_from_slice(b"\x7fELF\x02\x01\x01");
    put16(&mut b, 16, header::ET_DYN);
    put16(&mut b, 18, header::EM_X86_64);
    put32(&mut b, 20, 1);
    put64(&mut b, 32, 64);
    put16(&mut b, 52, 64);
    put16(&mut b, 54, 56);
    put16(&mut b, 56, 4);
    for (i, (kind, flags, at, size)) in [
        (ph::PT_LOAD, ph::PF_R, 0, 0x1000),
        (ph::PT_LOAD, ph::PF_R | ph::PF_X, 0x1000, 0x1000),
        (ph::PT_LOAD, ph::PF_R | ph::PF_W, 0x2000, 0x1000),
        (ph::PT_DYNAMIC, ph::PF_R | ph::PF_W, 0x2100, 10 * 16),
    ]
    .into_iter()
    .enumerate()
    {
        let p = 64 + i * 56;
        put32(&mut b, p, kind);
        put32(&mut b, p + 4, flags);
        put64(&mut b, p + 8, at);
        put64(&mut b, p + 16, at);
        put64(&mut b, p + 32, size);
        put64(&mut b, p + 40, size);
        put64(&mut b, p + 48, 8);
    }
    let names = b"\0libc.so.6\0dlopen\0GLIBC_2.34\0__errno_location\0GLIBC_2.2.5\0";
    b[0x500..0x500 + names.len()].copy_from_slice(names);
    for (i, (name, value)) in [(11, 0x1100), (29, 0x1120)].into_iter().enumerate() {
        let at = 0x618 + i * 24;
        put32(&mut b, at, name);
        b[at + 4] = (sym::STB_GLOBAL << 4) | sym::STT_FUNC;
        put16(&mut b, at + 6, 1);
        put64(&mut b, at + 8, value);
        put64(&mut b, at + 16, 16);
    }
    for (i, value) in [1, 3, 1, 0, 0, 0].into_iter().enumerate() {
        put32(&mut b, 0x700 + i * 4, value);
    }
    put16(&mut b, 0x742, 2);
    put16(&mut b, 0x744, 3);
    for (at, index, name, text, next) in [
        (0x780, 2, 18, b"GLIBC_2.34".as_slice(), 28),
        (0x79c, 3, 46, b"GLIBC_2.2.5".as_slice(), 0),
    ] {
        put16(&mut b, at, 1);
        put16(&mut b, at + 4, index);
        put16(&mut b, at + 6, 1);
        put32(&mut b, at + 8, elf_hash(text));
        put32(&mut b, at + 12, 20);
        put32(&mut b, at + 16, next);
        put32(&mut b, at + 20, name);
    }
    b[0x1100..0x1140].fill(0x90);
    b[0x110f] = 0xc3;
    b[0x112f] = 0xc3;
    for (i, (tag, value)) in [
        (dynamic::DT_STRTAB, 0x500),
        (dynamic::DT_STRSZ, names.len() as u64),
        (dynamic::DT_SYMTAB, 0x600),
        (dynamic::DT_SYMENT, 24),
        (dynamic::DT_HASH, 0x700),
        (dynamic::DT_VERSYM, 0x740),
        (dynamic::DT_VERDEF, 0x780),
        (dynamic::DT_VERDEFNUM, 2),
        (dynamic::DT_SONAME, 1),
        (dynamic::DT_NULL, 0),
    ]
    .into_iter()
    .enumerate()
    {
        put64(&mut b, 0x2100 + i * 16, tag);
        put64(&mut b, 0x2108 + i * 16, value);
    }
    b
}

#[test]
fn errno_accessor_requires_same_fully_validated_libc_with_both_public_exports() {
    let bytes = libc_image();
    let provider = parse_errno_provider(&bytes).unwrap();
    assert_eq!(provider.symbol.st_value, 0x1120);
    assert_eq!(provider.version, "GLIBC_2.2.5");
    let mut missing_dlopen = bytes.clone();
    missing_dlopen[0x50b] = b'x';
    assert!(parse_errno_provider(&missing_dlopen).is_err());
    let mut wrong_libc = bytes;
    wrong_libc[0x501] = b'x';
    assert!(parse_errno_provider(&wrong_libc).is_err());
}

#[test]
fn errno_refuses_ifunc_weak_hidden_undefined_wrong_or_hidden_version() {
    for (offset, value) in [
        (0x634, (sym::STB_GLOBAL << 4) | sym::STT_GNU_IFUNC),
        (0x634, (sym::STB_WEAK << 4) | sym::STT_FUNC),
        (0x635, sym::STV_HIDDEN),
        (0x636, 0),
        (0x744, 2),
        (0x745, 0x80),
    ] {
        let mut bytes = libc_image();
        bytes[offset] = value;
        assert!(parse_errno_provider(&bytes).is_err(), "offset {offset:#x}");
    }
}

#[test]
fn errno_refuses_duplicate_provider_symbol_and_executable_extent_change() {
    let mut bytes = libc_image();
    bytes.copy_within(0x630..0x648, 0x648);
    put32(&mut bytes, 0x704, 4);
    put16(&mut bytes, 0x746, 3);
    assert!(parse_errno_provider(&bytes).is_err());
    let mut bytes = libc_image();
    put64(&mut bytes, 0x638, 0x2000);
    assert!(parse_errno_provider(&bytes).is_err());
    let mut bytes = libc_image();
    put64(&mut bytes, 0x640, 129);
    assert!(parse_errno_provider(&bytes).is_err());
}
