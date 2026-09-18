use std::os::unix::ffi::OsStringExt;

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
    file_size: u64,
    memory_size: u64,
) {
    let at = 64 + index * 56;
    put32(bytes, at, kind);
    put32(bytes, at + 4, flags);
    put64(bytes, at + 8, offset);
    put64(bytes, at + 16, address);
    put64(bytes, at + 32, file_size);
    put64(bytes, at + 40, memory_size);
    put64(bytes, at + 48, 4096);
}

fn string_offset(strings: &[u8], name: &[u8]) -> u32 {
    strings
        .windows(name.len() + 1)
        .position(|window| &window[..name.len()] == name && window[name.len()] == 0)
        .unwrap() as u32
}

fn symbol(
    bytes: &mut [u8],
    index: usize,
    name: u32,
    binding: u8,
    kind: u8,
    section: u16,
    value: u64,
    size: u64,
) {
    let at = 0x500 + index * 24;
    put32(bytes, at, name);
    bytes[at + 4] = (binding << 4) | kind;
    bytes[at + 5] = sym::STV_DEFAULT;
    put16(bytes, at + 6, section);
    put64(bytes, at + 8, value);
    put64(bytes, at + 16, size);
}

fn version_definition(bytes: &mut [u8], at: usize, index: u16, name: u32, text: &[u8], next: u32) {
    put16(bytes, at, 1);
    put16(bytes, at + 4, index);
    put16(bytes, at + 6, 1);
    put32(bytes, at + 8, elf_hash(text));
    put32(bytes, at + 12, 20);
    put32(bytes, at + 16, next);
    put32(bytes, at + 20, name);
}

fn libc_image() -> Vec<u8> {
    let mut bytes = vec![0; 0x4000];
    elf_header(&mut bytes, header::ET_DYN, 4);
    phdr(&mut bytes, 0, ph::PT_LOAD, ph::PF_R, 0, 0, 0x1000, 0x1000);
    phdr(
        &mut bytes,
        1,
        ph::PT_LOAD,
        ph::PF_R | ph::PF_X,
        0x1000,
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
        0x400,
        0x2000,
    );

    let strings =
        b"\0libc.so.6\0dlopen\0GLIBC_2.34\0GLIBC_2.2.5\0environ\0_environ\0__environ\0getenv\0";
    bytes[0x400..0x400 + strings.len()].copy_from_slice(strings);
    symbol(
        &mut bytes,
        1,
        string_offset(strings, b"dlopen"),
        sym::STB_GLOBAL,
        sym::STT_FUNC,
        1,
        0x1100,
        16,
    );
    symbol(
        &mut bytes,
        2,
        string_offset(strings, b"environ"),
        sym::STB_WEAK,
        sym::STT_OBJECT,
        2,
        0x3800,
        8,
    );
    symbol(
        &mut bytes,
        3,
        string_offset(strings, b"_environ"),
        sym::STB_WEAK,
        sym::STT_OBJECT,
        2,
        0x3800,
        8,
    );
    symbol(
        &mut bytes,
        4,
        string_offset(strings, b"__environ"),
        sym::STB_GLOBAL,
        sym::STT_OBJECT,
        2,
        0x3800,
        8,
    );
    symbol(
        &mut bytes,
        5,
        string_offset(strings, b"getenv"),
        sym::STB_GLOBAL,
        sym::STT_FUNC,
        1,
        0x1140,
        11,
    );

    // SysV hash publishes the exact six-entry dynamic symbol count.
    for (index, value) in [1, 6, 1, 0, 0, 0, 0, 0, 0].into_iter().enumerate() {
        put32(&mut bytes, 0x600 + index * 4, value);
    }
    for (index, version) in [0, 2, 3, 3, 3, 3].into_iter().enumerate() {
        put16(&mut bytes, 0x680 + index * 2, version);
    }
    version_definition(
        &mut bytes,
        0x700,
        2,
        string_offset(strings, b"GLIBC_2.34"),
        b"GLIBC_2.34",
        28,
    );
    version_definition(
        &mut bytes,
        0x71c,
        3,
        string_offset(strings, b"GLIBC_2.2.5"),
        b"GLIBC_2.2.5",
        0,
    );

    // The only environment relocation is the GOT word read by getenv.
    put64(&mut bytes, 0x780, 0x2200);
    put64(
        &mut bytes,
        0x788,
        ((4_u64) << 32) | u64::from(reloc::R_X86_64_GLOB_DAT),
    );
    put64(&mut bytes, 0x790, 0);
    // Exercise the target observer's REL and PLT-table dispatch with unrelated
    // symbol-zero entries; the environment relocation above remains RELA.
    put64(&mut bytes, 0x7a0, 0x2210);
    put64(&mut bytes, 0x7a8, 0);
    put64(&mut bytes, 0x7c0, 0x2220);
    put64(&mut bytes, 0x7c8, 0);
    put64(&mut bytes, 0x7d0, 0);

    bytes[0x1100..0x1110].fill(0x90);
    bytes[0x110f] = 0xc3;
    // mov rax,[rip + GOT]; mov r12,[rax]; ret
    bytes[0x1140..0x114b]
        .copy_from_slice(&[0x48, 0x8b, 0x05, 0xb9, 0x10, 0, 0, 0x4c, 0x8b, 0x20, 0xc3]);

    let tags = [
        (dynamic::DT_STRTAB, 0x400),
        (dynamic::DT_STRSZ, strings.len() as u64),
        (dynamic::DT_SYMTAB, 0x500),
        (dynamic::DT_SYMENT, 24),
        (dynamic::DT_HASH, 0x600),
        (dynamic::DT_VERSYM, 0x680),
        (dynamic::DT_VERDEF, 0x700),
        (dynamic::DT_VERDEFNUM, 2),
        (
            dynamic::DT_SONAME,
            u64::from(string_offset(strings, b"libc.so.6")),
        ),
        (dynamic::DT_RELA, 0x780),
        (dynamic::DT_RELASZ, 24),
        (dynamic::DT_RELAENT, 24),
        (dynamic::DT_REL, 0x7a0),
        (dynamic::DT_RELSZ, 16),
        (dynamic::DT_RELENT, 16),
        (dynamic::DT_JMPREL, 0x7c0),
        (dynamic::DT_PLTRELSZ, 24),
        (dynamic::DT_PLTREL, dynamic::DT_RELA),
        (dynamic::DT_NULL, 0),
    ];
    phdr(
        &mut bytes,
        3,
        ph::PT_DYNAMIC,
        ph::PF_R | ph::PF_W,
        0x2000,
        0x2000,
        (tags.len() * 16) as u64,
        (tags.len() * 16) as u64,
    );
    for (index, (tag, value)) in tags.into_iter().enumerate() {
        put64(&mut bytes, 0x2000 + index * 16, tag);
        put64(&mut bytes, 0x2008 + index * 16, value);
    }
    bytes
}

#[derive(Clone)]
struct Target {
    expected_libc: Vec<u8>,
    regions: Vec<(u64, Vec<u8>)>,
    maps: Vec<Map>,
    aux: BTreeMap<u64, u64>,
}

impl Target {
    fn new() -> Self {
        let expected_libc = libc_image();
        let mut loaded_libc = expected_libc.clone();
        put64(&mut loaded_libc, 0x2200, 0x703800);
        put64(&mut loaded_libc, 0x3800, 0x900400);

        let mut main = vec![0; 0x2000];
        elf_header(&mut main, header::ET_EXEC, 4);
        phdr(
            &mut main,
            0,
            ph::PT_LOAD,
            ph::PF_R,
            0,
            0x400000,
            0x1000,
            0x1000,
        );
        phdr(
            &mut main,
            1,
            ph::PT_PHDR,
            ph::PF_R,
            64,
            0x400040,
            4 * 56,
            4 * 56,
        );
        phdr(
            &mut main,
            2,
            ph::PT_LOAD,
            ph::PF_R | ph::PF_W,
            0x1000,
            0x401000,
            0x1000,
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
        put64(&mut links, 0x150, 0x702000);
        put64(&mut links, 0x160, 0x900100);
        put64(&mut links, 0x400, 0x900500);
        put64(&mut links, 0x408, 0x900506);
        put64(&mut links, 0x410, 0);
        links[0x500..0x50c].copy_from_slice(b"A=one\0B=two\0");

        let maps = parse_maps(
            b"400000-401000 r--p 00000000 00:01 1 /main\n\
              401000-402000 r--p 00001000 00:01 1 /main\n\
              700000-701000 r--p 00000000 00:02 2 /libc\n\
              701000-702000 r-xp 00001000 00:02 2 /libc\n\
              702000-703000 r--p 00002000 00:02 2 /libc\n\
              703000-704000 rw-p 00000000 00:00 0\n\
              800000-801000 r-xp 00000000 00:03 3 /loader\n\
              900000-901000 rw-p 00000000 00:00 0\n",
        )
        .unwrap();
        Self {
            expected_libc,
            regions: vec![
                (0x400000, main),
                (0x700000, loaded_libc),
                (0x800000, interpreter),
                (0x900000, links),
            ],
            maps,
            aux: BTreeMap::from([
                (libc::AT_PHDR, 0x400040),
                (libc::AT_PHNUM, 4),
                (libc::AT_PHENT, 56),
                (libc::AT_BASE, 0x800000),
            ]),
        }
    }

    fn read(&self, address: u64, bytes: &mut [u8]) -> io::Result<()> {
        for (base, region) in &self.regions {
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
        Err(invalid("synthetic environment read outside target"))
    }

    fn write(&mut self, address: u64, bytes: &[u8]) {
        for (base, region) in &mut self.regions {
            if address >= *base
                && address
                    .checked_add(bytes.len() as u64)
                    .is_some_and(|end| end <= *base + region.len() as u64)
            {
                let offset = (address - *base) as usize;
                region[offset..offset + bytes.len()].copy_from_slice(bytes);
                return;
            }
        }
        panic!("synthetic environment write outside target");
    }

    fn put64(&mut self, address: u64, value: u64) {
        self.write(address, &value.to_le_bytes());
    }

    fn relocate_libc_dynamic_pointers(&mut self, selected: &[u64]) {
        for index in 0..MAX_DYNAMIC {
            let address = 0x702000 + (index * 16) as u64;
            let mut entry = [0; 16];
            self.read(address, &mut entry).unwrap();
            let tag = u64_at(&entry, 0);
            if tag == dynamic::DT_NULL {
                return;
            }
            if selected.contains(&tag) {
                self.put64(address + 8, 0x700000 + u64_at(&entry, 8));
            }
        }
        panic!("synthetic libc dynamic table is unterminated");
    }

    fn add_libc_duplicate_instance(&mut self) {
        let bytes = self
            .regions
            .iter()
            .find(|(base, _)| *base == 0x700000)
            .unwrap()
            .1
            .clone();
        self.regions.push((0x780000, bytes));
        self.maps.extend(
            parse_maps(
                b"780000-781000 r--p 00000000 00:02 2 /libc-duplicate\n\
                  781000-782000 r-xp 00001000 00:02 2 /libc-duplicate\n\
                  782000-783000 r--p 00002000 00:02 2 /libc-duplicate\n",
            )
            .unwrap(),
        );
        self.maps.sort_by_key(|mapping| mapping.start);
    }

    fn point_libc_dynamic_pointers_at_duplicate(&mut self, rebased: bool) {
        for index in 0..MAX_DYNAMIC {
            let address = 0x702000 + (index * 16) as u64;
            let mut entry = [0; 16];
            self.read(address, &mut entry).unwrap();
            let tag = u64_at(&entry, 0);
            if tag == dynamic::DT_NULL {
                return;
            }
            if [
                dynamic::DT_STRTAB,
                dynamic::DT_SYMTAB,
                dynamic::DT_RELA,
                dynamic::DT_REL,
                dynamic::DT_JMPREL,
            ]
            .contains(&tag)
            {
                let duplicate_address = 0x780000 + u64_at(&entry, 8);
                self.put64(
                    address + 8,
                    if rebased {
                        duplicate_address - 0x700000
                    } else {
                        duplicate_address
                    },
                );
            }
        }
        panic!("synthetic libc dynamic table is unterminated");
    }

    fn add_main_environment_relocation(&mut self, name: &str, kind: u32, value: u64) {
        const SYMBOL_INDEX: u64 = 1;
        let strings = [b"\0".as_slice(), name.as_bytes(), b"\0".as_slice()].concat();
        self.write(0x401200, &strings);
        let mut symbol = [0_u8; 24];
        put32(&mut symbol, 0, 1);
        symbol[4] = (sym::STB_WEAK << 4) | sym::STT_OBJECT;
        self.write(0x401300 + SYMBOL_INDEX * 24, &symbol);
        let mut relocation = [0_u8; 24];
        put64(&mut relocation, 0, 0x401500);
        put64(&mut relocation, 8, (SYMBOL_INDEX << 32) | u64::from(kind));
        self.write(0x401400, &relocation);
        self.put64(0x401500, value);
        let tags = [
            (dynamic::DT_DEBUG, 0x900000),
            (dynamic::DT_STRTAB, 0x401200),
            (dynamic::DT_STRSZ, strings.len() as u64),
            (dynamic::DT_SYMTAB, 0x401300),
            (dynamic::DT_SYMENT, 24),
            (dynamic::DT_RELA, 0x401400),
            (dynamic::DT_RELASZ, 24),
            (dynamic::DT_RELAENT, 24),
            (dynamic::DT_NULL, 0),
        ];
        for (index, (tag, tag_value)) in tags.into_iter().enumerate() {
            self.put64(0x401000 + (index * 16) as u64, tag);
            self.put64(0x401008 + (index * 16) as u64, tag_value);
        }
        let size = (tags.len() * 16) as u64;
        self.put64(0x400000 + (64 + 3 * 56 + 32) as u64, size);
        self.put64(0x400000 + (64 + 3 * 56 + 40) as u64, size);
    }

    fn make_main_pie(&mut self, relative_dynamic_pointers: bool) {
        self.write(0x400010, &header::ET_DYN.to_le_bytes());
        for (index, vaddr) in [(0, 0), (1, 0x40), (2, 0x1000), (3, 0x1000)] {
            self.put64(0x400000 + (64 + index * 56 + 16) as u64, vaddr);
        }
        self.put64(0x900100, 0x400000);
        self.put64(0x401400, 0x1500);
        if relative_dynamic_pointers {
            for index in 0..MAX_DYNAMIC {
                let address = 0x401000 + (index * 16) as u64;
                let mut entry = [0; 16];
                self.read(address, &mut entry).unwrap();
                let tag = u64_at(&entry, 0);
                if tag == dynamic::DT_NULL {
                    return;
                }
                if [dynamic::DT_STRTAB, dynamic::DT_SYMTAB, dynamic::DT_RELA].contains(&tag) {
                    self.put64(address + 8, u64_at(&entry, 8) - 0x400000);
                }
            }
            panic!("synthetic PIE dynamic table is unterminated");
        }
    }

    fn graph(&self, expected: &BTreeMap<OsString, OsString>) -> io::Result<TargetEnvironmentGraph> {
        let environment = EnvironmentProvider::parse(&self.expected_libc)?;
        let expected = expected_environment(expected)?;
        let mut memory = Memory::new(&self.maps, |address, bytes: &mut [u8]| {
            self.read(address, bytes)
        });
        let graph =
            observe_environment_inner(&mut memory, &self.aux, &environment, 101, 202, &expected)?;
        memory.recheck()?;
        Ok(graph)
    }
}

fn expected(entries: &[(&str, &str)]) -> BTreeMap<OsString, OsString> {
    entries
        .iter()
        .map(|(key, value)| (OsString::from(*key), OsString::from(*value)))
        .collect()
}

#[test]
fn resolves_exact_getenv_got_aliases_bss_and_pointer_graph() {
    let target = Target::new();
    let graph = target
        .graph(&expected(&[("A", "one"), ("B", "two")]))
        .unwrap();
    assert_eq!(graph.libc_load_bias, 0x700000);
    assert_eq!(graph.getenv_address, 0x701140);
    assert_eq!(graph.getenv_got.slot, 0x702200);
    assert_eq!(graph.getenv_got.value, 0x703800);
    assert_eq!(
        graph
            .aliases
            .iter()
            .map(|alias| alias.address)
            .collect::<BTreeSet<_>>(),
        BTreeSet::from([0x703800])
    );
    assert_eq!(graph.writable_load.mapped_file_end, 0x703000);
    assert_eq!(graph.writable_load.mapped_memory_end, 0x704000);
    assert_eq!(graph.environ_object.value, 0x900400);
    assert_eq!(
        graph
            .entries
            .iter()
            .map(|entry| entry.string_address)
            .collect::<Vec<_>>(),
        vec![0x900500, 0x900506]
    );
    assert_eq!(graph.pointer_array_bytes.len(), 24);
    assert_eq!(graph.bookkeeping, None);
}

#[test]
fn runtime_dynamic_pointers_accept_one_consistent_relative_or_absolute_form() {
    const POINTER_TAGS: &[u64] = &[
        dynamic::DT_STRTAB,
        dynamic::DT_SYMTAB,
        dynamic::DT_RELA,
        dynamic::DT_REL,
        dynamic::DT_JMPREL,
    ];
    let expected = expected(&[("A", "one"), ("B", "two")]);

    let relative = Target::new().graph(&expected).unwrap();
    let mut absolute_target = Target::new();
    absolute_target.relocate_libc_dynamic_pointers(POINTER_TAGS);
    let absolute = absolute_target.graph(&expected).unwrap();
    assert_eq!(relative, absolute);

    let mut mixed = Target::new();
    mixed.relocate_libc_dynamic_pointers(&[dynamic::DT_STRTAB]);
    assert!(mixed.graph(&expected).is_err());

    for tag in [dynamic::DT_REL, dynamic::DT_JMPREL] {
        let mut mixed_table = Target::new();
        mixed_table.relocate_libc_dynamic_pointers(&[tag]);
        let error = mixed_table.graph(&expected).unwrap_err().to_string();
        assert!(
            error.contains("mixes runtime dynamic pointer representations"),
            "tag={tag} error={error}",
        );
    }

    let mut ambiguous = Target::new();
    ambiguous.relocate_libc_dynamic_pointers(POINTER_TAGS);
    ambiguous.maps.push(Map {
        start: 0xe00000,
        end: 0xe01000,
        offset: 0,
        identity: (0, 2, 2),
        read: true,
        write: false,
        execute: false,
        private: true,
    });
    assert!(ambiguous.graph(&expected).is_err());
}

#[test]
fn nonzero_bias_pie_preserves_dynamic_pointer_modes_and_relative_relocation_offsets() {
    let make_graph = |relative_dynamic_pointers| {
        let mut target = Target::new();
        target.add_main_environment_relocation("environ", reloc::R_X86_64_GLOB_DAT, 0x703800);
        target.make_main_pie(relative_dynamic_pointers);
        target
            .graph(&expected(&[("A", "one"), ("B", "two")]))
            .unwrap()
    };
    let direct = make_graph(false);
    let rebased = make_graph(true);
    assert_eq!(direct, rebased);
    let imports = direct
        .relocations
        .iter()
        .filter(|relocation| relocation.link_map == 0x900100)
        .collect::<Vec<_>>();
    assert_eq!(imports.len(), 1, "{imports:#?}");
    assert_eq!(imports[0].load_bias, 0x400000);
    assert_eq!(imports[0].dynamic, 0x401000);
    assert_eq!(imports[0].slot, 0x401500);
    assert_eq!(imports[0].value, 0x703800);
}

#[test]
fn dynamic_image_parser_rejects_duplicate_instances_bad_offsets_and_unbounded_tables() {
    let expected = expected(&[("A", "one"), ("B", "two")]);
    let baseline = Target::new().graph(&expected).unwrap();

    let mut harmless_duplicate = Target::new();
    harmless_duplicate.add_libc_duplicate_instance();
    assert_eq!(harmless_duplicate.graph(&expected).unwrap(), baseline);
    let mut duplicate_memory =
        Memory::new(&harmless_duplicate.maps, |address, bytes: &mut [u8]| {
            harmless_duplicate.read(address, bytes)
        });
    let duplicate_node = LinkNode {
        address: 0x910000,
        bias: 0x780000,
        dynamic: 0x782000,
        next: 0,
        previous: 0,
    };
    let duplicate_image = dynamic_image(&mut duplicate_memory, duplicate_node, None).unwrap();
    assert_eq!(
        resolve_dynamic_pointer(
            &duplicate_memory,
            &duplicate_image,
            &mut None,
            "duplicate-instance-control",
            0x400,
            16,
        )
        .unwrap(),
        0x780400
    );

    for rebased in [false, true] {
        let mut decoy = Target::new();
        decoy.add_libc_duplicate_instance();
        decoy.point_libc_dynamic_pointers_at_duplicate(rebased);
        let error = decoy.graph(&expected).unwrap_err().to_string();
        assert!(
            error.contains("no unique authenticated runtime pointer"),
            "rebased={rebased} error={error}",
        );
    }

    let mut split_relro = Target::new();
    let mut second = split_relro.maps[4].clone();
    split_relro.maps[4].end = 0x702100;
    second.start = 0x702100;
    second.offset = 0x2100;
    split_relro.maps.insert(5, second);
    split_relro.graph(&expected).unwrap();

    let mut wrong_offset = Target::new();
    wrong_offset.maps[4].offset = 0x3000;
    let mut wrong_offset_memory = Memory::new(&wrong_offset.maps, |address, bytes: &mut [u8]| {
        wrong_offset.read(address, bytes)
    });
    let node = link_nodes(&mut wrong_offset_memory, 0x900140).unwrap()[1];
    let error = dynamic_image(&mut wrong_offset_memory, node, None)
        .unwrap_err()
        .to_string();
    assert!(error.contains("no unique ELF load instance"), "{error}");

    let mut unterminated = Target::new();
    unterminated.put64(0x702120, dynamic::DT_NEEDED);
    let error = unterminated.graph(&expected).unwrap_err().to_string();
    assert!(error.contains("lacks bounded terminator"), "{error}");

    let mut foreign_slot = Target::new();
    foreign_slot.add_main_environment_relocation("environ", reloc::R_X86_64_GLOB_DAT, 0x703800);
    foreign_slot.add_libc_duplicate_instance();
    foreign_slot.put64(0x401400, 0x782200);
    let error = foreign_slot.graph(&expected).unwrap_err().to_string();
    assert!(
        error.contains("relocation slot is outside writable PT_LOAD file bytes"),
        "{error}",
    );
}

#[test]
fn vdso_dynamic_image_is_bound_to_auxv_elf_pt_loads_and_exact_vma() {
    let bytes = libc_image();
    let maps = parse_maps(b"100000-104000 r-xp 00000000 00:00 0 [vdso]\n").unwrap();
    let node = LinkNode {
        address: 0x900000,
        bias: 0x100000,
        dynamic: 0x102000,
        next: 0,
        previous: 0,
    };
    let read = |address: u64, output: &mut [u8]| {
        let start = usize::try_from(address - 0x100000).unwrap();
        output.copy_from_slice(&bytes[start..start + output.len()]);
        Ok(())
    };
    let mut memory = Memory::new(&maps, read);
    let image = dynamic_image(&mut memory, node, Some(0x100000)).unwrap();
    assert_eq!(image.dynamic_table_end, 0x102130);
    assert_eq!(
        resolve_dynamic_pointer(&memory, &image, &mut None, "vdso-parser-control", 0x400, 16,)
            .unwrap(),
        0x100400
    );

    let mut missing_aux = Memory::new(&maps, read);
    assert!(dynamic_image(&mut missing_aux, node, None).is_err());
    let mut wrong_aux = Memory::new(&maps, read);
    assert!(dynamic_image(&mut wrong_aux, node, Some(0x100100)).is_err());
}

#[test]
fn runtime_dynamic_pointer_resolution_is_unique_same_image_and_range_complete() {
    let node = LinkNode {
        address: 0x900000,
        bias: 0x100000,
        dynamic: 0x100100,
        next: 0,
        previous: 0,
    };
    let maps = parse_maps(b"100000-101000 r--p 00000000 00:01 1 /image\n").unwrap();
    let memory = Memory::new(&maps, |_address, bytes: &mut [u8]| {
        bytes.fill(0);
        Ok(())
    });
    let image = DynamicImage {
        node,
        identity: (0, 1, 1),
        dynamic_mapping_start: 0x100000,
        dynamic_mapping_end: 0x101000,
        dynamic_table_end: 0x101000,
        file_loads: vec![DynamicFileLoad {
            start: 0x100000,
            end: 0x101000,
            offset: 0,
            read: true,
            write: false,
        }],
    };

    let mut direct_mode = None;
    assert_eq!(
        resolve_dynamic_pointer(
            &memory,
            &image,
            &mut direct_mode,
            "direct-control",
            0x100800,
            16,
        )
        .unwrap(),
        0x100800
    );
    assert_eq!(direct_mode, Some(DynamicPointerMode::Direct));

    let mut rebased_mode = None;
    assert_eq!(
        resolve_dynamic_pointer(
            &memory,
            &image,
            &mut rebased_mode,
            "rebased-control",
            0x800,
            16,
        )
        .unwrap(),
        0x100800
    );
    assert_eq!(rebased_mode, Some(DynamicPointerMode::Rebased));
    assert!(
        resolve_dynamic_pointer(
            &memory,
            &image,
            &mut direct_mode,
            "mixed-control",
            0x800,
            16,
        )
        .is_err()
    );

    let zero_bias_maps = parse_maps(b"000000-001000 r--p 00000000 00:01 1 /main\n").unwrap();
    let zero_bias_memory = Memory::new(&zero_bias_maps, |_address, bytes: &mut [u8]| {
        bytes.fill(0);
        Ok(())
    });
    let zero_bias_image = DynamicImage {
        node: LinkNode {
            address: 0x80,
            bias: 0,
            dynamic: 0x100,
            next: 0,
            previous: 0,
        },
        identity: (0, 1, 1),
        dynamic_mapping_start: 0,
        dynamic_mapping_end: 0x1000,
        dynamic_table_end: 0x1000,
        file_loads: vec![DynamicFileLoad {
            start: 0,
            end: 0x1000,
            offset: 0,
            read: true,
            write: false,
        }],
    };
    assert_eq!(
        resolve_dynamic_pointer(
            &zero_bias_memory,
            &zero_bias_image,
            &mut None,
            "zero-bias-control",
            0x800,
            16,
        )
        .unwrap(),
        0x800
    );

    let ambiguous_maps = parse_maps(
        b"100000-101000 r--p 00000000 00:01 1 /image\n\
          200000-201000 r--p 00100000 00:01 1 /same-image-second-load\n",
    )
    .unwrap();
    let ambiguous_memory = Memory::new(&ambiguous_maps, |_address, bytes: &mut [u8]| {
        bytes.fill(0);
        Ok(())
    });
    let mut ambiguous_image = image.clone();
    ambiguous_image.file_loads.push(DynamicFileLoad {
        start: 0x200000,
        end: 0x201000,
        offset: 0x100000,
        read: true,
        write: false,
    });
    assert!(
        resolve_dynamic_pointer(
            &ambiguous_memory,
            &ambiguous_image,
            &mut None,
            "ambiguity-control",
            0x100800,
            16,
        )
        .is_err()
    );

    let duplicate_maps = parse_maps(
        b"100000-101000 r--p 00000000 00:01 1 /image\n\
          180000-181000 r--p 00000000 00:01 1 /same-file-other-instance\n",
    )
    .unwrap();
    let duplicate_memory = Memory::new(&duplicate_maps, |_address, bytes: &mut [u8]| {
        bytes.fill(0);
        Ok(())
    });
    assert_eq!(
        resolve_dynamic_pointer(
            &duplicate_memory,
            &image,
            &mut None,
            "correct-with-duplicate-control",
            0x100800,
            16,
        )
        .unwrap(),
        0x100800
    );
    assert!(
        resolve_dynamic_pointer(
            &duplicate_memory,
            &image,
            &mut None,
            "direct-decoy-control",
            0x180100,
            16,
        )
        .is_err()
    );
    assert!(
        resolve_dynamic_pointer(
            &duplicate_memory,
            &image,
            &mut None,
            "rebased-decoy-control",
            0x80100,
            16,
        )
        .is_err()
    );

    let mut zero_length_mode = None;
    let zero_length = resolve_dynamic_pointer(
        &memory,
        &image,
        &mut zero_length_mode,
        "zero-length-control",
        0x100800,
        0,
    )
    .unwrap_err()
    .to_string();
    assert!(zero_length.contains("length=0"), "{zero_length}");
    assert_eq!(zero_length_mode, None);

    let mut range_overflow_mode = None;
    let range_overflow = resolve_dynamic_pointer(
        &memory,
        &image,
        &mut range_overflow_mode,
        "range-overflow-control",
        u64::MAX - 7,
        16,
    )
    .unwrap_err()
    .to_string();
    assert!(
        range_overflow.contains("direct=range overflow"),
        "{range_overflow}"
    );
    assert_eq!(range_overflow_mode, None);

    let mut nonreadable_load = image.clone();
    nonreadable_load.file_loads[0].read = false;
    assert!(
        resolve_dynamic_pointer(
            &memory,
            &nonreadable_load,
            &mut None,
            "non-PF_R-control",
            0x100800,
            16,
        )
        .is_err()
    );

    let anonymous_maps = parse_maps(
        b"100000-101000 r-xp 00000000 00:00 0 [vdso]\n\
          101000-102000 r--p 00000000 00:00 0 /anonymous-decoy\n",
    )
    .unwrap();
    let anonymous_memory = Memory::new(&anonymous_maps, |_address, bytes: &mut [u8]| {
        bytes.fill(0);
        Ok(())
    });
    let anonymous_image = DynamicImage {
        node,
        identity: (0, 0, 0),
        dynamic_mapping_start: 0x100000,
        dynamic_mapping_end: 0x101000,
        dynamic_table_end: 0x101000,
        file_loads: vec![DynamicFileLoad {
            start: 0x100000,
            end: 0x101000,
            offset: 0,
            read: true,
            write: false,
        }],
    };
    assert_eq!(
        resolve_dynamic_pointer(
            &anonymous_memory,
            &anonymous_image,
            &mut None,
            "vdso-relative-control",
            0x800,
            16,
        )
        .unwrap(),
        0x100800
    );
    assert!(
        resolve_dynamic_pointer(
            &anonymous_memory,
            &anonymous_image,
            &mut None,
            "vdso-cross-vma-control",
            0x1100,
            16,
        )
        .is_err()
    );

    for (permissions, identity) in [
        ("r--p", "00:02 2"),
        ("rw-p", "00:01 1"),
        ("r--s", "00:01 1"),
        ("---p", "00:01 1"),
    ] {
        let maps = parse_maps(
            format!(
                "100000-101000 r--p 00000000 00:01 1 /image\n\
                 101000-102000 {permissions} 00001000 {identity} /candidate\n"
            )
            .as_bytes(),
        )
        .unwrap();
        let memory = Memory::new(&maps, |_address, bytes: &mut [u8]| {
            bytes.fill(0);
            Ok(())
        });
        let mut mapping_image = image.clone();
        mapping_image.file_loads.push(DynamicFileLoad {
            start: 0x101000,
            end: 0x102000,
            offset: 0x1000,
            read: true,
            write: false,
        });
        assert!(
            resolve_dynamic_pointer(
                &memory,
                &mapping_image,
                &mut None,
                "mapping-control",
                0x101100,
                16,
            )
            .is_err(),
            "accepted permissions={permissions} identity={identity}"
        );
    }

    let crossing_maps = parse_maps(
        b"100000-101000 r--p 00000000 00:01 1 /image\n\
          101000-102000 r--p 00001000 00:02 2 /other-image\n",
    )
    .unwrap();
    let crossing_memory = Memory::new(&crossing_maps, |_address, bytes: &mut [u8]| {
        bytes.fill(0);
        Ok(())
    });
    let mut crossing_image = image.clone();
    crossing_image.file_loads[0].end = 0x102000;
    assert!(
        resolve_dynamic_pointer(
            &crossing_memory,
            &crossing_image,
            &mut None,
            "crossing-control",
            0x100ff8,
            16,
        )
        .is_err()
    );

    let overflow_image = DynamicImage {
        node: LinkNode {
            address: 1,
            bias: u64::MAX - 7,
            dynamic: u64::MAX - 4,
            next: 0,
            previous: 0,
        },
        identity: (0, 1, 1),
        dynamic_mapping_start: u64::MAX - 8,
        dynamic_mapping_end: u64::MAX,
        dynamic_table_end: u64::MAX,
        file_loads: Vec::new(),
    };
    assert!(
        resolve_dynamic_pointer(
            &memory,
            &overflow_image,
            &mut None,
            "overflow-control",
            16,
            16,
        )
        .is_err()
    );
}

#[test]
fn static_parser_refuses_alias_version_relocation_and_getenv_shape_mutations() {
    let base = libc_image();
    for mutation in 0..6 {
        let mut bytes = base.clone();
        match mutation {
            0 => put64(&mut bytes, 0x500 + 2 * 24 + 8, 0x3810),
            1 => put16(&mut bytes, 0x680 + 3 * 2, 2),
            2 => put64(
                &mut bytes,
                0x788,
                ((4_u64) << 32) | u64::from(reloc::R_X86_64_COPY),
            ),
            3 => put64(&mut bytes, 0x790, 1),
            4 => bytes[0x1143] ^= 1,
            5 => put64(&mut bytes, 64 + 2 * 56 + 32, 0x1900),
            _ => unreachable!(),
        }
        assert!(
            EnvironmentProvider::parse(&bytes).is_err(),
            "mutation {mutation}"
        );
    }
}

#[test]
fn bookkeeping_is_all_or_nothing_and_requires_two_exact_local_bss_words() {
    let image = libc_image();
    let provider = EnvironmentProvider::parse(&image).unwrap();
    let objects = || {
        vec![
            LocalObject {
                name: "__environ_counter".into(),
                rva: 0x3700,
                size: 8,
                binding: sym::STB_LOCAL,
                kind: sym::STT_OBJECT,
                visibility: sym::STV_DEFAULT,
                section: 3,
            },
            LocalObject {
                name: "__environ_array_list".into(),
                rva: 0x3710,
                size: 8,
                binding: sym::STB_LOCAL,
                kind: sym::STT_OBJECT,
                visibility: sym::STV_DEFAULT,
                section: 3,
            },
        ]
    };
    assert_eq!(
        choose_bookkeeping(objects(), &provider.writable_load).unwrap(),
        Some(StaticBookkeeping {
            counter_rva: 0x3700,
            allocation_list_rva: 0x3710
        })
    );
    assert!(choose_bookkeeping(objects().into_iter().take(1), &provider.writable_load).is_err());
    let mut invalid = objects();
    invalid[0].size = 4;
    assert!(choose_bookkeeping(invalid, &provider.writable_load).is_err());

    let mut overlapping = objects();
    overlapping[1].rva = overlapping[0].rva + 4;
    assert!(choose_bookkeeping(overlapping, &provider.writable_load).is_err());
}

#[test]
fn getenv_shape_binds_both_counter_reads_when_bookkeeping_is_available() {
    let mut code = vec![
        0x4c, 0x8b, 0x2d, 0xf9, 0x1f, 0, 0, // mov r13,[rip+0x1ff9] -> 0x3000
        0x48, 0x8b, 0x05, 0xf2, 0x0f, 0, 0, // mov rax,[rip+0xff2] -> 0x2000
        0x4c, 0x8b, 0x20, // mov r12,[rax]
        0x48, 0x8b, 0x05, 0xe8, 0x1f, 0, 0, // mov rax,[rip+0x1fe8] -> 0x3000
        0xc3,
    ];
    let bookkeeping = Some(StaticBookkeeping {
        counter_rva: 0x3000,
        allocation_list_rva: 0x3010,
    });
    assert!(
        validate_getenv_data_accesses([0; 32], &code, 0x1000, 0x2000, 0x4000, bookkeeping).is_ok()
    );
    code[20] ^= 1;
    assert!(
        validate_getenv_data_accesses([0; 32], &code, 0x1000, 0x2000, 0x4000, bookkeeping).is_err()
    );
}

#[test]
fn counter_guarded_getenv_requires_exact_provider_code_metadata_and_bookkeeping() {
    assert!(
        validate_getenv_data_accesses(
            COUNTER_GUARDED_LIBC_SHA256,
            COUNTER_GUARDED_GETENV_CODE,
            COUNTER_GUARDED_GETENV_RVA,
            COUNTER_GUARDED_GETENV_GOT_RVA,
            COUNTER_GUARDED_ENVIRONMENT_OBJECT_RVA,
            Some(COUNTER_GUARDED_BOOKKEEPING),
        )
        .is_ok()
    );

    assert!(
        validate_getenv_data_accesses(
            [0; 32],
            COUNTER_GUARDED_GETENV_CODE,
            COUNTER_GUARDED_GETENV_RVA,
            COUNTER_GUARDED_GETENV_GOT_RVA,
            COUNTER_GUARDED_ENVIRONMENT_OBJECT_RVA,
            Some(COUNTER_GUARDED_BOOKKEEPING),
        )
        .is_err()
    );

    for index in 0..COUNTER_GUARDED_GETENV_CODE.len() {
        let mut mutated = COUNTER_GUARDED_GETENV_CODE.to_vec();
        mutated[index] ^= 1;
        assert!(
            validate_getenv_data_accesses(
                COUNTER_GUARDED_LIBC_SHA256,
                &mutated,
                COUNTER_GUARDED_GETENV_RVA,
                COUNTER_GUARDED_GETENV_GOT_RVA,
                COUNTER_GUARDED_ENVIRONMENT_OBJECT_RVA,
                Some(COUNTER_GUARDED_BOOKKEEPING),
            )
            .is_err(),
            "mutated getenv byte {index} was accepted"
        );
    }

    for (address, got, object, bookkeeping) in [
        (
            COUNTER_GUARDED_GETENV_RVA + 1,
            COUNTER_GUARDED_GETENV_GOT_RVA,
            COUNTER_GUARDED_ENVIRONMENT_OBJECT_RVA,
            Some(COUNTER_GUARDED_BOOKKEEPING),
        ),
        (
            COUNTER_GUARDED_GETENV_RVA,
            COUNTER_GUARDED_GETENV_GOT_RVA + 1,
            COUNTER_GUARDED_ENVIRONMENT_OBJECT_RVA,
            Some(COUNTER_GUARDED_BOOKKEEPING),
        ),
        (
            COUNTER_GUARDED_GETENV_RVA,
            COUNTER_GUARDED_GETENV_GOT_RVA,
            COUNTER_GUARDED_ENVIRONMENT_OBJECT_RVA + 1,
            Some(COUNTER_GUARDED_BOOKKEEPING),
        ),
        (
            COUNTER_GUARDED_GETENV_RVA,
            COUNTER_GUARDED_GETENV_GOT_RVA,
            COUNTER_GUARDED_ENVIRONMENT_OBJECT_RVA,
            None,
        ),
        (
            COUNTER_GUARDED_GETENV_RVA,
            COUNTER_GUARDED_GETENV_GOT_RVA,
            COUNTER_GUARDED_ENVIRONMENT_OBJECT_RVA,
            Some(StaticBookkeeping {
                counter_rva: COUNTER_GUARDED_BOOKKEEPING.counter_rva + 1,
                ..COUNTER_GUARDED_BOOKKEEPING
            }),
        ),
        (
            COUNTER_GUARDED_GETENV_RVA,
            COUNTER_GUARDED_GETENV_GOT_RVA,
            COUNTER_GUARDED_ENVIRONMENT_OBJECT_RVA,
            Some(StaticBookkeeping {
                allocation_list_rva: COUNTER_GUARDED_BOOKKEEPING.allocation_list_rva + 1,
                ..COUNTER_GUARDED_BOOKKEEPING
            }),
        ),
    ] {
        assert!(
            validate_getenv_data_accesses(
                COUNTER_GUARDED_LIBC_SHA256,
                COUNTER_GUARDED_GETENV_CODE,
                address,
                got,
                object,
                bookkeeping,
            )
            .is_err()
        );
    }
}

#[test]
fn expected_map_and_active_parser_refuse_invalid_or_incomplete_bytes() {
    assert!(expected_environment(&expected(&[("A=B", "value")])).is_err());
    assert!(
        expected_environment(&BTreeMap::from([(
            OsString::from("A"),
            OsString::from_vec(b"bad\0value".to_vec()),
        )]))
        .is_err()
    );

    let mut null = Target::new();
    null.put64(0x703800, 0);
    assert!(null.graph(&expected(&[("A", "one")])).is_err());
    let empty = null.graph(&BTreeMap::new()).unwrap();
    assert_eq!(empty.pointer_array_address, 0);
    assert!(empty.pointer_array_bytes.is_empty());

    let mut unterminated = Target::new();
    for index in 0..=MAX_ENVIRONMENT_ENTRIES {
        unterminated.put64(0x900400 + (index * 8) as u64, 0x900f00);
    }
    assert!(unterminated.graph(&expected(&[("A", "one")])).is_err());

    let mut duplicate = Target::new();
    duplicate.write(0x900506, b"A=two\0");
    assert!(duplicate.graph(&expected(&[("A", "one")])).is_err());

    let mut no_equals = Target::new();
    no_equals.write(0x900500, b"A-one\0");
    assert!(no_equals.graph(&expected(&[("A", "one")])).is_err());
}

#[test]
fn target_graph_refuses_interposition_copy_and_wrong_bss_geometry() {
    let mut interposed = Target::new();
    interposed.put64(0x702200, 0x900800);
    assert!(
        interposed
            .graph(&expected(&[("A", "one"), ("B", "two")]))
            .is_err()
    );

    let mut copy = Target::new();
    copy.add_main_environment_relocation("environ", reloc::R_X86_64_COPY, 0);
    let copy = copy.graph(&expected(&[("A", "one"), ("B", "two")]));
    assert!(copy.is_err(), "COPY relocation was accepted: {copy:#?}");

    let mut wrong_bss = Target::new();
    wrong_bss.maps[5].identity = (0, 2, 2);
    assert!(
        wrong_bss
            .graph(&expected(&[("A", "one"), ("B", "two")]))
            .is_err()
    );
}

#[test]
fn graph_imports_for_all_aliases_must_resolve_to_the_authenticated_object() {
    for name in ENVIRONMENT_NAMES {
        let mut target = Target::new();
        target.add_main_environment_relocation(name, reloc::R_X86_64_GLOB_DAT, 0x703800);
        let mut fixture_memory = Memory::new(&target.maps, |address, bytes: &mut [u8]| {
            target.read(address, bytes)
        });
        let fixture_nodes = link_nodes(&mut fixture_memory, 0x900140).unwrap();
        assert_eq!(fixture_nodes.len(), 2, "{fixture_nodes:#?}");
        assert_eq!(fixture_nodes[0].address, 0x900100, "{fixture_nodes:#?}");
        assert_eq!(fixture_nodes[0].bias, 0, "{fixture_nodes:#?}");
        assert_eq!(fixture_nodes[0].dynamic, 0x401000, "{fixture_nodes:#?}");
        let (fixture_dynamic, fixture_image) =
            read_dynamic(&mut fixture_memory, fixture_nodes[0], None).unwrap();
        assert_eq!(
            unique_target_tag(&fixture_dynamic, dynamic::DT_RELA).unwrap(),
            Some(0x401400),
            "main fixture dynamic table is stale: {fixture_dynamic:#?}"
        );
        let mut fixture_mode = None;
        assert_eq!(
            relocation_tables(
                &fixture_memory,
                &fixture_dynamic,
                &fixture_image,
                &mut fixture_mode,
            )
            .unwrap()
            .len(),
            1,
            "main fixture relocation table is absent: {fixture_dynamic:#?}"
        );
        let fixture_relocations =
            observe_alias_relocations(&mut fixture_memory, 0x900140, 0x703800, None).unwrap();
        assert!(
            fixture_relocations
                .iter()
                .any(|relocation| relocation.link_map == 0x900100 && relocation.symbol == name),
            "fixture observer missed {name}: {fixture_relocations:#?}"
        );
        let graph = target
            .graph(&expected(&[("A", "one"), ("B", "two")]))
            .unwrap();
        assert!(
            graph
                .relocations
                .iter()
                .any(|relocation| relocation.link_map == 0x900100 && relocation.symbol == name),
            "missing {name} import in {:#?}",
            graph.relocations,
        );

        target.put64(0x401500, 0x900800);
        assert!(
            target
                .graph(&expected(&[("A", "one"), ("B", "two")]))
                .is_err()
        );
    }
}

#[test]
fn setenv_unsetenv_and_putenv_shapes_cannot_pass_exact_comparison() {
    let target = Target::new();
    let before = TargetEnvironmentBefore(
        target
            .graph(&expected(&[("A", "one"), ("B", "two")]))
            .unwrap(),
    );

    // setenv commonly replaces libc's pointer array. The original array and
    // strings are left untouched, and even the parsed map stays identical.
    let mut setenv = target.clone();
    setenv.put64(0x703800, 0x900700);
    setenv.put64(0x900700, 0x900500);
    setenv.put64(0x900708, 0x900506);
    setenv.put64(0x900710, 0);
    let after = TargetEnvironmentAfter(
        setenv
            .graph(&expected(&[("A", "one"), ("B", "two")]))
            .unwrap(),
    );
    assert!(before.compare_exact(&after).is_err());

    // Reordering preserves the parsed map while changing the active array.
    let mut reordered = target.clone();
    reordered.put64(0x900400, 0x900506);
    reordered.put64(0x900408, 0x900500);
    let after = TargetEnvironmentAfter(
        reordered
            .graph(&expected(&[("A", "one"), ("B", "two")]))
            .unwrap(),
    );
    assert!(before.compare_exact(&after).is_err());

    // unsetenv compacts the active array without changing either original
    // string. The changed order/termination is retained and rejected.
    let mut unsetenv = target.clone();
    unsetenv.put64(0x900400, 0x900506);
    unsetenv.put64(0x900408, 0);
    assert!(
        unsetenv
            .graph(&expected(&[("A", "one"), ("B", "two")]))
            .is_err()
    );
    let after = TargetEnvironmentAfter(unsetenv.graph(&expected(&[("B", "two")])).unwrap());
    assert!(before.compare_exact(&after).is_err());

    // putenv can expose caller-owned mutable bytes while retaining the same
    // object and pointer array. Exact string bytes still detect the mutation.
    let mut putenv = target;
    putenv.write(0x900500, b"A=ONE\0");
    assert!(
        putenv
            .graph(&expected(&[("A", "one"), ("B", "two")]))
            .is_err()
    );
    let after = TargetEnvironmentAfter(
        putenv
            .graph(&expected(&[("A", "ONE"), ("B", "two")]))
            .unwrap(),
    );
    assert!(before.compare_exact(&after).is_err());
}
