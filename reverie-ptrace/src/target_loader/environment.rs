/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! Observe the active libc environment without running target code.
//!
//! This is deliberately separate from the function resolver. It first reuses
//! that resolver's complete libc and default-link-map authentication, then
//! proves which data object the bound libc's `getenv` actually reads. The
//! resulting graph is evidence only: none of its addresses authorizes a call
//! or a target write.

use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::ffi::OsString;
use std::os::unix::ffi::OsStrExt;

use goblin::elf::reloc;
use sha2::Digest;
use sha2::Sha256;

use super::*;

const ENVIRONMENT_VERSION: &str = "GLIBC_2.2.5";
const ENVIRONMENT_NAMES: [&str; 3] = ["environ", "_environ", "__environ"];
const MAX_ENVIRONMENT_ENTRIES: usize = 256;
const MAX_ENVIRONMENT_BYTES: usize = 1024 * 1024;
const MAX_GRAPH_RELOCATIONS: usize = 32 * 1024;
const TARGET_PAGE_SIZE: u64 = 4096;

// This is the complete provider selected by the reviewed fixed staging graph,
// not a SONAME, pathname or inode allowlist.  The counter-guarded getenv below
// is accepted only for these exact provider bytes.  A different libc must fail
// closed until its complete access profile is separately reviewed.
const COUNTER_GUARDED_LIBC_SHA256: [u8; 32] = [
    0xd9, 0x32, 0xcb, 0x6b, 0xc8, 0x8d, 0xa1, 0x0c, 0xc7, 0x09, 0xd5, 0xb7, 0xec, 0xda, 0x57, 0xa1,
    0x38, 0x7b, 0x65, 0x71, 0x33, 0x16, 0x89, 0xdb, 0xa8, 0x80, 0x23, 0xb4, 0xc0, 0x51, 0x77, 0x76,
];
const COUNTER_GUARDED_GETENV_RVA: u64 = 0x428a0;
const COUNTER_GUARDED_ENVIRONMENT_OBJECT_RVA: u64 = 0x2033a0;
const COUNTER_GUARDED_GETENV_GOT_RVA: u64 = 0x1fafa8;
const COUNTER_GUARDED_GETENV_CODE: &[u8] = &[
    0xf3, 0x0f, 0x1e, 0xfa, 0x41, 0x57, 0x41, 0x56, 0x41, 0x55, 0x41, 0x54, 0x49, 0x89, 0xfc, 0x55,
    0x53, 0x48, 0x83, 0xec, 0x08, 0x4c, 0x8b, 0x3d, 0xec, 0x86, 0x1b, 0x00, 0x4c, 0x8b, 0x35, 0xbd,
    0xae, 0x1b, 0x00, 0x49, 0x8b, 0x2f, 0x48, 0x85, 0xed, 0x74, 0x69, 0x41, 0x80, 0x3c, 0x24, 0x00,
    0x74, 0x62, 0x4c, 0x89, 0xe7, 0xe8, 0xf6, 0x6b, 0xfe, 0xff, 0x49, 0x89, 0xc5, 0xeb, 0x05, 0x90,
    0x48, 0x83, 0xc5, 0x08, 0x48, 0x8b, 0x5d, 0x00, 0x48, 0x85, 0xdb, 0x74, 0x3b, 0x0f, 0xb6, 0x03,
    0x41, 0x38, 0x04, 0x24, 0x75, 0xea, 0x4c, 0x89, 0xea, 0x48, 0x89, 0xde, 0x4c, 0x89, 0xe7, 0xe8,
    0xdc, 0x6c, 0xfe, 0xff, 0x85, 0xc0, 0x75, 0xd8, 0x42, 0x80, 0x3c, 0x2b, 0x3d, 0x75, 0xd1, 0x48,
    0x83, 0xc4, 0x08, 0x4a, 0x8d, 0x44, 0x2b, 0x01, 0x5b, 0x5d, 0x41, 0x5c, 0x41, 0x5d, 0x41, 0x5e,
    0x41, 0x5f, 0xc3, 0x0f, 0x1f, 0x44, 0x00, 0x00, 0x48, 0x8b, 0x05, 0x51, 0xae, 0x1b, 0x00, 0x49,
    0x39, 0xc6, 0x75, 0x88, 0x48, 0x83, 0xc4, 0x08, 0x31, 0xc0, 0x5b, 0x5d, 0x41, 0x5c, 0x41, 0x5d,
    0x41, 0x5e, 0x41, 0x5f, 0xc3,
];

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct TargetEnvironmentMapping {
    pub(crate) start: u64,
    pub(crate) end: u64,
    pub(crate) offset: u64,
    pub(crate) identity: (u64, u64, u64),
    pub(crate) read: bool,
    pub(crate) write: bool,
    pub(crate) execute: bool,
    pub(crate) private: bool,
}

impl From<&Map> for TargetEnvironmentMapping {
    fn from(map: &Map) -> Self {
        Self {
            start: map.start,
            end: map.end,
            offset: map.offset,
            identity: map.identity,
            read: map.read,
            write: map.write,
            execute: map.execute,
            private: map.private,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct TargetEnvironmentWord {
    pub(crate) address: u64,
    pub(crate) bytes: [u8; 8],
    pub(crate) value: u64,
    pub(crate) mappings: Vec<TargetEnvironmentMapping>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct TargetEnvironmentAlias {
    pub(crate) name: String,
    pub(crate) address: u64,
    pub(crate) strong: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct TargetEnvironmentRelocation {
    pub(crate) link_map: u64,
    pub(crate) load_bias: u64,
    pub(crate) dynamic: u64,
    pub(crate) symbol: String,
    pub(crate) slot: u64,
    pub(crate) value: u64,
    pub(crate) addend: i64,
    pub(crate) mappings: Vec<TargetEnvironmentMapping>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct TargetEnvironmentLoad {
    pub(crate) elf_start: u64,
    pub(crate) file_end: u64,
    pub(crate) memory_end: u64,
    pub(crate) mapped_file_start: u64,
    pub(crate) mapped_file_end: u64,
    pub(crate) mapped_memory_end: u64,
    pub(crate) mappings: Vec<TargetEnvironmentMapping>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct TargetEnvironmentEntry {
    pub(crate) pointer_slot: u64,
    pub(crate) string_address: u64,
    /// Exact bytes, including the terminating NUL.
    pub(crate) bytes: Vec<u8>,
    pub(crate) key: Vec<u8>,
    pub(crate) value: Vec<u8>,
    pub(crate) mappings: Vec<TargetEnvironmentMapping>,
}

type ObservedPointerGraph = (
    u64,
    Vec<u8>,
    Vec<TargetEnvironmentMapping>,
    Vec<TargetEnvironmentEntry>,
    BTreeMap<Vec<u8>, Vec<u8>>,
);

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct TargetEnvironmentBookkeeping {
    pub(crate) counter: TargetEnvironmentWord,
    pub(crate) allocation_list: TargetEnvironmentWord,
}

/// One exact stopped-target observation of libc's active environment graph.
///
/// Equality intentionally includes addresses, pointer order, raw bytes,
/// mapping geometry, every alias/import, and the optional libc bookkeeping.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct TargetEnvironmentGraph {
    pub(crate) tid: i32,
    pub(crate) start_ticks: u64,
    pub(crate) executable_phdr: u64,
    pub(crate) libc_link_map: u64,
    pub(crate) libc_load_bias: u64,
    pub(crate) libc_mapping_identity: (u64, u64, u64),
    pub(crate) getenv_address: u64,
    pub(crate) aliases: Vec<TargetEnvironmentAlias>,
    pub(crate) relocations: Vec<TargetEnvironmentRelocation>,
    pub(crate) getenv_got: TargetEnvironmentRelocation,
    pub(crate) writable_load: TargetEnvironmentLoad,
    pub(crate) environ_object: TargetEnvironmentWord,
    pub(crate) pointer_array_address: u64,
    /// Exact pointer words in source order, including the terminating NULL.
    pub(crate) pointer_array_bytes: Vec<u8>,
    pub(crate) pointer_array_mappings: Vec<TargetEnvironmentMapping>,
    pub(crate) entries: Vec<TargetEnvironmentEntry>,
    pub(crate) parsed: BTreeMap<Vec<u8>, Vec<u8>>,
    pub(crate) bookkeeping: Option<TargetEnvironmentBookkeeping>,
}

/// Baseline captured before the first private target call.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct TargetEnvironmentBefore(pub(crate) TargetEnvironmentGraph);

/// Renewed observation after one private transition.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct TargetEnvironmentAfter(pub(crate) TargetEnvironmentGraph);

impl TargetEnvironmentBefore {
    /// Require byte-for-byte and address-for-address preservation. This does
    /// not repair an environment or accept an equal parsed map with a changed
    /// pointer graph.
    pub(crate) fn compare_exact(&self, after: &TargetEnvironmentAfter) -> io::Result<()> {
        if self.0 != after.0 {
            return Err(invalid("active libc environment graph changed"));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct StaticAlias {
    name: String,
    rva: u64,
    strong: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct StaticBookkeeping {
    counter_rva: u64,
    allocation_list_rva: u64,
}

const COUNTER_GUARDED_BOOKKEEPING: StaticBookkeeping = StaticBookkeeping {
    counter_rva: 0x1fd780,
    allocation_list_rva: 0x1fd788,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ReviewedGetenvProfile {
    provider_sha256: [u8; 32],
    code: &'static [u8],
    address: u64,
    got: u64,
    object: u64,
    bookkeeping: Option<StaticBookkeeping>,
}

const COUNTER_GUARDED_GETENV_PROFILE: ReviewedGetenvProfile = ReviewedGetenvProfile {
    provider_sha256: COUNTER_GUARDED_LIBC_SHA256,
    code: COUNTER_GUARDED_GETENV_CODE,
    address: COUNTER_GUARDED_GETENV_RVA,
    got: COUNTER_GUARDED_GETENV_GOT_RVA,
    object: COUNTER_GUARDED_ENVIRONMENT_OBJECT_RVA,
    bookkeeping: Some(COUNTER_GUARDED_BOOKKEEPING),
};

struct EnvironmentProvider<'a> {
    provider: Provider<'a>,
    aliases: Vec<StaticAlias>,
    getenv_rva: u64,
    getenv_got_rva: u64,
    writable_load: goblin::elf::ProgramHeader,
    bookkeeping: Option<StaticBookkeeping>,
}

impl<'a> EnvironmentProvider<'a> {
    fn parse(bytes: &'a [u8]) -> io::Result<Self> {
        Self::parse_with_additional_profiles(bytes, &[])
    }

    /// Parse with extra exact profiles supplied by a unit-test fixture.
    /// Production admission always enters through `parse` and therefore has
    /// no profiles beyond the reviewed whole-provider profile above.
    fn parse_with_additional_profiles(
        bytes: &'a [u8],
        additional_profiles: &[ReviewedGetenvProfile],
    ) -> io::Result<Self> {
        let provider_digest: [u8; 32] = Sha256::digest(bytes).into();
        // Provider::parse remains the unchanged executable-function policy. In
        // particular, adding this data resolver does not make writable data a
        // valid dlopen definition or weaken any exact code comparison.
        let provider = Provider::parse(bytes, "GLIBC_2.34")?;
        if provider.elf.soname != Some("libc.so.6") {
            return Err(invalid("environment provider is not libc"));
        }
        let version = version_index(&provider, ENVIRONMENT_VERSION)?;
        let versions = provider_versions(&provider)?;
        let strings = provider_strings(&provider)?;

        let mut aliases = Vec::new();
        for (name, binding) in [
            ("environ", sym::STB_WEAK),
            ("_environ", sym::STB_WEAK),
            ("__environ", sym::STB_GLOBAL),
        ] {
            let symbol = unique_dynamic_symbol(&provider, strings, name)?;
            if u16_at(versions, symbol.0 * 2) != version
                || symbol.1.st_type() != sym::STT_OBJECT
                || symbol.1.st_bind() != binding
                || symbol.1.st_other != sym::STV_DEFAULT
                || symbol.1.st_shndx == 0
                || symbol.1.st_shndx >= 0xff00
                || symbol.1.st_size != 8
            {
                return Err(invalid(
                    "libc environment alias is not the exact public object",
                ));
            }
            aliases.push(StaticAlias {
                name: name.to_owned(),
                rva: symbol.1.st_value,
                strong: binding == sym::STB_GLOBAL,
            });
        }
        if aliases
            .iter()
            .map(|alias| alias.rva)
            .collect::<BTreeSet<_>>()
            .len()
            != 1
        {
            return Err(invalid("libc environment aliases do not share one object"));
        }
        let object_rva = aliases[0].rva;
        let writable = provider
            .elf
            .program_headers
            .iter()
            .filter(|load| {
                load.p_type == ph::PT_LOAD
                    && load.p_flags == ph::PF_R | ph::PF_W
                    && object_rva >= load.p_vaddr
                    && add(object_rva, 8)
                        .ok()
                        .zip(add(load.p_vaddr, load.p_memsz).ok())
                        .is_some_and(|(end, load_end)| end <= load_end)
            })
            .cloned()
            .collect::<Vec<_>>();
        if writable.len() != 1 || object_rva < add(writable[0].p_vaddr, writable[0].p_filesz)? {
            return Err(invalid(
                "libc environment object is not in one zero-fill load extent",
            ));
        }
        let writable_load = writable[0].clone();

        let getenv = unique_dynamic_symbol(&provider, strings, "getenv")?;
        if u16_at(versions, getenv.0 * 2) != version
            || getenv.1.st_type() != sym::STT_FUNC
            || getenv.1.st_bind() != sym::STB_GLOBAL
            || getenv.1.st_other != sym::STV_DEFAULT
            || getenv.1.st_shndx == 0
            || getenv.1.st_shndx >= 0xff00
            || getenv.1.st_size == 0
            || getenv.1.st_size > 1024 * 1024
        {
            return Err(invalid("getenv is not the exact public normal function"));
        }
        let getenv_code =
            Provider::ro_file(&provider.elf, bytes, getenv.1.st_value, getenv.1.st_size)?;

        let mut alias_relocations = Vec::new();
        for relocation in provider
            .elf
            .dynrelas
            .iter()
            .chain(provider.elf.dynrels.iter())
            .chain(provider.elf.pltrelocs.iter())
        {
            let symbol = provider
                .elf
                .dynsyms
                .get(relocation.r_sym)
                .ok_or_else(|| invalid("environment relocation symbol is outside dynsym"))?;
            let name = c_string(strings, symbol.st_name)?;
            if !ENVIRONMENT_NAMES.contains(&name) {
                continue;
            }
            if relocation.r_type == reloc::R_X86_64_COPY {
                return Err(invalid(
                    "COPY relocation for libc environment is unsupported",
                ));
            }
            if relocation.r_type != reloc::R_X86_64_GLOB_DAT
                || relocation.r_addend != Some(0)
                || symbol.st_type() != sym::STT_OBJECT
            {
                return Err(invalid("unsupported libc environment relocation"));
            }
            alias_relocations.push((name, relocation.r_offset));
        }
        if alias_relocations.len() != 1 || alias_relocations[0].0 != "__environ" {
            return Err(invalid("libc has no unique __environ GOT relocation"));
        }
        let getenv_got_rva = alias_relocations[0].1;
        if getenv_got_rva < writable_load.p_vaddr
            || add(getenv_got_rva, 8)? > add(writable_load.p_vaddr, writable_load.p_filesz)?
        {
            return Err(invalid(
                "getenv environment GOT slot is outside writable file bytes",
            ));
        }

        let bookkeeping = static_bookkeeping(&provider.elf, &writable_load)?;
        validate_getenv_data_accesses(
            provider_digest,
            getenv_code,
            getenv.1.st_value,
            getenv_got_rva,
            object_rva,
            bookkeeping,
            additional_profiles,
        )?;

        Ok(Self {
            provider,
            aliases,
            getenv_rva: getenv.1.st_value,
            getenv_got_rva,
            writable_load,
            bookkeeping,
        })
    }
}

fn provider_tag(provider: &Provider<'_>, key: u64) -> io::Result<u64> {
    let mut value = None;
    for entry in &provider.elf.dynamic.as_ref().unwrap().dyns {
        if entry.d_tag == key && value.replace(entry.d_val).is_some() {
            return Err(invalid("duplicate environment symbol metadata"));
        }
    }
    value.ok_or_else(|| invalid("missing environment symbol metadata"))
}

fn provider_strings<'a>(provider: &Provider<'a>) -> io::Result<&'a [u8]> {
    Provider::ro_file(
        &provider.elf,
        provider.bytes,
        provider_tag(provider, dynamic::DT_STRTAB)?,
        provider_tag(provider, dynamic::DT_STRSZ)?,
    )
}

fn provider_versions<'a>(provider: &Provider<'a>) -> io::Result<&'a [u8]> {
    Provider::ro_file(
        &provider.elf,
        provider.bytes,
        provider_tag(provider, dynamic::DT_VERSYM)?,
        (provider.elf.dynsyms.len() * 2) as u64,
    )
}

fn version_index(provider: &Provider<'_>, requested: &str) -> io::Result<u16> {
    let strings = provider_strings(provider)?;
    let count = provider_tag(provider, dynamic::DT_VERDEFNUM)?;
    if count == 0 || count > 256 {
        return Err(invalid("environment version count is outside bound"));
    }
    let mut address = provider_tag(provider, dynamic::DT_VERDEF)?;
    let mut selected = None;
    for _ in 0..count {
        let definition = Provider::ro_file(&provider.elf, provider.bytes, address, 20)?;
        let auxiliary = Provider::ro_file(
            &provider.elf,
            provider.bytes,
            add(address, u32_at(definition, 12) as u64)?,
            8,
        )?;
        if c_string(strings, u32_at(auxiliary, 0) as usize)? == requested
            && selected.replace(u16_at(definition, 4)).is_some()
        {
            return Err(invalid("ambiguous libc environment symbol version"));
        }
        address = add(address, u32_at(definition, 16) as u64)?;
    }
    selected.ok_or_else(|| invalid("libc environment symbol version is absent"))
}

fn unique_dynamic_symbol(
    provider: &Provider<'_>,
    strings: &[u8],
    requested: &str,
) -> io::Result<(usize, goblin::elf::sym::Sym)> {
    let mut selected = None;
    for (index, symbol) in provider.elf.dynsyms.iter().enumerate() {
        if c_string(strings, symbol.st_name)? == requested
            && selected.replace((index, symbol)).is_some()
        {
            return Err(invalid("ambiguous libc environment dynamic symbol"));
        }
    }
    selected.ok_or_else(|| invalid("libc environment dynamic symbol is absent"))
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct LocalObject {
    name: String,
    rva: u64,
    size: u64,
    binding: u8,
    kind: u8,
    visibility: u8,
    section: usize,
}

fn choose_bookkeeping(
    objects: impl IntoIterator<Item = LocalObject>,
    writable: &goblin::elf::ProgramHeader,
) -> io::Result<Option<StaticBookkeeping>> {
    let mut counter = None;
    let mut allocation_list = None;
    for object in objects {
        let destination = match object.name.as_str() {
            "__environ_counter" => &mut counter,
            "__environ_array_list" => &mut allocation_list,
            _ => continue,
        };
        if destination.replace(object.clone()).is_some() {
            return Err(invalid("ambiguous libc environment bookkeeping symbol"));
        }
    }
    let (counter, allocation_list) = match (counter, allocation_list) {
        (None, None) => return Ok(None),
        (Some(counter), Some(allocation_list)) => (counter, allocation_list),
        _ => return Err(invalid("incomplete libc environment bookkeeping symbols")),
    };
    for object in [&counter, &allocation_list] {
        if object.size != 8
            || object.binding != sym::STB_LOCAL
            || object.kind != sym::STT_OBJECT
            || object.visibility != sym::STV_DEFAULT
            || object.section == 0
            || object.section >= 0xff00
            || object.rva < add(writable.p_vaddr, writable.p_filesz)?
            || add(object.rva, 8)? > add(writable.p_vaddr, writable.p_memsz)?
        {
            return Err(invalid(
                "libc environment bookkeeping is not an exact local BSS word",
            ));
        }
    }
    let counter_end = add(counter.rva, counter.size)?;
    let allocation_end = add(allocation_list.rva, allocation_list.size)?;
    if counter.rva < allocation_end && allocation_list.rva < counter_end {
        return Err(invalid("libc environment bookkeeping words overlap"));
    }
    Ok(Some(StaticBookkeeping {
        counter_rva: counter.rva,
        allocation_list_rva: allocation_list.rva,
    }))
}

fn static_bookkeeping(
    elf: &Elf<'_>,
    writable: &goblin::elf::ProgramHeader,
) -> io::Result<Option<StaticBookkeeping>> {
    let mut objects = Vec::new();
    for symbol in elf.syms.iter() {
        let Some(name) = elf.strtab.get_at(symbol.st_name) else {
            return Err(invalid("invalid libc static symbol name"));
        };
        if matches!(name, "__environ_counter" | "__environ_array_list") {
            objects.push(LocalObject {
                name: name.to_owned(),
                rva: symbol.st_value,
                size: symbol.st_size,
                binding: symbol.st_bind(),
                kind: symbol.st_type(),
                visibility: symbol.st_other,
                section: symbol.st_shndx,
            });
        }
    }
    choose_bookkeeping(objects, writable)
}

fn validate_getenv_data_accesses(
    provider_digest: [u8; 32],
    code: &[u8],
    address: u64,
    got: u64,
    object: u64,
    bookkeeping: Option<StaticBookkeeping>,
    additional_profiles: &[ReviewedGetenvProfile],
) -> io::Result<()> {
    let mut selected = None;
    for profile in
        std::iter::once(&COUNTER_GUARDED_GETENV_PROFILE).chain(additional_profiles.iter())
    {
        if provider_digest == profile.provider_sha256 && selected.replace(profile).is_some() {
            return Err(invalid("ambiguous reviewed getenv provider profile"));
        }
    }
    let profile = selected.ok_or_else(|| invalid("getenv provider bytes are not reviewed"))?;
    if code != profile.code
        || address != profile.address
        || got != profile.got
        || object != profile.object
        || bookkeeping != profile.bookkeeping
    {
        return Err(invalid(
            "getenv code or metadata differs from reviewed provider profile",
        ));
    }
    Ok(())
}

fn page_down(value: u64) -> u64 {
    value & !(TARGET_PAGE_SIZE - 1)
}

fn page_up(value: u64) -> io::Result<u64> {
    add(value, TARGET_PAGE_SIZE - 1).map(page_down)
}

fn mappings_for_range<F: FnMut(u64, &mut [u8]) -> io::Result<()>>(
    memory: &Memory<'_, F>,
    address: u64,
    length: usize,
) -> io::Result<Vec<TargetEnvironmentMapping>> {
    let end = add(address, length as u64)?;
    let mut at = address;
    let mut result = Vec::new();
    while at < end {
        let mapping = memory.mapping(at)?;
        let observed = TargetEnvironmentMapping::from(mapping);
        if result.last() != Some(&observed) {
            result.push(observed);
        }
        at = mapping.end.min(end);
    }
    Ok(result)
}

fn read_word<F: FnMut(u64, &mut [u8]) -> io::Result<()>>(
    memory: &mut Memory<'_, F>,
    address: u64,
) -> io::Result<TargetEnvironmentWord> {
    let mappings = mappings_for_range(memory, address, 8)?;
    let bytes: [u8; 8] = memory
        .get(address, 8)?
        .try_into()
        .map_err(|_| invalid("environment word width changed"))?;
    Ok(TargetEnvironmentWord {
        address,
        value: u64::from_le_bytes(bytes),
        bytes,
        mappings,
    })
}

fn observe_writable_load<F: FnMut(u64, &mut [u8]) -> io::Result<()>>(
    memory: &Memory<'_, F>,
    resolved: &TargetDlopen,
    load: &goblin::elf::ProgramHeader,
) -> io::Result<TargetEnvironmentLoad> {
    let elf_start = add(resolved.load_bias, load.p_vaddr)?;
    let file_end = add(elf_start, load.p_filesz)?;
    let memory_end = add(elf_start, load.p_memsz)?;
    let mapped_file_start = page_down(elf_start);
    let mapped_file_end = page_up(file_end)?;
    let mapped_memory_end = page_up(memory_end)?;
    let mapped_file_offset = page_down(load.p_offset);
    if !mapped_file_start.is_multiple_of(TARGET_PAGE_SIZE)
        || mapped_file_end > mapped_memory_end
        || load.p_offset % TARGET_PAGE_SIZE != load.p_vaddr % TARGET_PAGE_SIZE
    {
        return Err(invalid("libc writable load page geometry is invalid"));
    }

    let mut mappings = Vec::new();
    let mut at = mapped_file_start;
    while at < mapped_file_end {
        let mapping = memory.mapping(at)?;
        if mapping.identity != resolved.mapping_identity
            || !mapping.read
            || mapping.execute
            || !mapping.private
            || add(mapping.offset, at - mapping.start)?
                != add(mapped_file_offset, at - mapped_file_start)?
        {
            return Err(invalid("libc writable file mapping geometry disagrees"));
        }
        let observed = TargetEnvironmentMapping::from(mapping);
        if mappings.last() != Some(&observed) {
            mappings.push(observed);
        }
        at = mapping.end.min(mapped_file_end);
    }

    if mapped_file_end < mapped_memory_end {
        let bss = memory.mapping(mapped_file_end)?;
        if bss.start != mapped_file_end
            || bss.end != mapped_memory_end
            || bss.identity != (0, 0, 0)
            || bss.offset != 0
            || !bss.read
            || !bss.write
            || bss.execute
            || !bss.private
        {
            return Err(invalid("libc anonymous BSS mapping geometry disagrees"));
        }
        mappings.push(TargetEnvironmentMapping::from(bss));
    }

    Ok(TargetEnvironmentLoad {
        elf_start,
        file_end,
        memory_end,
        mapped_file_start,
        mapped_file_end,
        mapped_memory_end,
        mappings,
    })
}

#[derive(Clone, Copy, Debug)]
struct LinkNode {
    address: u64,
    bias: u64,
    dynamic: u64,
    next: u64,
    previous: u64,
}

fn read_link<F: FnMut(u64, &mut [u8]) -> io::Result<()>>(
    memory: &mut Memory<'_, F>,
    address: u64,
) -> io::Result<LinkNode> {
    let bytes = memory.get(address, 40)?;
    Ok(LinkNode {
        address,
        bias: u64_at(&bytes, 0),
        dynamic: u64_at(&bytes, 16),
        next: u64_at(&bytes, 24),
        previous: u64_at(&bytes, 32),
    })
}

fn link_nodes<F: FnMut(u64, &mut [u8]) -> io::Result<()>>(
    memory: &mut Memory<'_, F>,
    provider: u64,
) -> io::Result<Vec<LinkNode>> {
    let mut address = provider;
    let mut reverse = BTreeSet::new();
    loop {
        if reverse.len() >= MAX_LINKS || !reverse.insert(address) {
            return Err(invalid("environment link map reverse chain is cyclic"));
        }
        let node = read_link(memory, address)?;
        if node.previous == 0 {
            address = node.address;
            break;
        }
        address = node.previous;
    }

    let mut result = Vec::new();
    let mut previous = 0;
    let mut saw_provider = false;
    while address != 0 {
        if result.len() >= MAX_LINKS || result.iter().any(|node: &LinkNode| node.address == address)
        {
            return Err(invalid("environment link map forward chain is cyclic"));
        }
        let node = read_link(memory, address)?;
        if node.previous != previous || node.dynamic == 0 {
            return Err(invalid("environment link map is inconsistent"));
        }
        saw_provider |= node.address == provider;
        previous = address;
        address = node.next;
        result.push(node);
    }
    if !saw_provider {
        return Err(invalid("authenticated libc disappeared from link map"));
    }
    Ok(result)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DynamicPointerMode {
    Direct,
    Rebased,
}

#[derive(Clone, Copy, Debug)]
struct DynamicFileLoad {
    start: u64,
    end: u64,
    offset: u64,
    read: bool,
    write: bool,
}

#[derive(Clone, Debug)]
struct DynamicImage {
    node: LinkNode,
    identity: (u64, u64, u64),
    dynamic_mapping_start: u64,
    dynamic_mapping_end: u64,
    dynamic_table_end: u64,
    file_loads: Vec<DynamicFileLoad>,
}

fn readonly_file_range_reason<F: FnMut(u64, &mut [u8]) -> io::Result<()>>(
    memory: &Memory<'_, F>,
    address: u64,
    offset: u64,
    length: u64,
    identity: (u64, u64, u64),
) -> Result<(), String> {
    let end = address
        .checked_add(length)
        .ok_or_else(|| "range overflow".to_owned())?;
    let mut at = address;
    while at < end {
        let Some(mapping) = memory
            .maps
            .iter()
            .find(|mapping| mapping.start <= at && at < mapping.end)
        else {
            return Err(format!("address {at:#x} is unmapped"));
        };
        if !mapping.read {
            return Err(format!(
                "address {at:#x} is non-readable in [{:#x},{:#x})",
                mapping.start, mapping.end,
            ));
        }
        if mapping.write || !mapping.private {
            return Err(format!(
                "address {at:#x} has mutable/shared mapping [{:#x},{:#x})",
                mapping.start, mapping.end,
            ));
        }
        if mapping.identity != identity {
            return Err(format!(
                "address {at:#x} has identity {:?}, expected {identity:?}",
                mapping.identity,
            ));
        }
        let actual_offset = mapping
            .offset
            .checked_add(at - mapping.start)
            .ok_or_else(|| "mapped file offset overflow".to_owned())?;
        let expected_offset = offset
            .checked_add(at - address)
            .ok_or_else(|| "expected file offset overflow".to_owned())?;
        if actual_offset != expected_offset {
            return Err(format!(
                "address {at:#x} has file offset {actual_offset:#x}, expected {expected_offset:#x}",
            ));
        }
        at = mapping.end.min(end);
    }
    Ok(())
}

fn dynamic_file_candidate<F: FnMut(u64, &mut [u8]) -> io::Result<()>>(
    memory: &mut Memory<'_, F>,
    node: LinkNode,
    identity: (u64, u64, u64),
    dynamic_mapping_start: u64,
    dynamic_mapping_end: u64,
    header_address: u64,
) -> io::Result<Option<DynamicImage>> {
    let header_bytes = memory.get(header_address, 64)?;
    if &header_bytes[..7] != b"\x7fELF\x02\x01\x01"
        || !matches!(u16_at(&header_bytes, 16), header::ET_EXEC | header::ET_DYN)
        || u16_at(&header_bytes, 18) != header::EM_X86_64
        || u32_at(&header_bytes, 20) != 1
        || u16_at(&header_bytes, 52) != 64
        || u16_at(&header_bytes, 54) != 56
    {
        return Ok(None);
    }
    let kind = u16_at(&header_bytes, 16);
    if kind == header::ET_EXEC && node.bias != 0 {
        return Ok(None);
    }
    let count = u16_at(&header_bytes, 56) as u64;
    if count == 0 || count > 128 {
        return Ok(None);
    }
    let phdr_offset = u64_at(&header_bytes, 32);
    let phdr_size = count
        .checked_mul(56)
        .ok_or_else(|| invalid("target program header size overflow"))?;
    let phdr_address = header_address
        .checked_add(phdr_offset)
        .ok_or_else(|| invalid("target program header address overflow"))?;
    if phdr_offset
        .checked_add(phdr_size)
        .is_none_or(|end| end > MAX_FILE as u64)
        || readonly_file_range_reason(memory, phdr_address, phdr_offset, phdr_size, identity)
            .is_err()
    {
        return Ok(None);
    }
    let phdr_bytes = memory.get(phdr_address, phdr_size as usize)?;
    let program_headers = phdr_bytes
        .as_chunks::<56>()
        .0
        .iter()
        .map(|bytes| goblin::elf::ProgramHeader {
            p_type: u32_at(bytes, 0),
            p_flags: u32_at(bytes, 4),
            p_offset: u64_at(bytes, 8),
            p_vaddr: u64_at(bytes, 16),
            p_paddr: u64_at(bytes, 24),
            p_filesz: u64_at(bytes, 32),
            p_memsz: u64_at(bytes, 40),
            p_align: u64_at(bytes, 48),
        })
        .collect::<Vec<_>>();
    if validate_loads(&program_headers).is_err() {
        return Ok(None);
    }
    let Some(header_vaddr) = header_address.checked_sub(node.bias) else {
        return Ok(None);
    };
    if load_file_offset(&program_headers, header_vaddr, 64, true).ok() != Some(0) {
        return Ok(None);
    }
    let Some(phdr_vaddr) = header_vaddr.checked_add(phdr_offset) else {
        return Ok(None);
    };
    if load_file_offset(&program_headers, phdr_vaddr, phdr_size, true).ok() != Some(phdr_offset) {
        return Ok(None);
    }
    let dynamic = program_headers
        .iter()
        .filter(|segment| segment.p_type == ph::PT_DYNAMIC)
        .collect::<Vec<_>>();
    if dynamic.len() != 1 {
        return Ok(None);
    }
    let dynamic = dynamic[0];
    if dynamic.p_filesz == 0
        || dynamic.p_filesz > dynamic.p_memsz
        || dynamic.p_filesz % 16 != 0
        || dynamic.p_filesz / 16 > MAX_DYNAMIC as u64
        || load_file_offset(&program_headers, dynamic.p_vaddr, dynamic.p_filesz, false).ok()
            != Some(dynamic.p_offset)
        || node.bias.checked_add(dynamic.p_vaddr) != Some(node.dynamic)
    {
        return Ok(None);
    }

    let mut file_loads = Vec::new();
    for load in program_headers
        .iter()
        .filter(|segment| segment.p_type == ph::PT_LOAD && segment.p_filesz != 0)
    {
        let start = node
            .bias
            .checked_add(load.p_vaddr)
            .ok_or_else(|| invalid("target load address overflow"))?;
        let end = start
            .checked_add(load.p_filesz)
            .ok_or_else(|| invalid("target load range overflow"))?;
        file_loads.push(DynamicFileLoad {
            start,
            end,
            offset: load.p_offset,
            read: load.p_flags & ph::PF_R != 0,
            write: load.p_flags & ph::PF_W != 0,
        });
    }
    let dynamic_table_end = node
        .dynamic
        .checked_add(dynamic.p_filesz)
        .ok_or_else(|| invalid("target dynamic table range overflow"))?;
    let image = DynamicImage {
        node,
        identity,
        dynamic_mapping_start,
        dynamic_mapping_end,
        dynamic_table_end,
        file_loads,
    };
    if dynamic_range_reason(memory, &image, node.dynamic, dynamic.p_filesz).is_err() {
        return Ok(None);
    }
    Ok(Some(image))
}

fn dynamic_image<F: FnMut(u64, &mut [u8]) -> io::Result<()>>(
    memory: &mut Memory<'_, F>,
    node: LinkNode,
    vdso_ehdr: Option<u64>,
) -> io::Result<DynamicImage> {
    let mapping = memory.mapping(node.dynamic)?;
    if mapping.write || !mapping.private {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "target dynamic table is not private read-only memory: node={:#x} l_ld={:#x} map=[{:#x},{:#x}) read={} write={} execute={} private={}",
                node.address,
                node.dynamic,
                mapping.start,
                mapping.end,
                mapping.read,
                mapping.write,
                mapping.execute,
                mapping.private,
            ),
        ));
    }
    let image_end = node
        .bias
        .checked_add(MAX_FILE as u64)
        .ok_or_else(|| invalid("target dynamic image window overflows"))?;
    if node.dynamic < node.bias || node.dynamic >= image_end {
        return Err(invalid("target dynamic table is outside image window"));
    }
    let identity = mapping.identity;
    let dynamic_mapping_start = mapping.start;
    let dynamic_mapping_end = mapping.end;
    if identity.2 == 0 {
        let header_address =
            vdso_ehdr.ok_or_else(|| invalid("anonymous link-map node is not the auxv vDSO"))?;
        let header_mapping = memory.mapping(header_address)?;
        if header_mapping.start != dynamic_mapping_start
            || header_mapping.end != dynamic_mapping_end
        {
            return Err(invalid(
                "vDSO ELF header and dynamic table are not in one VMA",
            ));
        }
        return dynamic_file_candidate(
            memory,
            node,
            identity,
            dynamic_mapping_start,
            dynamic_mapping_end,
            header_address,
        )?
        .ok_or_else(|| invalid("vDSO dynamic table disagrees with its ELF load geometry"));
    }

    let header_addresses = memory
        .maps
        .iter()
        .filter(|candidate| {
            candidate.identity == identity
                && candidate.offset == 0
                && candidate.read
                && !candidate.write
                && candidate.private
                && candidate.start >= node.bias
                && candidate.start < image_end
                && candidate.end - candidate.start >= 64
        })
        .map(|candidate| candidate.start)
        .collect::<Vec<_>>();
    let mut candidates = Vec::new();
    for header_address in header_addresses {
        if let Some(candidate) = dynamic_file_candidate(
            memory,
            node,
            identity,
            dynamic_mapping_start,
            dynamic_mapping_end,
            header_address,
        )? {
            candidates.push(candidate);
        }
    }
    if candidates.len() != 1 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "target dynamic table has no unique ELF load instance: node={:#x} l_ld={:#x} bias={:#x} identity={identity:?} candidates={}",
                node.address,
                node.dynamic,
                node.bias,
                candidates.len(),
            ),
        ));
    }
    Ok(candidates.pop().unwrap())
}

fn dynamic_range_reason<F: FnMut(u64, &mut [u8]) -> io::Result<()>>(
    memory: &Memory<'_, F>,
    image: &DynamicImage,
    address: u64,
    length: u64,
) -> Result<(), String> {
    let end = address
        .checked_add(length)
        .ok_or_else(|| "range overflow".to_owned())?;
    let image_end = image
        .node
        .bias
        .checked_add(MAX_FILE as u64)
        .ok_or_else(|| "image window overflow".to_owned())?;
    if length == 0 || address < image.node.bias || end > image_end {
        return Err(format!(
            "range [{address:#x},{end:#x}) is outside image window [{:#x},{image_end:#x})",
            image.node.bias,
        ));
    }
    let mut at = address;
    while at < end {
        let Some(mapping) = memory
            .maps
            .iter()
            .find(|mapping| mapping.start <= at && at < mapping.end)
        else {
            return Err(format!("address {at:#x} is unmapped"));
        };
        if !mapping.read {
            return Err(format!(
                "address {at:#x} is non-readable in [{:#x},{:#x})",
                mapping.start, mapping.end,
            ));
        }
        if mapping.write || !mapping.private {
            return Err(format!(
                "address {at:#x} has mutable/shared mapping [{:#x},{:#x})",
                mapping.start, mapping.end,
            ));
        }
        if mapping.identity != image.identity {
            return Err(format!(
                "address {at:#x} has identity {:?}, expected {:?}",
                mapping.identity, image.identity,
            ));
        }
        if image.identity.2 == 0
            && (mapping.start != image.dynamic_mapping_start
                || mapping.end != image.dynamic_mapping_end)
        {
            return Err(format!(
                "anonymous address {at:#x} is outside l_ld VMA [{:#x},{:#x})",
                image.dynamic_mapping_start, image.dynamic_mapping_end,
            ));
        }
        let Some(load) = image
            .file_loads
            .iter()
            .find(|load| load.read && address >= load.start && end <= load.end)
        else {
            return Err(format!(
                "range [{address:#x},{end:#x}) is outside this load instance's file-backed PT_LOAD ranges",
            ));
        };
        let actual_offset = mapping
            .offset
            .checked_add(at - mapping.start)
            .ok_or_else(|| "mapped file offset overflow".to_owned())?;
        let expected_offset = load
            .offset
            .checked_add(at - load.start)
            .ok_or_else(|| "expected PT_LOAD file offset overflow".to_owned())?;
        if actual_offset != expected_offset {
            return Err(format!(
                "address {at:#x} has file offset {actual_offset:#x}, expected PT_LOAD offset {expected_offset:#x}",
            ));
        }
        at = mapping.end.min(end);
    }
    Ok(())
}

fn require_relocation_slot<F: FnMut(u64, &mut [u8]) -> io::Result<()>>(
    memory: &Memory<'_, F>,
    image: &DynamicImage,
    address: u64,
) -> io::Result<()> {
    let end = add(address, 8)?;
    let load = image
        .file_loads
        .iter()
        .find(|load| load.read && load.write && address >= load.start && end <= load.end)
        .ok_or_else(|| {
            invalid("target environment relocation slot is outside writable PT_LOAD file bytes")
        })?;
    let mut at = address;
    while at < end {
        let mapping = memory.mapping(at)?;
        if mapping.identity != image.identity || mapping.execute || !mapping.private {
            return Err(invalid(
                "target environment relocation slot mapping geometry disagrees",
            ));
        }
        let actual_offset = add(mapping.offset, at - mapping.start)?;
        let expected_offset = add(load.offset, at - load.start)?;
        if actual_offset != expected_offset {
            return Err(invalid(
                "target environment relocation slot file offset disagrees",
            ));
        }
        at = mapping.end.min(end);
    }
    Ok(())
}

fn require_dynamic_range<F: FnMut(u64, &mut [u8]) -> io::Result<()>>(
    memory: &Memory<'_, F>,
    image: &DynamicImage,
    address: u64,
    length: u64,
    stage: &'static str,
) -> io::Result<()> {
    dynamic_range_reason(memory, image, address, length).map_err(|reason| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "{stage} is outside authenticated dynamic image: node={:#x} l_ld={:#x} bias={:#x} range=[{address:#x},{:#x}) reason={reason}",
                image.node.address,
                image.node.dynamic,
                image.node.bias,
                address.saturating_add(length),
            ),
        )
    })
}

fn resolve_dynamic_pointer<F: FnMut(u64, &mut [u8]) -> io::Result<()>>(
    memory: &Memory<'_, F>,
    image: &DynamicImage,
    mode: &mut Option<DynamicPointerMode>,
    tag: &'static str,
    raw: u64,
    length: u64,
) -> io::Result<u64> {
    let direct = dynamic_range_reason(memory, image, raw, length);
    let rebased_address = image.node.bias.checked_add(raw);
    let rebased = match rebased_address {
        Some(address) if address == raw => None,
        Some(address) => Some((
            address,
            dynamic_range_reason(memory, image, address, length),
        )),
        None => Some((0, Err("checked bias addition overflow".to_owned()))),
    };

    let mut valid = Vec::new();
    if direct.is_ok() {
        valid.push((DynamicPointerMode::Direct, raw));
    }
    if let Some((address, result)) = &rebased
        && result.is_ok()
    {
        valid.push((DynamicPointerMode::Rebased, *address));
    }
    if valid.len() != 1 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "{tag} has no unique authenticated runtime pointer: node={:#x} l_ld={:#x} bias={:#x} raw={raw:#x} direct={} rebased={} length={length}",
                image.node.address,
                image.node.dynamic,
                image.node.bias,
                direct
                    .as_ref()
                    .err()
                    .map_or("valid".to_owned(), Clone::clone),
                rebased.as_ref().map_or_else(
                    || "same-as-direct".to_owned(),
                    |(address, result)| format!(
                        "{address:#x}:{}",
                        result
                            .as_ref()
                            .err()
                            .map_or("valid".to_owned(), Clone::clone)
                    )
                ),
            ),
        ));
    }
    let (selected_mode, address) = valid[0];
    if let Some(prior) = *mode
        && prior != selected_mode
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "{tag} mixes runtime dynamic pointer representations: node={:#x} prior={prior:?} current={selected_mode:?}",
                image.node.address,
            ),
        ));
    }
    *mode = Some(selected_mode);
    Ok(address)
}

fn read_dynamic<F: FnMut(u64, &mut [u8]) -> io::Result<()>>(
    memory: &mut Memory<'_, F>,
    node: LinkNode,
    vdso_ehdr: Option<u64>,
) -> io::Result<(Vec<(u64, u64)>, DynamicImage)> {
    let image = dynamic_image(memory, node, vdso_ehdr)?;
    let mut entries = Vec::new();
    for index in 0..MAX_DYNAMIC {
        let address = add(node.dynamic, (index * 16) as u64)?;
        if add(address, 16)? > image.dynamic_table_end {
            return Err(invalid("target dynamic table lacks bounded terminator"));
        }
        require_dynamic_range(memory, &image, address, 16, "target dynamic entry")?;
        let entry = memory.get(address, 16)?;
        let tag = u64_at(&entry, 0);
        let value = u64_at(&entry, 8);
        entries.push((tag, value));
        if tag == dynamic::DT_NULL {
            return Ok((entries, image));
        }
    }
    Err(invalid("target dynamic table exceeds environment bound"))
}

fn unique_target_tag(entries: &[(u64, u64)], key: u64) -> io::Result<Option<u64>> {
    let mut selected = None;
    for (_, value) in entries.iter().filter(|(tag, _)| *tag == key) {
        if selected.replace(*value).is_some() {
            return Err(invalid("duplicate target environment dynamic tag"));
        }
    }
    Ok(selected)
}

#[derive(Clone, Copy)]
struct RelocationTable {
    address: u64,
    size: u64,
    entry_size: u64,
    rela: bool,
}

fn relocation_tables<F: FnMut(u64, &mut [u8]) -> io::Result<()>>(
    memory: &Memory<'_, F>,
    entries: &[(u64, u64)],
    image: &DynamicImage,
    mode: &mut Option<DynamicPointerMode>,
) -> io::Result<Vec<RelocationTable>> {
    let mut tables = Vec::new();
    let rela = unique_target_tag(entries, dynamic::DT_RELA)?;
    let rela_size = unique_target_tag(entries, dynamic::DT_RELASZ)?;
    if rela.is_some() != rela_size.is_some() {
        return Err(invalid("incomplete target RELA table"));
    }
    if let (Some(address), Some(size)) = (rela, rela_size) {
        let entry = unique_target_tag(entries, dynamic::DT_RELAENT)?.unwrap_or(24);
        if entry != 24 || size % entry != 0 {
            return Err(invalid("invalid target RELA table width"));
        }
        tables.push(RelocationTable {
            address: resolve_dynamic_pointer(
                memory,
                image,
                mode,
                "DT_RELA",
                address,
                size.max(entry),
            )?,
            size,
            entry_size: entry,
            rela: true,
        });
    }
    let rel = unique_target_tag(entries, dynamic::DT_REL)?;
    let rel_size = unique_target_tag(entries, dynamic::DT_RELSZ)?;
    if rel.is_some() != rel_size.is_some() {
        return Err(invalid("incomplete target REL table"));
    }
    if let (Some(address), Some(size)) = (rel, rel_size) {
        let entry = unique_target_tag(entries, dynamic::DT_RELENT)?.unwrap_or(16);
        if entry != 16 || size % entry != 0 {
            return Err(invalid("invalid target REL table width"));
        }
        tables.push(RelocationTable {
            address: resolve_dynamic_pointer(
                memory,
                image,
                mode,
                "DT_REL",
                address,
                size.max(entry),
            )?,
            size,
            entry_size: entry,
            rela: false,
        });
    }
    let jump = unique_target_tag(entries, dynamic::DT_JMPREL)?;
    let jump_size = unique_target_tag(entries, dynamic::DT_PLTRELSZ)?;
    if jump.is_some() != jump_size.is_some() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "incomplete target PLT relocation table: DT_JMPREL={jump:?} DT_PLTRELSZ={jump_size:?}"
            ),
        ));
    }
    if let (Some(address), Some(size)) = (jump, jump_size) {
        let kind = unique_target_tag(entries, dynamic::DT_PLTREL)?
            .ok_or_else(|| invalid("missing target PLT relocation kind"))?;
        let (entry_size, rela) = match kind {
            dynamic::DT_RELA => (24, true),
            dynamic::DT_REL => (16, false),
            _ => return Err(invalid("unsupported target PLT relocation kind")),
        };
        if size % entry_size != 0 {
            return Err(invalid("invalid target PLT relocation width"));
        }
        tables.push(RelocationTable {
            address: resolve_dynamic_pointer(
                memory,
                image,
                mode,
                "DT_JMPREL",
                address,
                size.max(entry_size),
            )?,
            size,
            entry_size,
            rela,
        });
    }
    for (index, table) in tables.iter().enumerate() {
        if table.size / table.entry_size > MAX_GRAPH_RELOCATIONS as u64 {
            return Err(invalid("target relocation table exceeds environment bound"));
        }
        let end = add(table.address, table.size)?;
        for prior in &tables[..index] {
            let prior_end = add(prior.address, prior.size)?;
            if table.address < prior_end && prior.address < end {
                return Err(invalid("target relocation tables overlap"));
            }
        }
    }
    Ok(tables)
}

fn target_symbol_name<F: FnMut(u64, &mut [u8]) -> io::Result<()>>(
    memory: &mut Memory<'_, F>,
    image: &DynamicImage,
    symbol_table: u64,
    string_table: u64,
    string_size: u64,
    index: usize,
) -> io::Result<(String, goblin::elf::sym::Sym)> {
    if index >= 65536 {
        return Err(invalid("target environment symbol index exceeds bound"));
    }
    let record_address = add(symbol_table, (index * 24) as u64)?;
    require_dynamic_range(
        memory,
        image,
        record_address,
        24,
        "target dynamic symbol record",
    )?;
    let record = memory.get(record_address, 24)?;
    let name_offset = u32_at(&record, 0) as u64;
    if name_offset >= string_size {
        return Err(invalid(
            "target environment symbol name is outside string table",
        ));
    }
    let available = usize::try_from((string_size - name_offset).min(4096))
        .map_err(|_| invalid("target environment string bound overflow"))?;
    let mut name = Vec::new();
    let mut at = add(string_table, name_offset)?;
    while name.len() < available {
        let mapping = memory.mapping(at)?;
        let amount = usize::try_from((mapping.end - at).min(64))
            .map_err(|_| invalid("target symbol string chunk overflow"))?
            .min(available - name.len());
        if amount == 0 {
            return Err(invalid("target symbol string made no progress"));
        }
        require_dynamic_range(
            memory,
            image,
            at,
            amount as u64,
            "target dynamic symbol string",
        )?;
        let chunk = memory.get(at, amount)?;
        if let Some(end) = chunk.iter().position(|byte| *byte == 0) {
            name.extend_from_slice(&chunk[..end]);
            break;
        }
        name.extend_from_slice(&chunk);
        at = add(at, amount as u64)?;
    }
    if name.len() == available {
        return Err(invalid("unterminated target environment symbol name"));
    }
    let name = std::str::from_utf8(&name)
        .map_err(|_| invalid("invalid target environment symbol name"))?
        .to_owned();
    Ok((
        name,
        goblin::elf::sym::Sym {
            st_name: name_offset as usize,
            st_info: record[4],
            st_other: record[5],
            st_shndx: u16_at(&record, 6) as usize,
            st_value: u64_at(&record, 8),
            st_size: u64_at(&record, 16),
        },
    ))
}

fn observe_alias_relocations<F: FnMut(u64, &mut [u8]) -> io::Result<()>>(
    memory: &mut Memory<'_, F>,
    provider_link_map: u64,
    object_address: u64,
    vdso_ehdr: Option<u64>,
) -> io::Result<Vec<TargetEnvironmentRelocation>> {
    let nodes = link_nodes(memory, provider_link_map)?;
    let mut result = Vec::new();
    let mut slots = BTreeSet::new();
    let mut total = 0_usize;
    for node in nodes {
        let (entries, image) = read_dynamic(memory, node, vdso_ehdr)?;
        let mut pointer_mode = None;
        let tables = relocation_tables(memory, &entries, &image, &mut pointer_mode)?;
        if tables.is_empty() {
            continue;
        }
        let string_size = unique_target_tag(&entries, dynamic::DT_STRSZ)?
            .ok_or_else(|| invalid("missing target environment string size"))?;
        if unique_target_tag(&entries, dynamic::DT_SYMENT)?.unwrap_or(24) != 24
            || string_size == 0
            || string_size > MAX_FILE as u64
        {
            return Err(invalid("invalid target environment symbol metadata"));
        }
        let symbol_table = resolve_dynamic_pointer(
            memory,
            &image,
            &mut pointer_mode,
            "DT_SYMTAB",
            unique_target_tag(&entries, dynamic::DT_SYMTAB)?
                .ok_or_else(|| invalid("missing target environment symbol table"))?,
            24,
        )?;
        let string_table = resolve_dynamic_pointer(
            memory,
            &image,
            &mut pointer_mode,
            "DT_STRTAB",
            unique_target_tag(&entries, dynamic::DT_STRTAB)?
                .ok_or_else(|| invalid("missing target environment string table"))?,
            string_size,
        )?;
        let mut symbols: BTreeMap<usize, (String, goblin::elf::sym::Sym)> = BTreeMap::new();
        for table in tables {
            for index in 0..table.size / table.entry_size {
                total += 1;
                if total > MAX_GRAPH_RELOCATIONS {
                    return Err(invalid("target graph relocation count exceeds bound"));
                }
                let bytes = memory.get(
                    add(table.address, index * table.entry_size)?,
                    table.entry_size as usize,
                )?;
                let info = u64_at(&bytes, 8);
                let symbol_index = usize::try_from(info >> 32)
                    .map_err(|_| invalid("target relocation symbol index overflow"))?;
                let kind = info as u32;
                let (name, symbol) = if let Some(symbol) = symbols.get(&symbol_index) {
                    symbol.clone()
                } else {
                    let symbol = target_symbol_name(
                        memory,
                        &image,
                        symbol_table,
                        string_table,
                        string_size,
                        symbol_index,
                    )?;
                    symbols.insert(symbol_index, symbol.clone());
                    symbol
                };
                if !ENVIRONMENT_NAMES.contains(&name.as_str()) {
                    continue;
                }
                if kind == reloc::R_X86_64_COPY {
                    return Err(invalid("target graph contains environment COPY relocation"));
                }
                if kind != reloc::R_X86_64_GLOB_DAT
                    || !table.rela
                    || i64::from_le_bytes(bytes[16..24].try_into().unwrap()) != 0
                    || symbol.st_type() != sym::STT_OBJECT
                    || !matches!(symbol.st_bind(), sym::STB_GLOBAL | sym::STB_WEAK)
                    || symbol.st_other != sym::STV_DEFAULT
                {
                    return Err(invalid(
                        "target graph has unsupported environment relocation",
                    ));
                }
                let slot = add(node.bias, u64_at(&bytes, 0))?;
                if !slots.insert((node.address, slot)) {
                    return Err(invalid("duplicate target environment relocation slot"));
                }
                require_relocation_slot(memory, &image, slot)?;
                let word = read_word(memory, slot)?;
                if word.value != object_address {
                    return Err(invalid(
                        "target environment relocation is interposed or unresolved",
                    ));
                }
                result.push(TargetEnvironmentRelocation {
                    link_map: node.address,
                    load_bias: node.bias,
                    dynamic: node.dynamic,
                    symbol: name,
                    slot,
                    value: word.value,
                    addend: 0,
                    mappings: word.mappings,
                });
            }
        }
    }
    if result.is_empty() {
        return Err(invalid("target graph has no environment relocation"));
    }
    Ok(result)
}

fn expected_environment(
    expected: &BTreeMap<OsString, OsString>,
) -> io::Result<BTreeMap<Vec<u8>, Vec<u8>>> {
    if expected.len() > MAX_ENVIRONMENT_ENTRIES {
        return Err(invalid("expected environment pointer count exceeds bound"));
    }
    let mut total = 0_usize;
    let mut result = BTreeMap::new();
    for (key, value) in expected {
        let key = key.as_os_str().as_bytes();
        let value = value.as_os_str().as_bytes();
        if key.is_empty() || key.contains(&0) || key.contains(&b'=') || value.contains(&0) {
            return Err(invalid("expected environment contains invalid bytes"));
        }
        total = total
            .checked_add(key.len() + value.len() + 2)
            .ok_or_else(|| invalid("expected environment byte count overflow"))?;
        if total > MAX_ENVIRONMENT_BYTES {
            return Err(invalid("expected environment byte count exceeds bound"));
        }
        if result.insert(key.to_vec(), value.to_vec()).is_some() {
            return Err(invalid("expected environment contains duplicate key"));
        }
    }
    Ok(result)
}

fn read_environment_string<F: FnMut(u64, &mut [u8]) -> io::Result<()>>(
    memory: &mut Memory<'_, F>,
    address: u64,
    remaining: usize,
) -> io::Result<Vec<u8>> {
    if address == 0 || remaining == 0 {
        return Err(invalid("environment string address or budget is invalid"));
    }
    let mut bytes = Vec::new();
    let mut at = address;
    while bytes.len() < remaining {
        let mapping = memory.mapping(at)?;
        let amount = usize::try_from((mapping.end - at).min(4096))
            .map_err(|_| invalid("environment string chunk overflow"))?
            .min(remaining - bytes.len());
        if amount == 0 {
            return Err(invalid("environment string made no progress"));
        }
        let chunk = memory.get(at, amount)?;
        if let Some(end) = chunk.iter().position(|byte| *byte == 0) {
            bytes.extend_from_slice(&chunk[..=end]);
            return Ok(bytes);
        }
        bytes.extend_from_slice(&chunk);
        at = add(at, amount as u64)?;
    }
    Err(invalid("unterminated environment string exceeds bound"))
}

fn parse_environment_entry(bytes: &[u8]) -> io::Result<(Vec<u8>, Vec<u8>)> {
    let content = bytes
        .strip_suffix(&[0])
        .ok_or_else(|| invalid("environment string lacks terminator"))?;
    let equals = content
        .iter()
        .position(|byte| *byte == b'=')
        .ok_or_else(|| invalid("environment string lacks equals"))?;
    if equals == 0 {
        return Err(invalid("environment string has empty key"));
    }
    Ok((content[..equals].to_vec(), content[equals + 1..].to_vec()))
}

fn observe_pointer_graph<F: FnMut(u64, &mut [u8]) -> io::Result<()>>(
    memory: &mut Memory<'_, F>,
    object: &TargetEnvironmentWord,
    expected: &BTreeMap<Vec<u8>, Vec<u8>>,
) -> io::Result<ObservedPointerGraph> {
    let array = object.value;
    if array == 0 {
        if expected.is_empty() {
            return Ok((0, Vec::new(), Vec::new(), Vec::new(), BTreeMap::new()));
        }
        return Err(invalid(
            "nonempty expected environment has NULL libc object",
        ));
    }

    let mut pointer_bytes = Vec::new();
    let mut pointers = Vec::new();
    for index in 0..=MAX_ENVIRONMENT_ENTRIES {
        let address = add(array, (index * 8) as u64)?;
        let bytes = memory.get(address, 8)?;
        let pointer = u64_at(&bytes, 0);
        pointer_bytes.extend_from_slice(&bytes);
        if pointer == 0 {
            break;
        }
        if index == MAX_ENVIRONMENT_ENTRIES {
            return Err(invalid(
                "environment pointer array lacks bounded terminator",
            ));
        }
        pointers.push((address, pointer));
    }
    if pointer_bytes
        .last_chunk::<8>()
        .is_none_or(|last| u64::from_le_bytes(*last) != 0)
    {
        return Err(invalid("environment pointer array is unterminated"));
    }
    let pointer_mappings = mappings_for_range(memory, array, pointer_bytes.len())?;

    let mut aggregate = 0_usize;
    let mut entries = Vec::new();
    let mut parsed = BTreeMap::new();
    for (pointer_slot, string_address) in pointers {
        let remaining = MAX_ENVIRONMENT_BYTES
            .checked_sub(aggregate)
            .ok_or_else(|| invalid("environment byte budget underflow"))?;
        let bytes = read_environment_string(memory, string_address, remaining)?;
        aggregate = aggregate
            .checked_add(bytes.len())
            .ok_or_else(|| invalid("environment byte count overflow"))?;
        let mappings = mappings_for_range(memory, string_address, bytes.len())?;
        let (key, value) = parse_environment_entry(&bytes)?;
        if parsed.insert(key.clone(), value.clone()).is_some() {
            return Err(invalid("active environment contains duplicate key"));
        }
        entries.push(TargetEnvironmentEntry {
            pointer_slot,
            string_address,
            bytes,
            key,
            value,
            mappings,
        });
    }
    if &parsed != expected {
        return Err(invalid("active libc environment differs from expected map"));
    }
    Ok((array, pointer_bytes, pointer_mappings, entries, parsed))
}

fn observe_environment_inner<F: FnMut(u64, &mut [u8]) -> io::Result<()>>(
    memory: &mut Memory<'_, F>,
    aux: &BTreeMap<u64, u64>,
    environment: &EnvironmentProvider<'_>,
    tid: i32,
    start_ticks: u64,
    expected: &BTreeMap<Vec<u8>, Vec<u8>>,
) -> io::Result<TargetEnvironmentGraph> {
    let mut resolved = resolve(memory, aux, &environment.provider)?;
    resolved.tid = tid;
    resolved.start_ticks = start_ticks;
    let writable_load = observe_writable_load(memory, &resolved, &environment.writable_load)?;

    let aliases = environment
        .aliases
        .iter()
        .map(|alias| {
            Ok(TargetEnvironmentAlias {
                name: alias.name.clone(),
                address: add(resolved.load_bias, alias.rva)?,
                strong: alias.strong,
            })
        })
        .collect::<io::Result<Vec<_>>>()?;
    let object_address = aliases
        .iter()
        .map(|alias| alias.address)
        .next()
        .ok_or_else(|| invalid("active environment has no object alias"))?;
    if aliases.iter().any(|alias| alias.address != object_address) {
        return Err(invalid("active environment aliases diverged"));
    }
    let environ_object = read_word(memory, object_address)?;

    let relocations = observe_alias_relocations(
        memory,
        resolved.link_map,
        object_address,
        aux.get(&libc::AT_SYSINFO_EHDR).copied(),
    )?;
    let got_address = add(resolved.load_bias, environment.getenv_got_rva)?;
    let matching_got = relocations
        .iter()
        .filter(|relocation| {
            relocation.link_map == resolved.link_map
                && relocation.slot == got_address
                && relocation.symbol == "__environ"
        })
        .cloned()
        .collect::<Vec<_>>();
    if matching_got.len() != 1 {
        return Err(invalid(
            "authenticated getenv GOT relocation is absent or ambiguous",
        ));
    }
    let getenv_got = matching_got[0].clone();
    let getenv_address = add(resolved.load_bias, environment.getenv_rva)?;
    let getenv_mapping = memory.mapping(getenv_address)?;
    if getenv_mapping.identity != resolved.mapping_identity
        || !getenv_mapping.execute
        || getenv_mapping.write
        || !getenv_mapping.private
    {
        return Err(invalid("target getenv is not exact private libc code"));
    }

    let (pointer_array_address, pointer_array_bytes, pointer_array_mappings, entries, parsed) =
        observe_pointer_graph(memory, &environ_object, expected)?;
    let bookkeeping = environment
        .bookkeeping
        .map(|bookkeeping| -> io::Result<TargetEnvironmentBookkeeping> {
            Ok(TargetEnvironmentBookkeeping {
                counter: read_word(memory, add(resolved.load_bias, bookkeeping.counter_rva)?)?,
                allocation_list: read_word(
                    memory,
                    add(resolved.load_bias, bookkeeping.allocation_list_rva)?,
                )?,
            })
        })
        .transpose()?;

    Ok(TargetEnvironmentGraph {
        tid,
        start_ticks,
        executable_phdr: resolved.executable_phdr,
        libc_link_map: resolved.link_map,
        libc_load_bias: resolved.load_bias,
        libc_mapping_identity: resolved.mapping_identity,
        getenv_address,
        aliases,
        relocations,
        getenv_got,
        writable_load,
        environ_object,
        pointer_array_address,
        pointer_array_bytes,
        pointer_array_mappings,
        entries,
        parsed,
        bookkeeping,
    })
}

fn observe_environment(
    task: &Stopped,
    expected_libc: &[u8],
    expected: &BTreeMap<OsString, OsString>,
) -> io::Result<TargetEnvironmentGraph> {
    let tid = task.pid().as_raw();
    let before = Snapshot::read(tid)?;
    let maps = parse_maps(&before.maps)?;
    let aux = parse_auxv(&before.auxv)?;
    let expected = expected_environment(expected)?;
    let provider = EnvironmentProvider::parse(expected_libc)?;
    let mut memory = Memory::new(&maps, |address, bytes: &mut [u8]| {
        let address = usize::try_from(address).map_err(|_| invalid("target address overflow"))?;
        task.read_exact(address, bytes)
            .map_err(|error| io::Error::other(format!("read environment at {address:#x}: {error}")))
    });
    let graph = observe_environment_inner(
        &mut memory,
        &aux,
        &provider,
        tid,
        before.start_ticks,
        &expected,
    )?;
    // This rereads all dynamic metadata, link-map nodes, mappings' contents,
    // pointer words, strings and bookkeeping observed above.
    memory.recheck()?;
    if Snapshot::read(tid)? != before {
        return Err(invalid(
            "target environment or process identity changed during observation",
        ));
    }
    Ok(graph)
}

/// Capture the baseline immediately before the first private target call.
/// All tasks sharing this address space must remain quiescent throughout.
pub(crate) fn observe_environment_before(
    task: &Stopped,
    expected_libc: &[u8],
    expected: &BTreeMap<OsString, OsString>,
) -> io::Result<TargetEnvironmentBefore> {
    observe_environment(task, expected_libc, expected).map(TargetEnvironmentBefore)
}

/// Renew the complete environment observation after a target transition.
///
/// The caller must keep every task sharing the address space quiescent during
/// each observation, and must renew rather than reuse coordinates after every
/// resume. `TargetEnvironmentBefore::compare_exact` performs the cross-stop
/// comparison; this function's readback only proves stability within one stop.
pub(crate) fn observe_environment_after(
    task: &Stopped,
    expected_libc: &[u8],
    expected: &BTreeMap<OsString, OsString>,
) -> io::Result<TargetEnvironmentAfter> {
    observe_environment(task, expected_libc, expected).map(TargetEnvironmentAfter)
}

#[cfg(test)]
mod tests;
