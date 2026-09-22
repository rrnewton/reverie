//! The explicit host initializer has a separate ordinary-export policy.
//! None of the versioned public dlopen parser's checks are changed.
use super::*;

const INITIALIZER: &str = "reverie_liteinst_initialize_host";
const MAX_RUNTIME_FILE: usize = 64 * 1024 * 1024;

fn runtime_file_bound(length: usize) -> io::Result<()> {
    if length > MAX_RUNTIME_FILE {
        return Err(invalid("runtime exceeds file bound"));
    }
    Ok(())
}

/// An exact ordinary initializer observed in a stopped target image.
/// Coordinates expire on any address-space resume, just as for TargetDlopen.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TargetHostInitializer {
    /// Observed stopped TID.
    pub tid: i32,
    /// Kernel start ticks for that TID.
    pub start_ticks: u64,
    /// Original executable program headers.
    pub executable_phdr: u64,
    /// Exact runtime node in the default link map.
    pub link_map: u64,
    /// Runtime load bias.
    pub load_bias: u64,
    /// Normal, unversioned initializer address.
    pub address: u64,
    /// Observed backing device/inode tuple.
    pub mapping_identity: (u64, u64, u64),
}

/// One exact file-backed RX page that the controller has intentionally isolated.
///
/// The caller must independently authenticate pkey, path and bytes. This type
/// only projects an exact current no-access maps record back to its original RX
/// shape for one stopped-target resolver transaction.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct TargetIsolatedRxPage {
    start: u64,
    end: u64,
    offset: u64,
    identity: (u64, u64, u64),
}

impl TargetIsolatedRxPage {
    pub(crate) fn new(
        start: u64,
        end: u64,
        offset: u64,
        identity: (u64, u64, u64),
    ) -> Option<Self> {
        (end.checked_sub(start) == Some(4096)
            && start.is_multiple_of(4096)
            && offset.is_multiple_of(4096)
            && identity.2 != 0)
            .then_some(Self {
                start,
                end,
                offset,
                identity,
            })
    }
}

fn project_isolated_rx_page(maps: &mut [Map], isolated: TargetIsolatedRxPage) -> io::Result<()> {
    let mut matched = None;
    for (index, mapping) in maps.iter().enumerate() {
        if mapping.start == isolated.start
            && mapping.end == isolated.end
            && mapping.offset == isolated.offset
            && mapping.identity == isolated.identity
            && !mapping.read
            && !mapping.write
            && !mapping.execute
            && mapping.private
            && matched.replace(index).is_some()
        {
            return Err(invalid(
                "isolated runtime page matches more than one maps record",
            ));
        }
    }
    let matched = matched.ok_or_else(|| {
        invalid("isolated runtime page does not name one exact private no-access mapping")
    })?;
    maps[matched].read = true;
    maps[matched].execute = true;
    Ok(())
}

/// Resolve the exact unversioned host initializer without running target code.
///
/// The controller supplies independently bound runtime bytes. All tasks sharing
/// this image must remain stopped. The complete provider/map/readback contract
/// of resolve_dlopen applies; this does not authorize executing the result.
pub fn resolve_host_initializer(
    task: &Stopped,
    expected_runtime: &[u8],
) -> io::Result<TargetHostInitializer> {
    resolve_host_initializer_with_options(task, expected_runtime, None, |address, bytes| {
        let address = usize::try_from(address).map_err(|_| invalid("address overflow"))?;
        task.read_exact(address, bytes).map_err(io::Error::other)
    })
}

pub(crate) fn resolve_host_initializer_with_isolated_page(
    task: &Stopped,
    expected_runtime: &[u8],
    isolated: TargetIsolatedRxPage,
    read_memory: impl FnMut(u64, &mut [u8]) -> io::Result<()>,
) -> io::Result<TargetHostInitializer> {
    resolve_host_initializer_with_options(task, expected_runtime, Some(isolated), read_memory)
}

fn resolve_host_initializer_with_options(
    task: &Stopped,
    expected_runtime: &[u8],
    isolated: Option<TargetIsolatedRxPage>,
    mut read_memory: impl FnMut(u64, &mut [u8]) -> io::Result<()>,
) -> io::Result<TargetHostInitializer> {
    let tid = task.pid().as_raw();
    let before = Snapshot::read(tid)?;
    let mut maps = parse_maps(&before.maps)?;
    if let Some(isolated) = isolated {
        project_isolated_rx_page(&mut maps, isolated)?;
    }
    let aux = parse_auxv(&before.auxv)?;
    let provider = parse_runtime(expected_runtime)?;
    let mut memory = Memory::new(&maps, |address, bytes: &mut [u8]| {
        read_memory(address, bytes)
    });
    // Reuse the unchanged complete link-map traversal and provider comparison.
    let resolved = resolve(&mut memory, &aux, &provider)?;
    memory.recheck()?;
    if Snapshot::read(tid)? != before {
        return Err(invalid("initializer target changed during observation"));
    }
    Ok(TargetHostInitializer {
        tid,
        start_ticks: before.start_ticks,
        executable_phdr: resolved.executable_phdr,
        link_map: resolved.link_map,
        load_bias: resolved.load_bias,
        address: resolved.address,
        mapping_identity: resolved.mapping_identity,
    })
}

fn parse_runtime(bytes: &[u8]) -> io::Result<Provider<'_>> {
    // The existing staged Rust runtime is about 35 MiB including debug data.
    // This separate policy does not widen dlopen's 32 MiB libc/libdl limit.
    runtime_file_bound(bytes.len())?;
    let elf = Elf::parse(bytes).map_err(|_| invalid("malformed runtime ELF"))?;
    if !elf.is_64
        || !elf.little_endian
        || elf.header.e_machine != header::EM_X86_64
        || elf.header.e_type != header::ET_DYN
        || elf.header.e_ehsize != 64
        || elf.header.e_version != 1
        || elf.header.e_phentsize != 56
        || elf.program_headers.len() > 128
    {
        return Err(invalid("runtime is not supported x86-64 ELF"));
    }
    validate_loads(&elf.program_headers)?;
    for p in &elf.program_headers {
        if p.p_type == ph::PT_LOAD {
            range(bytes, p.p_offset, p.p_filesz)?;
        }
    }
    let segments: Vec<_> = elf
        .program_headers
        .iter()
        .filter(|p| p.p_type == ph::PT_DYNAMIC)
        .collect();
    if segments.len() != 1 {
        return Err(invalid("ambiguous runtime dynamic segment"));
    }
    let segment = segments[0];
    let (dynamic, dynamic_offset, dynamic_size) =
        (segment.p_vaddr, segment.p_offset, segment.p_filesz);
    if dynamic_size == 0
        || dynamic_size % 16 != 0
        || dynamic_size / 16 > MAX_DYNAMIC as u64
        || load_file_offset(&elf.program_headers, dynamic, dynamic_size, false)? != dynamic_offset
    {
        return Err(invalid("runtime dynamic layout invalid"));
    }
    range(bytes, dynamic_offset, dynamic_size)?;
    let tags = elf
        .dynamic
        .as_ref()
        .ok_or_else(|| invalid("missing runtime dynamic table"))?;
    if !tags
        .dyns
        .last()
        .is_some_and(|d| d.d_tag == dynamic::DT_NULL)
        || tags.info.textrel
    {
        return Err(invalid(
            "runtime has text relocation or unterminated dynamic table",
        ));
    }
    let mut unique = BTreeMap::new();
    for d in &tags.dyns {
        if [
            dynamic::DT_STRTAB,
            dynamic::DT_STRSZ,
            dynamic::DT_SYMTAB,
            dynamic::DT_SYMENT,
            dynamic::DT_VERSYM,
            dynamic::DT_FLAGS,
            dynamic::DT_HASH,
            dynamic::DT_GNU_HASH,
        ]
        .contains(&d.d_tag)
            && unique.insert(d.d_tag, d.d_val).is_some()
        {
            return Err(invalid("duplicate runtime symbol metadata"));
        }
    }
    let tag = |key| {
        unique
            .get(&key)
            .copied()
            .ok_or_else(|| invalid("missing runtime symbol metadata"))
    };
    if tag(dynamic::DT_SYMENT)? != 24
        || elf.dynsyms.is_empty()
        || elf.dynsyms.len() > 65536
        || unique.get(&dynamic::DT_FLAGS).copied().unwrap_or(0) & dynamic::DF_TEXTREL != 0
    {
        return Err(invalid("unsupported runtime symbols or text relocations"));
    }
    let strings = Provider::ro_file(
        &elf,
        bytes,
        tag(dynamic::DT_STRTAB)?,
        tag(dynamic::DT_STRSZ)?,
    )?;
    let records = Provider::ro_file(
        &elf,
        bytes,
        tag(dynamic::DT_SYMTAB)?,
        (elf.dynsyms.len() * 24) as u64,
    )?;
    let versions = unique
        .get(&dynamic::DT_VERSYM)
        .map(|address| Provider::ro_file(&elf, bytes, *address, (elf.dynsyms.len() * 2) as u64))
        .transpose()?;
    let mut selected = None;
    for (index, symbol) in elf.dynsyms.iter().enumerate() {
        let record = &records[index * 24..(index + 1) * 24];
        if symbol.st_name != u32_at(record, 0) as usize
            || symbol.st_info != record[4]
            || symbol.st_other != record[5]
            || symbol.st_shndx != u16_at(record, 6) as usize
            || symbol.st_value != u64_at(record, 8)
            || symbol.st_size != u64_at(record, 16)
        {
            return Err(invalid("runtime dynamic symbol table disagreement"));
        }
        if c_string(strings, symbol.st_name)? != INITIALIZER {
            continue;
        }
        if symbol.st_bind() != sym::STB_GLOBAL
            || symbol.st_type() != sym::STT_FUNC
            || symbol.st_other != sym::STV_DEFAULT
            || symbol.st_shndx == 0
            || symbol.st_shndx >= 0xff00
            || symbol.st_size == 0
            || symbol.st_size > 1024 * 1024
            || versions.is_some_and(|v| u16_at(v, index * 2) != 1)
        {
            return Err(invalid(
                "initializer is not an ordinary unversioned global function",
            ));
        }
        Provider::ro_file(&elf, bytes, symbol.st_value, symbol.st_size)?;
        if !elf.program_headers.iter().any(|p| {
            p.p_type == ph::PT_LOAD
                && p.p_flags == (ph::PF_R | ph::PF_X)
                && symbol.st_value >= p.p_vaddr
                && add(symbol.st_value, symbol.st_size)
                    .ok()
                    .zip(add(p.p_vaddr, p.p_filesz).ok())
                    .is_some_and(|(a, b)| a <= b)
        }) {
            return Err(invalid("initializer is outside exact executable bytes"));
        }
        if selected.replace(symbol).is_some() {
            return Err(invalid("ambiguous ordinary initializer"));
        }
    }
    Ok(Provider {
        bytes,
        elf,
        symbol: selected.ok_or_else(|| invalid("ordinary initializer absent"))?,
        version: "unversioned",
        dynamic,
        dynamic_offset,
        dynamic_size,
    })
}

pub(crate) fn validate_host_runtime_elf(bytes: &[u8]) -> io::Result<()> {
    parse_runtime(bytes).map(drop)
}

#[cfg(test)]
mod tests;
