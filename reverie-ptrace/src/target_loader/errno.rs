//! Narrow libc TLS accessor for the fixed glibc caller's errno observation.
use super::*;

#[derive(Debug, Eq, PartialEq)]
pub(crate) struct TargetErrnoLocation {
    pub tid: i32,
    pub start_ticks: u64,
    pub executable_phdr: u64,
    pub link_map: u64,
    pub load_bias: u64,
    pub address: u64,
    pub mapping_identity: (u64, u64, u64),
}

pub(crate) fn resolve_errno_location(
    task: &Stopped,
    bytes: &[u8],
) -> io::Result<TargetErrnoLocation> {
    let tid = task.pid().as_raw();
    let before = Snapshot::read(tid)?;
    let maps = parse_maps(&before.maps)?;
    let aux = parse_auxv(&before.auxv)?;
    let provider = parse_errno_provider(bytes)?;
    let mut memory = Memory::new(&maps, |address, value: &mut [u8]| {
        task.read_exact(
            usize::try_from(address).map_err(|_| invalid("address overflow"))?,
            value,
        )
        .map_err(io::Error::other)
    });
    let result = resolve(&mut memory, &aux, &provider)?;
    memory.recheck()?;
    if Snapshot::read(tid)? != before {
        return Err(invalid("errno provider changed"));
    }
    Ok(TargetErrnoLocation {
        tid,
        start_ticks: before.start_ticks,
        executable_phdr: result.executable_phdr,
        link_map: result.link_map,
        load_bias: result.load_bias,
        address: result.address,
        mapping_identity: result.mapping_identity,
    })
}

fn parse_errno_provider(bytes: &[u8]) -> io::Result<Provider<'_>> {
    // First satisfy the complete unchanged libc/dlopen parser, including every
    // raw symbol record, all version definitions and their auxiliary chains.
    // The fixed fixture requires the same actual libc to supply both exports.
    let mut provider = Provider::parse(bytes, "GLIBC_2.34")?;
    if provider.elf.soname != Some("libc.so.6") {
        return Err(invalid("errno provider is not libc"));
    }
    let elf = &provider.elf;
    let tags: BTreeMap<_, _> = elf
        .dynamic
        .as_ref()
        .unwrap()
        .dyns
        .iter()
        .map(|entry| (entry.d_tag, entry.d_val))
        .collect();
    let tag = |key| {
        tags.get(&key)
            .copied()
            .ok_or_else(|| invalid("missing errno metadata"))
    };
    let strings = Provider::ro_file(
        elf,
        bytes,
        tag(dynamic::DT_STRTAB)?,
        tag(dynamic::DT_STRSZ)?,
    )?;
    let versions = Provider::ro_file(
        elf,
        bytes,
        tag(dynamic::DT_VERSYM)?,
        (elf.dynsyms.len() * 2) as u64,
    )?;
    let mut address = tag(dynamic::DT_VERDEF)?;
    let mut selected_version = None;
    for _ in 0..tag(dynamic::DT_VERDEFNUM)? {
        let def = Provider::ro_file(elf, bytes, address, 20)?;
        let aux = Provider::ro_file(elf, bytes, add(address, u32_at(def, 12) as u64)?, 8)?;
        if c_string(strings, u32_at(aux, 0) as usize)? == "GLIBC_2.2.5"
            && selected_version.replace(u16_at(def, 4)).is_some()
        {
            return Err(invalid("ambiguous errno version definition"));
        }
        address = add(address, u32_at(def, 16) as u64)?;
    }
    let version = selected_version.ok_or_else(|| invalid("errno version absent"))?;
    let mut selected = None;
    for (index, symbol) in elf.dynsyms.iter().enumerate() {
        if c_string(strings, symbol.st_name)? != "__errno_location" {
            continue;
        }
        if u16_at(versions, index * 2) != version
            || symbol.st_bind() != sym::STB_GLOBAL
            || symbol.st_type() != sym::STT_FUNC
            || symbol.st_other != sym::STV_DEFAULT
            || symbol.st_shndx == 0
            || symbol.st_shndx >= 0xff00
            || symbol.st_size == 0
            || symbol.st_size > 128
        {
            return Err(invalid(
                "errno accessor is not the exact ordinary default export",
            ));
        }
        let code = Provider::ro_file(elf, bytes, symbol.st_value, symbol.st_size)?;
        if !elf.program_headers.iter().any(|p| {
            p.p_type == ph::PT_LOAD
                && p.p_flags == (ph::PF_R | ph::PF_X)
                && symbol.st_value >= p.p_vaddr
                && add(symbol.st_value, symbol.st_size)
                    .ok()
                    .zip(add(p.p_vaddr, p.p_filesz).ok())
                    .is_some_and(|(a, b)| a <= b)
        }) {
            return Err(invalid("errno accessor is not in exact executable bytes"));
        }
        // This one glibc fixture has an ordinary straight-line accessor. The
        // independent source/disassembly review must prove that its bound bytes
        // only calculate the TLS pointer; the call loop refuses every syscall.
        if code.is_empty() || selected.replace(symbol).is_some() {
            return Err(invalid("ambiguous errno accessor"));
        }
    }
    provider.symbol = selected.ok_or_else(|| invalid("errno accessor absent"))?;
    provider.version = "GLIBC_2.2.5";
    Ok(provider)
}

#[cfg(test)]
mod tests;
