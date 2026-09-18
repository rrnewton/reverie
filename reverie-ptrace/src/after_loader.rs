//! Inputs and retained diagnostics for the experimental one-task host caller.
use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::ffi::OsString;
use std::io::Read;
use std::io::Seek;
use std::io::Write;
use std::io::{self};
use std::os::fd::AsRawFd;
use std::os::fd::FromRawFd;
use std::os::unix::fs::MetadataExt;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex;

use goblin::elf::Elf;
use goblin::elf::dynamic;
use goblin::elf::header;
use goblin::elf::program_header as ph;
use goblin::elf::reloc;
use goblin::elf::section_header;
use goblin::elf::sym;
use sha2::Digest;
use sha2::Sha256;

pub(crate) const MAX_CALLER_FILE: usize = 32 * 1024 * 1024;
pub(crate) const MAX_RUNTIME_FILE: usize = 64 * 1024 * 1024;
pub(crate) const MAX_RUNTIME_LOAD_SPAN: u64 = 128 * 1024 * 1024;
const RUNTIME_LOAD_PAGE: u64 = 4096;
const MAX_STAGE_MARKER: usize = 1024;
const HOST_INITIALIZER: &str = "reverie_liteinst_initialize_host";
const LEGACY_INITIALIZER: &str = "reverie_liteinst_initialize";
pub(crate) const RUNTIME_SEALS: i32 =
    libc::F_SEAL_WRITE | libc::F_SEAL_GROW | libc::F_SEAL_SHRINK | libc::F_SEAL_SEAL;

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) struct FileIdentity {
    pub(crate) device: u64,
    pub(crate) inode: u64,
}

impl FileIdentity {
    pub(crate) fn from_metadata(metadata: &std::fs::Metadata) -> Self {
        Self {
            device: metadata.dev(),
            inode: metadata.ino(),
        }
    }
}

#[derive(Clone, Debug)]
struct StageMarker {
    path: PathBuf,
    bytes: Arc<[u8]>,
    file_identity: FileIdentity,
}

/// A controller-bound file in the fixed loader/runtime dependency graph.
#[derive(Clone, Debug)]
pub struct LiteinstCallerImage {
    pub(crate) path: PathBuf,
    pub(crate) bytes: Arc<[u8]>,
    pub(crate) file_identity: FileIdentity,
    marker: Option<StageMarker>,
}
impl LiteinstCallerImage {
    /// Bind one expected executable, libc or loader dependency, capped at 32 MiB.
    pub fn read(path: impl AsRef<Path>) -> io::Result<Self> {
        Self::read_bounded(path.as_ref(), MAX_CALLER_FILE)
    }

    pub(crate) fn dynamic_soname(&self) -> Option<String> {
        Elf::parse(&self.bytes).ok()?.soname.map(str::to_owned)
    }

    /// Produce the canonical stage marker for a qualified constructor-disabled
    /// runtime. Staging can call this directly instead of reproducing the
    /// schema or invoking an external hex-dump utility. ELF inspection proves
    /// the initializer properties; the staging caller remains responsible for
    /// making the recorded Cargo feature claims match its build invocation.
    pub fn runtime_stage_marker(runtime: &[u8]) -> io::Result<Vec<u8>> {
        if runtime.len() > MAX_RUNTIME_FILE {
            return Err(io::Error::other("staged runtime exceeds byte bound"));
        }
        validate_runtime_elf(runtime)?;
        Ok(canonical_stage_marker(runtime))
    }

    /// Bind the separate constructor-disabled runtime and its retained stage
    /// marker. Only this runtime input permits an ELF up to 64 MiB.
    ///
    /// The marker is retained byte for byte after its canonical build claims,
    /// exact byte length and SHA-256 digest have been checked. Those claims are
    /// trusted staging-producer evidence. Independently, the runtime must export
    /// the explicit host initializer and must not put the legacy initializer in
    /// its dynamic initializer array.
    pub fn read_runtime(path: impl AsRef<Path>, marker: impl AsRef<Path>) -> io::Result<Self> {
        let mut image = Self::read_bounded(path.as_ref(), MAX_RUNTIME_FILE)?;
        let path = marker.as_ref().canonicalize()?;
        let mut file = std::fs::File::open(&path)?;
        let before = file.metadata()?;
        let mut bytes = Vec::new();
        (&mut file)
            .take(MAX_STAGE_MARKER as u64 + 1)
            .read_to_end(&mut bytes)?;
        if !before.is_file()
            || bytes.is_empty()
            || bytes.len() > MAX_STAGE_MARKER
            || file_stamp(&before) != file_stamp(&file.metadata()?)
            || file_stamp(&before) != file_stamp(&std::fs::metadata(&path)?)
        {
            return Err(io::Error::other(
                "missing, oversized or changed runtime stage marker",
            ));
        }
        validate_runtime_elf(&image.bytes)?;
        validate_stage_marker(&bytes, &image.bytes)?;
        image.marker = Some(StageMarker {
            path,
            bytes: bytes.into(),
            file_identity: FileIdentity::from_metadata(&before),
        });
        Ok(image)
    }

    /// Exact runtime marker bytes retained during input binding.
    pub fn runtime_marker(&self) -> Option<&[u8]> {
        self.marker.as_ref().map(|marker| marker.bytes.as_ref())
    }

    fn read_bounded(path: &Path, limit: usize) -> io::Result<Self> {
        let path = path.canonicalize()?;
        let mut file = std::fs::File::open(&path)?;
        let before = file.metadata()?;
        let mut bytes = Vec::new();
        (&mut file).take(limit as u64 + 1).read_to_end(&mut bytes)?;
        if !before.is_file()
            || bytes.len() > limit
            || !bytes.starts_with(b"\x7fELF")
            || file_stamp(&before) != file_stamp(&file.metadata()?)
            || file_stamp(&before) != file_stamp(&std::fs::metadata(&path)?)
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "caller input is not bounded ELF",
            ));
        }
        Ok(Self {
            path,
            bytes: bytes.into(),
            file_identity: FileIdentity::from_metadata(&before),
            marker: None,
        })
    }
}

fn validate_stage_marker(marker: &[u8], runtime: &[u8]) -> io::Result<()> {
    let marker = std::str::from_utf8(marker)
        .map_err(|_| io::Error::other("runtime stage marker is not canonical UTF-8"))?;
    let body = marker
        .strip_suffix('\n')
        .ok_or_else(|| io::Error::other("runtime stage marker lacks final newline"))?;
    if body.contains('\r') {
        return Err(io::Error::other(
            "runtime stage marker contains non-canonical line endings",
        ));
    }
    let fields: Vec<_> = body.split('\n').collect();
    if fields.len() != 6 {
        return Err(io::Error::other(
            "runtime stage marker has missing or extra fields",
        ));
    }
    if fields[0] != "schema=1" {
        return Err(io::Error::other("unsupported runtime stage marker schema"));
    }
    let length = fields[1]
        .strip_prefix("dso_bytes=")
        .ok_or_else(|| io::Error::other("runtime stage marker lacks DSO byte length"))?;
    let parsed_length = length
        .parse::<usize>()
        .map_err(|_| io::Error::other("runtime stage marker has invalid DSO byte length"))?;
    if parsed_length.to_string() != length || parsed_length != runtime.len() {
        return Err(io::Error::other(
            "runtime stage marker DSO byte length differs",
        ));
    }
    let digest = fields[2]
        .strip_prefix("dso_sha256=")
        .ok_or_else(|| io::Error::other("runtime stage marker lacks DSO SHA-256"))?;
    if digest.len() != 64
        || !digest
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        || digest != format!("{:x}", Sha256::digest(runtime))
    {
        return Err(io::Error::other("runtime stage marker DSO SHA-256 differs"));
    }
    if fields[3] != "default_features=false"
        || fields[4] != "features=[liteinst-after-loader-experiment]"
        || fields[5] != "preload_constructor=false"
    {
        return Err(io::Error::other(
            "runtime stage marker build features differ",
        ));
    }
    Ok(())
}

fn canonical_stage_marker(runtime: &[u8]) -> Vec<u8> {
    format!(
        "schema=1\ndso_bytes={}\ndso_sha256={:x}\ndefault_features=false\nfeatures=[liteinst-after-loader-experiment]\npreload_constructor=false\n",
        runtime.len(),
        Sha256::digest(runtime)
    )
    .into_bytes()
}

fn validate_runtime_elf(bytes: &[u8]) -> io::Result<()> {
    let elf = Elf::parse(bytes).map_err(|_| io::Error::other("malformed staged runtime ELF"))?;
    if !elf.is_64
        || !elf.little_endian
        || elf.header.e_machine != header::EM_X86_64
        || elf.header.e_type != header::ET_DYN
        || elf.program_headers.len() > 128
    {
        return Err(io::Error::other("unsupported staged runtime ELF"));
    }
    validate_runtime_load_geometry(&elf, bytes.len())?;
    crate::target_loader::validate_host_runtime_elf(bytes)?;

    let mut host_initializer = None;
    let mut legacy_addresses = Vec::new();
    let mut legacy_symbols = Vec::new();
    for (index, symbol) in elf.dynsyms.iter().enumerate() {
        let Some(name) = elf.dynstrtab.get_at(symbol.st_name) else {
            continue;
        };
        if name == HOST_INITIALIZER {
            if symbol.st_bind() != sym::STB_GLOBAL
                || symbol.st_type() != sym::STT_FUNC
                || symbol.st_other != sym::STV_DEFAULT
                || symbol.st_shndx == section_header::SHN_UNDEF as usize
                || symbol.st_shndx >= section_header::SHN_LORESERVE as usize
                || symbol.st_value == 0
                || symbol.st_size == 0
                || !symbol_is_file_backed_executable(&elf, &symbol)
                || host_initializer.replace(index).is_some()
            {
                return Err(io::Error::other(
                    "staged runtime host initializer is not one ordinary export",
                ));
            }
        }
        if name == LEGACY_INITIALIZER {
            legacy_symbols.push(index);
            if symbol.st_shndx != section_header::SHN_UNDEF as usize {
                legacy_addresses.push(symbol.st_value);
            }
        }
    }
    if host_initializer.is_none() {
        return Err(io::Error::other(
            "staged runtime host initializer export is absent",
        ));
    }
    reject_legacy_init_array(&elf, bytes, &legacy_symbols, &legacy_addresses)
}

fn validate_runtime_load_geometry(elf: &Elf<'_>, file_length: usize) -> io::Result<(u64, u64)> {
    let file_length = u64::try_from(file_length)
        .map_err(|_| io::Error::other("staged runtime byte length is not representable"))?;
    let loads = elf
        .program_headers
        .iter()
        .filter(|header| header.p_type == ph::PT_LOAD)
        .collect::<Vec<_>>();
    if loads.is_empty() {
        return Err(io::Error::other("staged runtime has no PT_LOAD"));
    }

    let mut first = u64::MAX;
    let mut last = 0;
    let mut rounded_loads: Vec<(u64, u64, u32, u64, u64)> = Vec::with_capacity(loads.len());
    for (index, load) in loads.iter().enumerate() {
        let file_end = load
            .p_offset
            .checked_add(load.p_filesz)
            .ok_or_else(|| io::Error::other("staged runtime PT_LOAD file range overflow"))?;
        let memory_end = load
            .p_vaddr
            .checked_add(load.p_memsz)
            .ok_or_else(|| io::Error::other("staged runtime PT_LOAD memory range overflow"))?;
        if load.p_filesz > load.p_memsz
            || file_end > file_length
            || load.p_flags & !(ph::PF_R | ph::PF_W | ph::PF_X) != 0
            || load.p_flags & ph::PF_W == 0 && load.p_flags & ph::PF_R == 0
            || load.p_flags & (ph::PF_W | ph::PF_X) == (ph::PF_W | ph::PF_X)
            || load.p_vaddr % RUNTIME_LOAD_PAGE != load.p_offset % RUNTIME_LOAD_PAGE
            || load.p_align > 1
                && (!load.p_align.is_power_of_two()
                    || load.p_vaddr % load.p_align != load.p_offset % load.p_align)
        {
            return Err(io::Error::other(
                "staged runtime PT_LOAD geometry is invalid",
            ));
        }
        if loads[..index].iter().any(|prior| {
            prior
                .p_vaddr
                .checked_add(prior.p_memsz)
                .is_none_or(|prior_end| load.p_vaddr < prior_end && prior.p_vaddr < memory_end)
        }) {
            return Err(io::Error::other(
                "staged runtime PT_LOAD memory ranges overlap",
            ));
        }

        let page_start = load.p_vaddr & !(RUNTIME_LOAD_PAGE - 1);
        let page_end = memory_end
            .checked_add(RUNTIME_LOAD_PAGE - 1)
            .map(|end| end & !(RUNTIME_LOAD_PAGE - 1))
            .ok_or_else(|| io::Error::other("staged runtime PT_LOAD page range overflow"))?;
        let file_memory_end = load
            .p_vaddr
            .checked_add(load.p_filesz)
            .ok_or_else(|| io::Error::other("staged runtime PT_LOAD memory range overflow"))?;
        let file_page_end = if load.p_filesz == 0 {
            page_start
        } else {
            file_memory_end
                .checked_add(RUNTIME_LOAD_PAGE - 1)
                .map(|end| end & !(RUNTIME_LOAD_PAGE - 1))
                .ok_or_else(|| io::Error::other("staged runtime PT_LOAD page range overflow"))?
                .min(page_end)
        };
        let file_page_start = load.p_offset & !(RUNTIME_LOAD_PAGE - 1);
        for &(prior_start, prior_end, prior_flags, prior_file_end, prior_file_start) in
            &rounded_loads
        {
            let overlap_start = page_start.max(prior_start);
            let overlap_end = page_end.min(prior_end);
            if overlap_start >= overlap_end {
                continue;
            }
            let prior_file_overlap_end = prior_file_end.clamp(overlap_start, overlap_end);
            let file_overlap_end = file_page_end.clamp(overlap_start, overlap_end);
            let file_projection_matches = if file_overlap_end == overlap_start {
                true
            } else {
                let prior_offset = prior_file_start
                    .checked_add(overlap_start - prior_start)
                    .ok_or_else(|| {
                        io::Error::other("staged runtime PT_LOAD page projection overflow")
                    })?;
                let offset = file_page_start
                    .checked_add(overlap_start - page_start)
                    .ok_or_else(|| {
                        io::Error::other("staged runtime PT_LOAD page projection overflow")
                    })?;
                prior_offset == offset
            };
            if prior_flags != load.p_flags
                || prior_file_overlap_end != file_overlap_end
                || !file_projection_matches
            {
                return Err(io::Error::other(
                    "staged runtime PT_LOAD page mappings are incompatible",
                ));
            }
        }
        rounded_loads.push((
            page_start,
            page_end,
            load.p_flags,
            file_page_end,
            file_page_start,
        ));
        first = first.min(page_start);
        last = last.max(page_end);
    }
    let span = last
        .checked_sub(first)
        .ok_or_else(|| io::Error::other("staged runtime PT_LOAD span underflow"))?;
    if span > MAX_RUNTIME_LOAD_SPAN {
        return Err(io::Error::other(
            "staged runtime PT_LOAD span exceeds private mmap admission",
        ));
    }
    Ok((first, last))
}

fn symbol_is_file_backed_executable(elf: &Elf<'_>, symbol: &goblin::elf::Sym) -> bool {
    symbol
        .st_value
        .checked_add(symbol.st_size)
        .is_some_and(|end| {
            elf.program_headers.iter().any(|load| {
                load.p_type == ph::PT_LOAD
                    && load.p_flags == (ph::PF_R | ph::PF_X)
                    && symbol.st_value >= load.p_vaddr
                    && load
                        .p_vaddr
                        .checked_add(load.p_filesz)
                        .is_some_and(|load_end| end <= load_end)
            })
        })
}

fn reject_legacy_init_array(
    elf: &Elf<'_>,
    bytes: &[u8],
    legacy_symbols: &[usize],
    legacy_addresses: &[u64],
) -> io::Result<()> {
    let Some(dynamic) = elf.dynamic.as_ref() else {
        return Err(io::Error::other("staged runtime has no dynamic table"));
    };
    let mut array_address = None;
    let mut array_size = None;
    for entry in &dynamic.dyns {
        match entry.d_tag {
            dynamic::DT_INIT_ARRAY => {
                if array_address.replace(entry.d_val).is_some() {
                    return Err(io::Error::other("duplicate runtime init-array address"));
                }
            }
            dynamic::DT_INIT_ARRAYSZ => {
                if array_size.replace(entry.d_val).is_some() {
                    return Err(io::Error::other("duplicate runtime init-array size"));
                }
            }
            _ => {}
        }
    }
    let (array_address, array_size) = match (array_address, array_size) {
        (None, None) => return Ok(()),
        (Some(address), Some(0)) if address == 0 => return Ok(()),
        (Some(address), Some(size)) if address != 0 && size != 0 && size % 8 == 0 => {
            (address, size)
        }
        _ => return Err(io::Error::other("runtime init-array tags are inconsistent")),
    };
    if array_size > 1024 * 1024 {
        return Err(io::Error::other("runtime init array exceeds bound"));
    }
    let array_offset = file_offset(elf, array_address, array_size)?;
    let array_size = usize::try_from(array_size)
        .map_err(|_| io::Error::other("runtime init-array size overflow"))?;
    let array_end = array_offset
        .checked_add(array_size)
        .ok_or_else(|| io::Error::other("runtime init-array file range overflow"))?;
    let array = bytes
        .get(array_offset..array_end)
        .ok_or_else(|| io::Error::other("runtime init array is outside staged bytes"))?;

    for (slot, raw) in array.chunks_exact(8).enumerate() {
        let slot_address = array_address
            .checked_add((slot * 8) as u64)
            .ok_or_else(|| io::Error::other("runtime init-array address overflow"))?;
        let raw = u64::from_le_bytes(raw.try_into().unwrap());
        let relocations: Vec<_> = elf
            .dynrelas
            .iter()
            .chain(elf.dynrels.iter())
            .chain(elf.pltrelocs.iter())
            .filter(|relocation| relocation.r_offset == slot_address)
            .collect();
        if relocations.len() > 1 {
            return Err(io::Error::other(
                "runtime init-array slot has ambiguous relocations",
            ));
        }
        let target = match relocations.first().copied() {
            None => raw,
            Some(relocation) if legacy_symbols.contains(&relocation.r_sym) => {
                return Err(io::Error::other(
                    "legacy runtime initializer appears in init array",
                ));
            }
            Some(relocation)
                if matches!(
                    relocation.r_type,
                    reloc::R_X86_64_RELATIVE | reloc::R_X86_64_RELATIVE64
                ) =>
            {
                relocation.r_addend.map(|value| value as u64).unwrap_or(raw)
            }
            Some(relocation) if relocation.r_type == reloc::R_X86_64_64 => {
                let symbol = elf.dynsyms.get(relocation.r_sym).ok_or_else(|| {
                    io::Error::other("runtime init-array relocation symbol is invalid")
                })?;
                symbol
                    .st_value
                    .checked_add(relocation.r_addend.map(|value| value as u64).unwrap_or(raw))
                    .ok_or_else(|| io::Error::other("runtime init-array target overflow"))?
            }
            Some(_) => {
                return Err(io::Error::other(
                    "runtime init-array relocation is not supported",
                ));
            }
        };
        if legacy_addresses.contains(&target) {
            return Err(io::Error::other(
                "legacy runtime initializer appears in init array",
            ));
        }
    }
    Ok(())
}

fn file_offset(elf: &Elf<'_>, address: u64, length: u64) -> io::Result<usize> {
    let end = address
        .checked_add(length)
        .ok_or_else(|| io::Error::other("runtime file range overflow"))?;
    let load = elf
        .program_headers
        .iter()
        .find(|load| {
            load.p_type == ph::PT_LOAD
                && address >= load.p_vaddr
                && load
                    .p_vaddr
                    .checked_add(load.p_filesz)
                    .is_some_and(|load_end| end <= load_end)
        })
        .ok_or_else(|| io::Error::other("runtime range is not file backed"))?;
    usize::try_from(
        load.p_offset
            .checked_add(address - load.p_vaddr)
            .ok_or_else(|| io::Error::other("runtime file offset overflow"))?,
    )
    .map_err(|_| io::Error::other("runtime file offset is not representable"))
}

fn file_stamp(m: &std::fs::Metadata) -> (u64, u64, u64, i64, i64, i64, i64) {
    (
        m.dev(),
        m.ino(),
        m.len(),
        m.mtime(),
        m.mtime_nsec(),
        m.ctime(),
        m.ctime_nsec(),
    )
}

/// The controller owns this descriptor until all configuration clones are
/// dropped. The target opens its proc path and independently proves the same
/// sealed inode before calling dlopen through its own descriptor.
#[derive(Debug)]
pub(crate) struct SealedRuntime {
    pub(crate) file: std::fs::File,
    pub(crate) image: LiteinstCallerImage,
}
impl SealedRuntime {
    fn prepare(stage: &LiteinstCallerImage) -> io::Result<Self> {
        let marker = stage
            .marker
            .as_ref()
            .ok_or_else(|| io::Error::other("runtime stage marker absent"))?;
        let rebound = LiteinstCallerImage::read_runtime(&stage.path, &marker.path)?;
        let rebound_marker = rebound.marker.as_ref().unwrap();
        if rebound.bytes != stage.bytes
            || rebound.file_identity != stage.file_identity
            || rebound_marker.bytes != marker.bytes
            || rebound_marker.file_identity != marker.file_identity
        {
            return Err(io::Error::other(
                "runtime stage or marker changed before sealing",
            ));
        }
        // SAFETY: static NUL-terminated name, supported memfd flags, and exactly
        // one File owner is constructed for a successfully returned descriptor.
        let fd = unsafe {
            libc::memfd_create(
                c"liteinst-after-loader-runtime".as_ptr(),
                libc::MFD_CLOEXEC | libc::MFD_ALLOW_SEALING,
            )
        };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        let mut file = unsafe { std::fs::File::from_raw_fd(fd) };
        file.write_all(&stage.bytes)?;
        if unsafe { libc::fcntl(file.as_raw_fd(), libc::F_ADD_SEALS, RUNTIME_SEALS) } != 0 {
            return Err(io::Error::last_os_error());
        }
        if unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GET_SEALS) } != RUNTIME_SEALS {
            return Err(io::Error::other("runtime seals differ"));
        }
        file.rewind()?;
        let mut bytes = Vec::new();
        (&mut file)
            .take(MAX_RUNTIME_FILE as u64 + 1)
            .read_to_end(&mut bytes)?;
        if bytes.as_slice() != stage.bytes.as_ref() {
            return Err(io::Error::other("sealed runtime readback differs"));
        }
        let metadata = file.metadata()?;
        let image = LiteinstCallerImage {
            path: PathBuf::from(format!(
                "/proc/{}/fd/{}",
                std::process::id(),
                file.as_raw_fd()
            )),
            bytes: stage.bytes.clone(),
            file_identity: FileIdentity::from_metadata(&metadata),
            marker: None,
        };
        Ok(Self { file, image })
    }
}

/// One controller observation. This is diagnostic data, never a clock correction.
#[derive(Clone, Debug)]
pub struct LiteinstCallerObservation {
    /// Phase or physical event name.
    pub operation: String,
    /// Existing persistent ptrace clock, when PMU is available.
    pub raw_clock: Option<u64>,
    /// Retained details, including refusals and syscall addresses/results.
    pub detail: String,
}

/// Bounded retained observations shared with the controller.
#[derive(Clone, Debug, Default)]
pub struct LiteinstCallerDiagnostics(Arc<Mutex<DiagnosticState>>);

#[derive(Debug, Default)]
struct DiagnosticState {
    entries: Vec<LiteinstCallerObservation>,
    bytes: usize,
    last_clock: Option<u64>,
}
impl LiteinstCallerDiagnostics {
    /// Copy the actual observations accumulated so far, including on failure.
    pub fn observations(&self) -> Vec<LiteinstCallerObservation> {
        self.0.lock().unwrap().entries.clone()
    }
    pub(crate) fn record(
        &self,
        operation: impl Into<String>,
        raw_clock: Option<u64>,
        detail: impl Into<String>,
    ) -> io::Result<()> {
        let operation = operation.into();
        let detail = detail.into();
        let size = operation
            .len()
            .checked_add(detail.len())
            .ok_or_else(|| io::Error::other("caller diagnostic size overflow"))?;
        let mut state = self.0.lock().unwrap();
        let total = state
            .bytes
            .checked_add(size)
            .ok_or_else(|| io::Error::other("caller diagnostic total overflow"))?;
        if state.entries.len() >= 100_000 || size > 8 * 1024 * 1024 || total > 64 * 1024 * 1024 {
            return Err(io::Error::other("caller diagnostic bound exceeded"));
        }
        if let (Some(before), Some(after)) = (state.last_clock, raw_clock) {
            crate::entry_call::RawClockInterval { before, after }
                .delta()
                .map_err(|_| io::Error::other("persistent ptrace clock went backwards"))?;
        }
        if raw_clock.is_some() {
            state.last_clock = raw_clock;
        }
        state.bytes = total;
        state.entries.push(LiteinstCallerObservation {
            operation,
            raw_clock,
            detail,
        });
        Ok(())
    }
}

/// Exact inputs for the opt-in, single-task after-loader experiment.
///
/// This is not a supported general startup mode. The implementation suspends
/// the existing ptrace Timer across private loader/runtime execution and proves
/// its exact restoration before the guest resumes.
#[derive(Clone, Debug)]
pub struct LiteinstAfterLoaderConfig {
    pub(crate) executable: LiteinstCallerImage,
    pub(crate) provider: LiteinstCallerImage,
    pub(crate) runtime: LiteinstCallerImage,
    pub(crate) sealed_runtime: Arc<SealedRuntime>,
    pub(crate) dependencies: Vec<LiteinstCallerImage>,
    /// Members of `dependencies` that must already be mapped at executable
    /// entry. This is the exact DT_NEEDED/PT_INTERP closure of `executable`,
    /// excluding `provider`, which is stored separately.
    pub(crate) initial_dependencies: Vec<LiteinstCallerImage>,
    /// Members of `dependencies` that are reachable only from `runtime` and
    /// therefore must be absent at entry and become mapped by `dlopen`.
    pub(crate) deferred_dependencies: Vec<LiteinstCallerImage>,
    pub(crate) environment: BTreeMap<OsString, OsString>,
    pub(crate) diagnostics: LiteinstCallerDiagnostics,
    pub(crate) physical_observer: safeptrace::PhysicalEventObserver,
}
impl LiteinstAfterLoaderConfig {
    /// Bind the fixed experiment's executable, libc, constructor-disabled runtime
    /// and complete other loader dependencies.
    ///
    /// # Safety
    /// The independently reviewed graph must contain no guest interposer, audit
    /// callback or IFUNC that can call guest code during the controller's dlopen
    /// or initializer. The runtime must omit its legacy constructor. These are
    /// semantic properties that ELF identity alone cannot prove. The runtime
    /// stage marker binds the exact ELF to the staging producer's canonical
    /// record of a constructor-disabled build with only the after-loader
    /// experiment feature enabled. The producer remains trusted for its Cargo
    /// invocation claims.
    /// The remaining loader graph must remain immutable through the run. This
    /// constructor seals an exact private copy of the runtime; failure to create
    /// or expose that copy makes staging unavailable. Supply the exact fixed
    /// fixture; this does not authorize arbitrary-program execution.
    /// The complete expected environment must be reviewed together with that
    /// graph, including locale/module paths and allocator configuration. The
    /// launcher and stopped image both compare the exact complete environment.
    pub unsafe fn new(
        executable: LiteinstCallerImage,
        provider: LiteinstCallerImage,
        runtime: LiteinstCallerImage,
        dependencies: Vec<LiteinstCallerImage>,
        environment: BTreeMap<OsString, OsString>,
    ) -> io::Result<Self> {
        if dependencies.len() > 32
            || executable.bytes.len() > MAX_CALLER_FILE
            || provider.bytes.len() > MAX_CALLER_FILE
            || dependencies
                .iter()
                .any(|image| image.bytes.len() > MAX_CALLER_FILE)
            || runtime.bytes.len() > MAX_RUNTIME_FILE
        {
            return Err(io::Error::other("caller dependency bound exceeded"));
        }
        if environment.len() > 256
            || environment.iter().any(|(key, value)| {
                let key = key.as_encoded_bytes();
                key.is_empty()
                    || key.contains(&0)
                    || key.contains(&b'=')
                    || value.as_encoded_bytes().contains(&0)
                    || key.starts_with(b"LD_")
                    || key == b"GLIBC_TUNABLES"
                    || key.starts_with(b"MALLOC_")
                    || key == b"GCONV_PATH"
                    || key == b"LOCPATH"
            })
            || environment
                .iter()
                .map(|(k, v)| k.len() + v.len() + 2)
                .sum::<usize>()
                > 1024 * 1024
        {
            return Err(io::Error::other(
                "environment is outside the reviewed fixed loader fixture",
            ));
        }
        let sealed_runtime = Arc::new(SealedRuntime::prepare(&runtime)?);
        let (initial_dependencies, deferred_dependencies) =
            partition_loader_images(&executable, &provider, &runtime, &dependencies)?;
        let diagnostics = LiteinstCallerDiagnostics::default();
        let physical_observer = safeptrace::PhysicalEventObserver::new(
            safeptrace::PhysicalEventObserverConfig::default(),
        )
        .map_err(|_| io::Error::other("physical event observer allocation failed"))?;
        let marker = runtime.marker.as_ref().unwrap();
        diagnostics.record("runtime input sealed", None, format!(
            "stage={} device={} inode={} bytes={} marker={} marker_device={} marker_inode={} marker_bytes={} sealed={} device={} inode={} seals={:#x}",
            runtime.path.display(), runtime.file_identity.device,
            runtime.file_identity.inode, runtime.bytes.len(), marker.path.display(),
            marker.file_identity.device, marker.file_identity.inode, marker.bytes.len(),
            sealed_runtime.image.path.display(), sealed_runtime.image.file_identity.device,
            sealed_runtime.image.file_identity.inode, RUNTIME_SEALS))?;
        diagnostics.record(
            "loader dependency phases bound",
            None,
            format!(
                "initial_count={} deferred_count={} initial_paths={:?} deferred_paths={:?}",
                initial_dependencies.len(),
                deferred_dependencies.len(),
                initial_dependencies
                    .iter()
                    .map(|image| (image.dynamic_soname(), image.path.as_path()))
                    .collect::<Vec<_>>(),
                deferred_dependencies
                    .iter()
                    .map(|image| (image.dynamic_soname(), image.path.as_path()))
                    .collect::<Vec<_>>(),
            ),
        )?;
        Ok(Self {
            executable,
            provider,
            runtime,
            sealed_runtime,
            dependencies,
            initial_dependencies,
            deferred_dependencies,
            environment,
            diagnostics,
            physical_observer,
        })
    }
    pub(crate) fn validate_environment(
        &self,
        actual: &BTreeMap<OsString, OsString>,
    ) -> io::Result<()> {
        if actual != &self.environment {
            return Err(io::Error::other("complete caller environment differs"));
        }
        Ok(())
    }
    /// Retain this handle before starting the tracer, so failures keep evidence.
    pub fn diagnostics(&self) -> LiteinstCallerDiagnostics {
        self.diagnostics.clone()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct LoaderImageContract {
    soname: Option<String>,
    needed: BTreeSet<String>,
    interpreter: Option<PathBuf>,
}

fn valid_loader_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._+-".contains(&byte))
}

fn loader_image_contract(
    image: &LiteinstCallerImage,
    expected_type: u16,
) -> io::Result<LoaderImageContract> {
    let elf = Elf::parse(&image.bytes)
        .map_err(|error| io::Error::other(format!("bound loader ELF parse failed: {error}")))?;
    if !elf.is_64
        || !elf.little_endian
        || elf.header.e_machine != header::EM_X86_64
        || elf.header.e_type != expected_type
        || elf.program_headers.len() > 128
    {
        return Err(io::Error::other(
            "bound loader image is outside the fixed x86-64 contract",
        ));
    }
    let soname = elf.soname.map(str::to_owned);
    if soname
        .as_deref()
        .is_some_and(|name| !valid_loader_name(name))
    {
        return Err(io::Error::other(
            "bound loader image has malformed DT_SONAME",
        ));
    }
    let mut needed = BTreeSet::new();
    for dependency in elf.libraries {
        if !valid_loader_name(dependency) || !needed.insert(dependency.to_owned()) {
            return Err(io::Error::other(
                "bound loader image has malformed or duplicate DT_NEEDED",
            ));
        }
    }
    let interpreter = elf.interpreter.map(PathBuf::from);
    if interpreter.as_ref().is_some_and(|path| !path.is_absolute()) {
        return Err(io::Error::other(
            "bound executable has non-absolute PT_INTERP",
        ));
    }
    Ok(LoaderImageContract {
        soname,
        needed,
        interpreter,
    })
}

fn loader_dependency_closure(
    roots: &BTreeSet<String>,
    graph: &BTreeMap<String, BTreeSet<String>>,
) -> io::Result<BTreeSet<String>> {
    let mut pending = roots.clone();
    let mut reached = BTreeSet::new();
    while let Some(name) = pending.pop_first() {
        if !reached.insert(name.clone()) {
            continue;
        }
        let needed = graph.get(&name).ok_or_else(|| {
            io::Error::other(format!("bound loader graph lacks dependency {name}"))
        })?;
        pending.extend(needed.iter().cloned());
    }
    Ok(reached)
}

/// Partition one complete reviewed union graph without making an absent image
/// optional. Initial names are required before `dlopen`; deferred names are
/// required to be absent then and present after `dlopen`.
fn partition_loader_dependency_names(
    initial_roots: &BTreeSet<String>,
    runtime_roots: &BTreeSet<String>,
    graph: &BTreeMap<String, BTreeSet<String>>,
) -> io::Result<(BTreeSet<String>, BTreeSet<String>)> {
    let initial = loader_dependency_closure(initial_roots, graph)?;
    let runtime = loader_dependency_closure(runtime_roots, graph)?;
    let union = initial.union(&runtime).cloned().collect::<BTreeSet<_>>();
    if union != graph.keys().cloned().collect() {
        return Err(io::Error::other(
            "bound loader graph contains an unreachable dependency",
        ));
    }
    let deferred = runtime.difference(&initial).cloned().collect();
    Ok((initial, deferred))
}

fn partition_loader_images(
    executable: &LiteinstCallerImage,
    provider: &LiteinstCallerImage,
    runtime: &LiteinstCallerImage,
    dependencies: &[LiteinstCallerImage],
) -> io::Result<(Vec<LiteinstCallerImage>, Vec<LiteinstCallerImage>)> {
    let executable_contract = loader_image_contract(executable, header::ET_EXEC)?;
    let runtime_contract = loader_image_contract(runtime, header::ET_DYN)?;
    if runtime_contract.interpreter.is_some() {
        return Err(io::Error::other("bound runtime unexpectedly has PT_INTERP"));
    }

    let mut images = BTreeMap::<String, (&LiteinstCallerImage, LoaderImageContract)>::new();
    for image in std::iter::once(provider).chain(dependencies.iter()) {
        let contract = loader_image_contract(image, header::ET_DYN)?;
        let soname = contract
            .soname
            .clone()
            .ok_or_else(|| io::Error::other("bound loader dependency lacks DT_SONAME"))?;
        if images
            .values()
            .any(|(existing, _)| existing.file_identity == image.file_identity)
        {
            return Err(io::Error::other(
                "bound loader graph reuses one file identity for multiple SONAMEs",
            ));
        }
        if images.insert(soname, (image, contract)).is_some() {
            return Err(io::Error::other(
                "bound loader graph contains duplicate DT_SONAME",
            ));
        }
    }
    let provider_name = images
        .iter()
        .find_map(|(name, (image, _))| std::ptr::eq(*image, provider).then(|| name.clone()))
        .ok_or_else(|| io::Error::other("provider is absent from the bound loader graph"))?;
    let graph = images
        .iter()
        .map(|(name, (_, contract))| (name.clone(), contract.needed.clone()))
        .collect::<BTreeMap<_, _>>();

    let interpreter = executable_contract
        .interpreter
        .as_ref()
        .ok_or_else(|| io::Error::other("bound executable lacks PT_INTERP"))?;
    let interpreter_name = interpreter
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| valid_loader_name(name))
        .ok_or_else(|| io::Error::other("bound PT_INTERP has no canonical loader name"))?
        .to_owned();
    let interpreter_path = interpreter.canonicalize()?;
    if images
        .get(&interpreter_name)
        .map(|(image, _)| image.path.as_path())
        != Some(interpreter_path.as_path())
    {
        return Err(io::Error::other(
            "bound PT_INTERP differs from the loader graph",
        ));
    }

    let mut initial_roots = executable_contract.needed;
    initial_roots.insert(interpreter_name);
    let (initial, deferred) =
        partition_loader_dependency_names(&initial_roots, &runtime_contract.needed, &graph)?;
    if !initial.contains(&provider_name) {
        return Err(io::Error::other(
            "bound dlopen provider is not in the initial loader closure",
        ));
    }
    if runtime_contract
        .soname
        .as_ref()
        .is_some_and(|name| graph.contains_key(name))
    {
        return Err(io::Error::other(
            "bound runtime aliases a loader dependency SONAME",
        ));
    }

    let initial_dependencies = initial
        .iter()
        .filter(|name| name.as_str() != provider_name.as_str())
        .map(|name| images.get(name).unwrap().0.clone())
        .collect();
    let deferred_dependencies = deferred
        .iter()
        .filter(|name| name.as_str() != provider_name.as_str())
        .map(|name| images.get(name).unwrap().0.clone())
        .collect();
    Ok((initial_dependencies, deferred_dependencies))
}

#[cfg(test)]
mod tests;
