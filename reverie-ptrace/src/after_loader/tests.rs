use std::os::unix::fs::PermissionsExt;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;

use super::*;

const FIFO_CHILD: &str = "REVERIE_LOADER_CACHE_FIFO_CHILD";
const FIFO_CHILD_CACHE: &str = "REVERIE_LOADER_CACHE_FIFO_CHILD_CACHE";
const FIFO_CHILD_DEFERRED: &str = "REVERIE_LOADER_CACHE_FIFO_CHILD_DEFERRED";

fn put16(bytes: &mut [u8], at: usize, value: u16) {
    bytes[at..at + 2].copy_from_slice(&value.to_le_bytes());
}
fn put32(bytes: &mut [u8], at: usize, value: u32) {
    bytes[at..at + 4].copy_from_slice(&value.to_le_bytes());
}
fn put64(bytes: &mut [u8], at: usize, value: u64) {
    bytes[at..at + 8].copy_from_slice(&value.to_le_bytes());
}

#[derive(Clone, Debug)]
struct SyntheticCacheEntry {
    flags: u32,
    key: Vec<u8>,
    value: Vec<u8>,
    osversion: u32,
    hwcap: u64,
}

impl SyntheticCacheEntry {
    fn x86_64(key: impl AsRef<[u8]>, value: impl AsRef<[u8]>) -> Self {
        Self {
            flags: GLIBC_CACHE_ENTRY_FLAGS_X86_64,
            key: key.as_ref().to_vec(),
            value: value.as_ref().to_vec(),
            osversion: 0,
            hwcap: 0,
        }
    }
}

fn synthetic_glibc_cache(entries: &[SyntheticCacheEntry]) -> Vec<u8> {
    let entry_bytes = entries.len() * GLIBC_CACHE_ENTRY_SIZE;
    let string_start = GLIBC_CACHE_HEADER_SIZE + entry_bytes;
    let mut strings = Vec::new();
    let mut offsets = Vec::with_capacity(entries.len());
    for entry in entries {
        let key = string_start + strings.len();
        strings.extend_from_slice(&entry.key);
        strings.push(0);
        let value = string_start + strings.len();
        strings.extend_from_slice(&entry.value);
        strings.push(0);
        offsets.push((key, value));
    }

    let mut bytes = vec![0; string_start + strings.len()];
    bytes[..GLIBC_CACHE_MAGIC.len()].copy_from_slice(GLIBC_CACHE_MAGIC);
    put32(&mut bytes, 20, u32::try_from(entries.len()).unwrap());
    put32(&mut bytes, 24, u32::try_from(strings.len()).unwrap());
    bytes[28] = GLIBC_CACHE_LITTLE_ENDIAN;
    for (index, entry) in entries.iter().enumerate() {
        let at = GLIBC_CACHE_HEADER_SIZE + index * GLIBC_CACHE_ENTRY_SIZE;
        put32(&mut bytes, at, entry.flags);
        put32(&mut bytes, at + 4, u32::try_from(offsets[index].0).unwrap());
        put32(&mut bytes, at + 8, u32::try_from(offsets[index].1).unwrap());
        put32(&mut bytes, at + 12, entry.osversion);
        put64(&mut bytes, at + 16, entry.hwcap);
    }
    bytes[string_start..].copy_from_slice(&strings);
    bytes
}

fn synthetic_glibc_cache_with_extension(mut bytes: Vec<u8>, extra_padding: usize) -> Vec<u8> {
    assert_eq!(extra_padding % 4, 0);
    let extension = ((bytes.len() + 3) & !3) + extra_padding;
    let section = extension + GLIBC_CACHE_EXTENSION_HEADER_SIZE;
    let data = section + GLIBC_CACHE_EXTENSION_SECTION_SIZE;
    bytes.resize(data + 1, 0);
    put32(&mut bytes, 32, u32::try_from(extension).unwrap());
    put32(&mut bytes, extension, GLIBC_CACHE_EXTENSION_MAGIC);
    put32(&mut bytes, extension + 4, 1);
    put32(&mut bytes, section, 0);
    put32(&mut bytes, section + 4, 0);
    put32(&mut bytes, section + 8, u32::try_from(data).unwrap());
    put32(&mut bytes, section + 12, 1);
    bytes[data] = b'g';
    bytes
}

struct CacheAliasInput {
    directory: PathBuf,
    cache: PathBuf,
    deferred: PathBuf,
    alias: PathBuf,
    initial: PathBuf,
    unbound: PathBuf,
}

impl CacheAliasInput {
    fn new() -> Self {
        let directory = std::env::temp_dir().join(format!(
            "liteinst-cache-alias-{}-{}",
            std::process::id(),
            SEQUENCE.fetch_add(1, Ordering::SeqCst)
        ));
        std::fs::create_dir(&directory).unwrap();
        let directory = directory.canonicalize().unwrap();
        let cache = directory.join("ld.so.cache");
        let deferred = directory.join("libdeferred-real.so.1");
        let alias = directory.join("libdeferred.so.1");
        let initial = directory.join("libinitial.so.1");
        let unbound = directory.join("libunbound.so.1");
        std::fs::write(&deferred, b"\x7fELFdeferred-v1").unwrap();
        std::fs::write(&initial, b"\x7fELFinitial-v1").unwrap();
        std::fs::write(&unbound, b"\x7fELFunbound-v1").unwrap();
        std::os::unix::fs::symlink(&deferred, &alias).unwrap();
        Self {
            directory,
            cache,
            deferred,
            alias,
            initial,
            unbound,
        }
    }

    fn deferred_image(&self) -> LiteinstCallerImage {
        LiteinstCallerImage::read(&self.deferred).unwrap()
    }

    fn entry(&self) -> SyntheticCacheEntry {
        SyntheticCacheEntry::x86_64(
            b"libdeferred.so.1",
            self.alias.as_os_str().as_encoded_bytes(),
        )
    }

    fn bind(
        &self,
        bytes: &[u8],
        dependency: &LiteinstCallerImage,
    ) -> io::Result<BTreeMap<PathBuf, LiteinstLoaderCacheAlias>> {
        std::fs::write(&self.cache, bytes)?;
        let cache = LiteinstLoaderCache::read(&self.cache)?;
        bind_glibc_loader_cache_alias_targets(
            &cache,
            &[("libdeferred.so.1".to_owned(), dependency.clone())],
        )
    }

    fn retarget_alias(&self, target: &Path) {
        std::fs::remove_file(&self.alias).unwrap();
        std::os::unix::fs::symlink(target, &self.alias).unwrap();
    }
}

impl Drop for CacheAliasInput {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.alias);
        let _ = std::fs::remove_file(&self.cache);
        let _ = std::fs::remove_file(&self.deferred);
        let _ = std::fs::remove_file(&self.initial);
        let _ = std::fs::remove_file(&self.unbound);
        let _ = std::fs::remove_file(self.directory.join("deferred-old"));
        let _ = std::fs::remove_file(self.directory.join("same-bytes-copy"));
        let _ = std::fs::remove_dir(&self.directory);
    }
}

fn make_fifo(path: &Path) {
    let path = std::ffi::CString::new(path.as_os_str().as_encoded_bytes()).unwrap();
    let result = unsafe { libc::mkfifo(path.as_ptr(), 0o600) };
    assert_eq!(result, 0, "mkfifo failed: {}", io::Error::last_os_error());
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
    for (index, (kind, flags, at, size)) in [
        (ph::PT_LOAD, ph::PF_R, 0, 0x1000),
        (ph::PT_LOAD, ph::PF_R | ph::PF_X, 0x1000, 0x1000),
        (ph::PT_LOAD, ph::PF_R | ph::PF_W, 0x2000, 0x1000),
        (ph::PT_DYNAMIC, ph::PF_R | ph::PF_W, 0x2100, 7 * 16),
    ]
    .into_iter()
    .enumerate()
    {
        let at_header = 64 + index * 56;
        put32(&mut bytes, at_header, kind);
        put32(&mut bytes, at_header + 4, flags);
        put64(&mut bytes, at_header + 8, at);
        put64(&mut bytes, at_header + 16, at);
        put64(&mut bytes, at_header + 32, size);
        put64(&mut bytes, at_header + 40, size);
        put64(&mut bytes, at_header + 48, 8);
    }
    let strings = b"\0reverie_liteinst_initialize_host\0reverie_liteinst_initialize\0";
    bytes[0x500..0x500 + strings.len()].copy_from_slice(strings);
    let legacy_name = b"\0reverie_liteinst_initialize_host\0".len() as u32;
    for (index, (name, address)) in [(1, 0x1100), (legacy_name, 0x1120)].into_iter().enumerate() {
        let symbol = 0x618 + index * 24;
        put32(&mut bytes, symbol, name);
        bytes[symbol + 4] = (sym::STB_GLOBAL << 4) | sym::STT_FUNC;
        put16(&mut bytes, symbol + 6, 1);
        put64(&mut bytes, symbol + 8, address);
        put64(&mut bytes, symbol + 16, 16);
    }
    for (index, value) in [1, 3, 1, 0, 0, 0].into_iter().enumerate() {
        put32(&mut bytes, 0x700 + index * 4, value);
    }
    put16(&mut bytes, 0x742, 1);
    put16(&mut bytes, 0x744, 1);
    bytes[0x1100..0x1110].fill(0x90);
    bytes[0x110f] = 0xc3;
    bytes[0x1120..0x1130].fill(0x90);
    bytes[0x112f] = 0xc3;
    for (index, (tag, value)) in [
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
    let header = 64 + index * 56;
    put32(bytes, header, ph::PT_LOAD);
    put32(bytes, header + 4, flags);
    put64(bytes, header + 8, offset);
    put64(bytes, header + 16, address);
    put64(bytes, header + 32, size);
    put64(bytes, header + 40, size);
    put64(bytes, header + 48, 8);
}

fn runtime_load_span(bytes: &[u8]) -> io::Result<(u64, u64)> {
    let elf = Elf::parse(bytes).map_err(io::Error::other)?;
    validate_runtime_load_geometry(&elf, bytes.len())
}

fn assert_runtime_geometry_rejected(bytes: &[u8]) {
    assert!(runtime_load_span(bytes).is_err());
    assert!(LiteinstCallerImage::runtime_stage_marker(bytes).is_err());
}

#[test]
fn runtime_stage_preflight_bounds_exact_page_rounded_load_geometry() {
    const THIRD_LOAD: usize = 64 + 2 * 56;

    let mut artifact_shape = runtime();
    put64(&mut artifact_shape, THIRD_LOAD + 40, 0x07d6_f702 - 0x2000);
    assert_eq!(
        runtime_load_span(&artifact_shape).unwrap(),
        (0, 0x07d7_0000)
    );
    assert!(LiteinstCallerImage::runtime_stage_marker(&artifact_shape).is_ok());

    let mut exact_limit = runtime();
    put64(
        &mut exact_limit,
        THIRD_LOAD + 40,
        MAX_RUNTIME_LOAD_SPAN - 0x2000,
    );
    assert_eq!(
        runtime_load_span(&exact_limit).unwrap(),
        (0, MAX_RUNTIME_LOAD_SPAN)
    );
    assert!(LiteinstCallerImage::runtime_stage_marker(&exact_limit).is_ok());

    let mut over_limit = exact_limit;
    put64(
        &mut over_limit,
        THIRD_LOAD + 40,
        MAX_RUNTIME_LOAD_SPAN - 0x2000 + 1,
    );
    assert_runtime_geometry_rejected(&over_limit);
}

#[test]
fn runtime_stage_preflight_rejects_invalid_or_overlapping_loads() {
    const SECOND_LOAD: usize = 64 + 56;
    const THIRD_LOAD: usize = 64 + 2 * 56;

    let mut files_exceed_memory = runtime();
    put64(&mut files_exceed_memory, THIRD_LOAD + 40, 0x800);
    assert_runtime_geometry_rejected(&files_exceed_memory);

    let mut file_outside_image = runtime();
    put64(&mut file_outside_image, THIRD_LOAD + 8, 0x3000);
    put64(&mut file_outside_image, THIRD_LOAD + 16, 0x3000);
    assert_runtime_geometry_rejected(&file_outside_image);

    let mut invalid_flags = runtime();
    put32(&mut invalid_flags, THIRD_LOAD + 4, ph::PF_R | ph::PF_W | 8);
    assert_runtime_geometry_rejected(&invalid_flags);

    let mut writable_executable = runtime();
    put32(
        &mut writable_executable,
        THIRD_LOAD + 4,
        ph::PF_R | ph::PF_W | ph::PF_X,
    );
    assert_runtime_geometry_rejected(&writable_executable);

    let mut initializer_in_execute_only_load = runtime();
    put32(
        &mut initializer_in_execute_only_load,
        SECOND_LOAD + 4,
        ph::PF_X,
    );
    assert_runtime_geometry_rejected(&initializer_in_execute_only_load);

    let mut invalid_alignment = runtime();
    put64(&mut invalid_alignment, THIRD_LOAD + 48, 3);
    assert_runtime_geometry_rejected(&invalid_alignment);

    let mut page_incongruent = runtime();
    put64(&mut page_incongruent, THIRD_LOAD + 16, 0x2008);
    assert_runtime_geometry_rejected(&page_incongruent);

    let mut overlap = runtime();
    put64(&mut overlap, THIRD_LOAD + 8, 0x1800);
    put64(&mut overlap, THIRD_LOAD + 16, 0x1800);
    assert_runtime_geometry_rejected(&overlap);

    let mut file_range_overflow = runtime();
    put64(&mut file_range_overflow, THIRD_LOAD + 8, u64::MAX);
    assert_eq!(
        runtime_load_span(&file_range_overflow)
            .unwrap_err()
            .to_string(),
        "staged runtime PT_LOAD file range overflow"
    );
    assert!(LiteinstCallerImage::runtime_stage_marker(&file_range_overflow).is_err());

    let mut address_overflow = runtime();
    put64(&mut address_overflow, THIRD_LOAD + 16, !0xfff);
    assert_eq!(
        runtime_load_span(&address_overflow)
            .unwrap_err()
            .to_string(),
        "staged runtime PT_LOAD memory range overflow"
    );
    assert!(LiteinstCallerImage::runtime_stage_marker(&address_overflow).is_err());

    let mut page_range_overflow = runtime();
    put64(&mut page_range_overflow, THIRD_LOAD + 8, 0x2001);
    put64(&mut page_range_overflow, THIRD_LOAD + 16, u64::MAX - 0xffe);
    put64(&mut page_range_overflow, THIRD_LOAD + 32, 1);
    put64(&mut page_range_overflow, THIRD_LOAD + 40, 1);
    assert_eq!(
        runtime_load_span(&page_range_overflow)
            .unwrap_err()
            .to_string(),
        "staged runtime PT_LOAD page range overflow"
    );
    assert!(LiteinstCallerImage::runtime_stage_marker(&page_range_overflow).is_err());
}

#[test]
fn runtime_stage_rejects_unused_unreadable_and_incompatible_page_loads() {
    let mut readable_extra = runtime();
    append_runtime_load(&mut readable_extra, 4, ph::PF_R, 0, 0x4000, 0x100);
    assert!(runtime_load_span(&readable_extra).is_ok());
    assert!(LiteinstCallerImage::runtime_stage_marker(&readable_extra).is_ok());

    for flags in [0, ph::PF_X] {
        let mut unreadable_extra = runtime();
        append_runtime_load(&mut unreadable_extra, 4, flags, 0, 0x4000, 0x100);
        assert_runtime_geometry_rejected(&unreadable_extra);
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
    assert!(runtime_load_span(&compatible_shared_page).is_ok());
    assert!(LiteinstCallerImage::runtime_stage_marker(&compatible_shared_page).is_ok());

    let mut incompatible_projection = compatible_shared_page.clone();
    put64(&mut incompatible_projection, 64 + 5 * 56 + 8, 0x1800);
    assert_eq!(
        runtime_load_span(&incompatible_projection)
            .unwrap_err()
            .to_string(),
        "staged runtime PT_LOAD page mappings are incompatible"
    );
    assert!(LiteinstCallerImage::runtime_stage_marker(&incompatible_projection).is_err());

    let mut incompatible_shared_page = compatible_shared_page;
    put32(
        &mut incompatible_shared_page,
        64 + 5 * 56 + 4,
        ph::PF_R | ph::PF_X,
    );
    assert_eq!(
        runtime_load_span(&incompatible_shared_page)
            .unwrap_err()
            .to_string(),
        "staged runtime PT_LOAD page mappings are incompatible"
    );
    assert!(LiteinstCallerImage::runtime_stage_marker(&incompatible_shared_page).is_err());
}

#[test]
fn runtime_stage_preflight_reuses_the_target_loader_runtime_contract() {
    const DYNAMIC_LOAD: usize = 64 + 3 * 56;
    let valid = runtime();
    assert!(crate::target_loader::validate_host_runtime_elf(&valid).is_ok());
    assert!(LiteinstCallerImage::runtime_stage_marker(&valid).is_ok());

    let mut invalid_elf_version = runtime();
    put32(&mut invalid_elf_version, 20, 2);
    assert!(crate::target_loader::validate_host_runtime_elf(&invalid_elf_version).is_err());
    assert!(LiteinstCallerImage::runtime_stage_marker(&invalid_elf_version).is_err());

    let mut invalid_header_size = runtime();
    put16(&mut invalid_header_size, 52, 63);
    assert!(crate::target_loader::validate_host_runtime_elf(&invalid_header_size).is_err());
    assert!(LiteinstCallerImage::runtime_stage_marker(&invalid_header_size).is_err());

    let mut invalid_program_header_size = runtime();
    put16(&mut invalid_program_header_size, 54, 55);
    assert!(crate::target_loader::validate_host_runtime_elf(&invalid_program_header_size).is_err());
    assert!(LiteinstCallerImage::runtime_stage_marker(&invalid_program_header_size).is_err());

    let mut versioned_initializer = runtime();
    put16(&mut versioned_initializer, 0x742, 2);
    assert!(crate::target_loader::validate_host_runtime_elf(&versioned_initializer).is_err());
    assert!(LiteinstCallerImage::runtime_stage_marker(&versioned_initializer).is_err());

    let mut text_relocation = runtime();
    put64(&mut text_relocation, DYNAMIC_LOAD + 32, 8 * 16);
    put64(&mut text_relocation, DYNAMIC_LOAD + 40, 8 * 16);
    put64(&mut text_relocation, 0x2160, dynamic::DT_TEXTREL);
    put64(&mut text_relocation, 0x2168, 0);
    put64(&mut text_relocation, 0x2170, dynamic::DT_NULL);
    put64(&mut text_relocation, 0x2178, 0);
    assert!(crate::target_loader::validate_host_runtime_elf(&text_relocation).is_err());
    assert!(LiteinstCallerImage::runtime_stage_marker(&text_relocation).is_err());

    let mut duplicate_dynamic_metadata = runtime();
    put64(&mut duplicate_dynamic_metadata, DYNAMIC_LOAD + 32, 8 * 16);
    put64(&mut duplicate_dynamic_metadata, DYNAMIC_LOAD + 40, 8 * 16);
    put64(&mut duplicate_dynamic_metadata, 0x2160, dynamic::DT_STRTAB);
    put64(&mut duplicate_dynamic_metadata, 0x2168, 0x500);
    put64(&mut duplicate_dynamic_metadata, 0x2170, dynamic::DT_NULL);
    put64(&mut duplicate_dynamic_metadata, 0x2178, 0);
    assert!(crate::target_loader::validate_host_runtime_elf(&duplicate_dynamic_metadata).is_err());
    assert!(LiteinstCallerImage::runtime_stage_marker(&duplicate_dynamic_metadata).is_err());
}

fn canonical_marker(runtime: &[u8]) -> Vec<u8> {
    canonical_stage_marker(runtime)
}

fn runtime_with_relocated_init_array(target: u64) -> Vec<u8> {
    let mut bytes = runtime();
    let dynamic_header = 64 + 3 * 56;
    put64(&mut bytes, dynamic_header + 32, 12 * 16);
    put64(&mut bytes, dynamic_header + 40, 12 * 16);
    for (index, (tag, value)) in [
        (dynamic::DT_INIT_ARRAY, 0x2200),
        (dynamic::DT_INIT_ARRAYSZ, 8),
        (dynamic::DT_RELA, 0x2300),
        (dynamic::DT_RELASZ, 24),
        (dynamic::DT_RELAENT, 24),
        (dynamic::DT_NULL, 0),
    ]
    .into_iter()
    .enumerate()
    {
        put64(&mut bytes, 0x2160 + index * 16, tag);
        put64(&mut bytes, 0x2168 + index * 16, value);
    }
    put64(&mut bytes, 0x2300, 0x2200);
    put64(&mut bytes, 0x2308, reloc::R_X86_64_RELATIVE as u64);
    put64(&mut bytes, 0x2310, target);
    bytes
}

static SEQUENCE: AtomicU64 = AtomicU64::new(0);
struct Input {
    directory: PathBuf,
    stage: PathBuf,
    marker: PathBuf,
}
impl Input {
    fn new() -> Self {
        let directory = std::env::temp_dir().join(format!(
            "liteinst-caller-input-{}-{}",
            std::process::id(),
            SEQUENCE.fetch_add(1, Ordering::SeqCst)
        ));
        std::fs::create_dir(&directory).unwrap();
        let stage = directory.join("runtime.so");
        let marker = directory.join("stage.marker");
        // This tests byte binding and ELF qualification. No constructor or
        // target code is executed.
        let bytes = runtime();
        std::fs::write(&stage, &bytes).unwrap();
        std::fs::write(&marker, canonical_marker(&bytes)).unwrap();
        Self {
            directory,
            stage,
            marker,
        }
    }
    fn image(&self) -> LiteinstCallerImage {
        LiteinstCallerImage::read_runtime(&self.stage, &self.marker).unwrap()
    }
}
impl Drop for Input {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.stage);
        let _ = std::fs::remove_file(&self.marker);
        let _ = std::fs::remove_dir(&self.directory);
    }
}

#[test]
fn sealed_runtime_retains_exact_stage_bytes_and_refuses_write_grow_shrink() {
    let input = Input::new();
    let stage = input.image();
    let sealed = SealedRuntime::prepare(&stage).unwrap();
    assert_eq!(sealed.image.bytes, stage.bytes);
    assert_eq!(
        sealed.image.file_identity.inode,
        sealed.file.metadata().unwrap().ino()
    );
    assert_eq!(
        unsafe { libc::fcntl(sealed.file.as_raw_fd(), libc::F_GET_SEALS) },
        RUNTIME_SEALS
    );
    assert_eq!(
        sealed.file.set_len(1).unwrap_err().raw_os_error(),
        Some(libc::EPERM)
    );
    assert_eq!(
        sealed.file.set_len(1000).unwrap_err().raw_os_error(),
        Some(libc::EPERM)
    );
    let mut writer = &sealed.file;
    assert_eq!(
        writer.write_all(b"X").unwrap_err().raw_os_error(),
        Some(libc::EPERM)
    );
    let mut actual = Vec::new();
    std::fs::File::open(&sealed.image.path)
        .unwrap()
        .read_to_end(&mut actual)
        .unwrap();
    assert_eq!(actual.as_slice(), stage.bytes.as_ref());
    assert_ne!(stage.file_identity, sealed.image.file_identity);
}

#[test]
fn changed_regular_stage_or_marker_is_refused_before_sealed_copy() {
    let input = Input::new();
    let stage = input.image();
    let mut replacement = runtime();
    replacement[0x1100] ^= 1;
    std::fs::write(&input.stage, replacement).unwrap();
    assert!(SealedRuntime::prepare(&stage).is_err());
    let bytes = runtime();
    std::fs::write(&input.stage, &bytes).unwrap();
    std::fs::write(&input.marker, canonical_marker(&bytes)).unwrap();
    let stage = input.image();
    std::fs::write(&input.marker, b"changed marker\n").unwrap();
    assert!(SealedRuntime::prepare(&stage).is_err());
}

#[test]
fn runtime_marker_requires_every_canonical_field_and_exact_dso_binding() {
    let input = Input::new();
    let bytes = std::fs::read(&input.stage).unwrap();
    assert!(LiteinstCallerImage::read_runtime(&input.stage, &input.marker).is_ok());
    let valid = String::from_utf8(canonical_marker(&bytes)).unwrap();
    assert_eq!(
        LiteinstCallerImage::runtime_stage_marker(&bytes).unwrap(),
        valid.as_bytes()
    );
    let digest_at = valid.find("dso_sha256=").unwrap() + "dso_sha256=".len();
    let digest_byte = valid[digest_at..digest_at + 64]
        .bytes()
        .position(|byte| (b'a'..=b'f').contains(&byte))
        .unwrap()
        + digest_at;
    let mut uppercase_digest = valid.clone().into_bytes();
    uppercase_digest[digest_byte] = uppercase_digest[digest_byte].to_ascii_uppercase();
    let uppercase_digest = String::from_utf8(uppercase_digest).unwrap();
    for invalid in [
        "nonempty legacy marker\n".to_owned(),
        valid.replace("schema=1", "schema=2"),
        valid.replacen("schema=1\n", "", 1),
        valid.replace("dso_bytes=12288", "dso_bytes=012288"),
        valid.replace("dso_bytes=12288", "dso_bytes=12287"),
        valid.replace("dso_sha256=", "dso_sha256=0"),
        uppercase_digest,
        valid.replace("default_features=false", "default_features=true"),
        valid.replace("features=[liteinst-after-loader-experiment]", "features=[]"),
        valid.replace(
            "features=[liteinst-after-loader-experiment]",
            "features=[liteinst-after-loader-experiment,preload-constructor]",
        ),
        valid.replace("preload_constructor=false", "preload_constructor=true"),
        format!("{valid}schema=1\n"),
        valid.replacen("schema=1\n", "schema=1\nschema=1\n", 1),
        valid.trim_end().to_owned(),
        valid.replace('\n', "\r\n"),
    ] {
        std::fs::write(&input.marker, invalid).unwrap();
        assert!(
            LiteinstCallerImage::read_runtime(&input.stage, &input.marker).is_err(),
            "accepted malformed marker"
        );
    }
    std::fs::write(&input.marker, [0xff, b'\n']).unwrap();
    assert!(LiteinstCallerImage::read_runtime(&input.stage, &input.marker).is_err());
    let mut other = bytes.clone();
    other[0x1100] ^= 1;
    std::fs::write(&input.stage, &other).unwrap();
    std::fs::write(&input.marker, canonical_marker(&bytes)).unwrap();
    assert!(LiteinstCallerImage::read_runtime(&input.stage, &input.marker).is_err());
}

#[test]
fn runtime_requires_host_export_and_refuses_legacy_init_array_entry() {
    let input = Input::new();
    let mut missing_host = runtime();
    missing_host[0x501] = b'x';
    std::fs::write(&input.stage, &missing_host).unwrap();
    std::fs::write(&input.marker, canonical_marker(&missing_host)).unwrap();
    assert!(LiteinstCallerImage::read_runtime(&input.stage, &input.marker).is_err());

    let mut legacy_constructor = runtime();
    let dynamic_header = 64 + 3 * 56;
    put64(&mut legacy_constructor, dynamic_header + 32, 9 * 16);
    put64(&mut legacy_constructor, dynamic_header + 40, 9 * 16);
    put64(&mut legacy_constructor, 0x2160, dynamic::DT_INIT_ARRAY);
    put64(&mut legacy_constructor, 0x2168, 0x2200);
    put64(&mut legacy_constructor, 0x2170, dynamic::DT_INIT_ARRAYSZ);
    put64(&mut legacy_constructor, 0x2178, 8);
    put64(&mut legacy_constructor, 0x2180, dynamic::DT_NULL);
    put64(&mut legacy_constructor, 0x2200, 0x1120);
    std::fs::write(&input.stage, &legacy_constructor).unwrap();
    std::fs::write(&input.marker, canonical_marker(&legacy_constructor)).unwrap();
    assert!(LiteinstCallerImage::read_runtime(&input.stage, &input.marker).is_err());

    let relocated_legacy_constructor = runtime_with_relocated_init_array(0x1120);
    std::fs::write(&input.stage, &relocated_legacy_constructor).unwrap();
    std::fs::write(
        &input.marker,
        canonical_marker(&relocated_legacy_constructor),
    )
    .unwrap();
    assert!(LiteinstCallerImage::read_runtime(&input.stage, &input.marker).is_err());

    let mut externally_resolved_legacy_constructor = runtime_with_relocated_init_array(0);
    put16(&mut externally_resolved_legacy_constructor, 0x636, 0);
    put64(&mut externally_resolved_legacy_constructor, 0x638, 0);
    put64(
        &mut externally_resolved_legacy_constructor,
        0x2308,
        (2_u64 << 32) | u64::from(reloc::R_X86_64_64),
    );
    std::fs::write(&input.stage, &externally_resolved_legacy_constructor).unwrap();
    std::fs::write(
        &input.marker,
        canonical_marker(&externally_resolved_legacy_constructor),
    )
    .unwrap();
    assert!(LiteinstCallerImage::read_runtime(&input.stage, &input.marker).is_err());

    let unrelated_initializer = runtime_with_relocated_init_array(0x1140);
    std::fs::write(&input.stage, &unrelated_initializer).unwrap();
    std::fs::write(&input.marker, canonical_marker(&unrelated_initializer)).unwrap();
    assert!(LiteinstCallerImage::read_runtime(&input.stage, &input.marker).is_ok());
}

#[test]
fn ordinary_input_cannot_be_used_as_an_unqualified_runtime_stage() {
    let input = Input::new();
    let stage = LiteinstCallerImage::read(&input.stage).unwrap();
    assert!(stage.runtime_marker().is_none());
    assert!(SealedRuntime::prepare(&stage).is_err());
    assert_eq!(MAX_CALLER_FILE, 32 * 1024 * 1024);
    assert_eq!(MAX_RUNTIME_FILE, 64 * 1024 * 1024);
}

#[test]
fn missing_marker_and_diagnostic_overflow_remain_errors() {
    let input = Input::new();
    assert!(
        LiteinstCallerImage::read_runtime(&input.stage, input.directory.join("missing")).is_err()
    );
    let diagnostics = LiteinstCallerDiagnostics::default();
    diagnostics.record("before", Some(7), "kept").unwrap();
    assert!(
        diagnostics
            .record("backwards", Some(6), "not accepted")
            .is_err()
    );
    assert!(
        diagnostics
            .record("oversized", None, "x".repeat(8 * 1024 * 1024 + 1))
            .is_err()
    );
    let retained = diagnostics.observations();
    assert_eq!(retained.len(), 1);
    assert_eq!(retained[0].detail, "kept");
    assert_eq!(retained[0].raw_clock, Some(7));
}

#[test]
fn loader_union_graph_has_exact_initial_and_deferred_closures() {
    let graph = BTreeMap::from([
        (
            "ld-linux-x86-64.so.2".to_owned(),
            BTreeSet::from(["libc.so.6".to_owned()]),
        ),
        ("libc.so.6".to_owned(), BTreeSet::new()),
        (
            "libgcc_s.so.1".to_owned(),
            BTreeSet::from(["libc.so.6".to_owned()]),
        ),
    ]);
    let initial_roots = BTreeSet::from(["ld-linux-x86-64.so.2".to_owned(), "libc.so.6".to_owned()]);
    let runtime_roots = BTreeSet::from([
        "ld-linux-x86-64.so.2".to_owned(),
        "libc.so.6".to_owned(),
        "libgcc_s.so.1".to_owned(),
    ]);
    let (initial, deferred) =
        partition_loader_dependency_names(&initial_roots, &runtime_roots, &graph).unwrap();
    assert_eq!(
        initial,
        BTreeSet::from(["ld-linux-x86-64.so.2".to_owned(), "libc.so.6".to_owned(),])
    );
    assert_eq!(deferred, BTreeSet::from(["libgcc_s.so.1".to_owned()]));
}

#[test]
fn loader_phase_partition_refuses_missing_and_unreachable_images() {
    let complete = BTreeMap::from([
        ("loader".to_owned(), BTreeSet::from(["libc".to_owned()])),
        ("libc".to_owned(), BTreeSet::new()),
        ("runtime-only".to_owned(), BTreeSet::new()),
    ]);
    let initial = BTreeSet::from(["loader".to_owned()]);
    let runtime = BTreeSet::from(["runtime-only".to_owned()]);
    assert!(partition_loader_dependency_names(&initial, &runtime, &complete).is_ok());

    let missing = BTreeMap::from([("loader".to_owned(), BTreeSet::from(["absent".to_owned()]))]);
    assert!(partition_loader_dependency_names(&initial, &runtime, &missing).is_err());

    let mut unreachable = complete.clone();
    unreachable.insert("unreachable".to_owned(), BTreeSet::new());
    assert!(partition_loader_dependency_names(&initial, &runtime, &unreachable).is_err());

    let cyclic = BTreeMap::from([
        ("loader".to_owned(), BTreeSet::from(["libc".to_owned()])),
        ("libc".to_owned(), BTreeSet::from(["loader".to_owned()])),
        ("runtime-only".to_owned(), BTreeSet::new()),
    ]);
    let (cyclic_initial, cyclic_deferred) =
        partition_loader_dependency_names(&initial, &runtime, &cyclic).unwrap();
    assert_eq!(
        cyclic_initial,
        BTreeSet::from(["loader".to_owned(), "libc".to_owned()])
    );
    assert_eq!(cyclic_deferred, BTreeSet::from(["runtime-only".to_owned()]));
}

fn assert_cache_parse_rejected(label: &str, bytes: &[u8]) {
    assert!(
        parse_glibc_loader_cache(bytes).is_err(),
        "accepted malformed glibc cache: {label}"
    );
}

#[test]
fn glibc_cache_binds_one_exact_deferred_alias() {
    let input = CacheAliasInput::new();
    let dependency = input.deferred_image();
    let cache_bytes = synthetic_glibc_cache(&[input.entry()]);
    std::fs::write(&input.cache, &cache_bytes).unwrap();
    let cache = LiteinstLoaderCache::read(&input.cache).unwrap();
    let aliases = bind_glibc_loader_cache_alias_targets(
        &cache,
        &[("libdeferred.so.1".to_owned(), dependency.clone())],
    )
    .unwrap();
    let alias = aliases.get(&input.alias).unwrap();
    assert_eq!(alias.soname(), "libdeferred.so.1");
    assert_eq!(alias.raw_path(), input.alias);
    assert_eq!(alias.raw_link(), input.deferred);
    assert_eq!(
        alias.raw_link_stamp(),
        file_stamp(&std::fs::symlink_metadata(&input.alias).unwrap())
    );
    assert_eq!(
        alias.dependency_stamp(),
        file_stamp(&std::fs::metadata(&input.deferred).unwrap())
    );
    assert_eq!(alias.dependency().path, dependency.path);
    assert_eq!(alias.dependency().file_identity, dependency.file_identity);
    assert_eq!(alias.dependency().bytes, dependency.bytes);
    assert_eq!(cache.path(), input.cache);
    assert_eq!(cache.bytes(), cache_bytes);
    assert_eq!(
        cache.file_identity(),
        FileIdentity::from_metadata(&std::fs::metadata(&input.cache).unwrap())
    );
    alias.revalidate().unwrap();
}

#[test]
fn glibc_cache_alias_revalidation_binds_link_spelling_and_stable_dependency_stamp() {
    let input = CacheAliasInput::new();
    let dependency = input.deferred_image();
    let aliases = input
        .bind(&synthetic_glibc_cache(&[input.entry()]), &dependency)
        .unwrap();
    let alias = aliases.get(&input.alias).unwrap().clone();

    std::fs::remove_file(&input.alias).unwrap();
    std::os::unix::fs::symlink(input.deferred.file_name().unwrap(), &input.alias).unwrap();
    assert_eq!(input.alias.canonicalize().unwrap(), input.deferred);
    assert!(
        alias.revalidate().is_err(),
        "accepted a different raw symlink payload reaching the same dependency"
    );

    let input = CacheAliasInput::new();
    let dependency = input.deferred_image();
    let aliases = input
        .bind(&synthetic_glibc_cache(&[input.entry()]), &dependency)
        .unwrap();
    let alias = aliases.get(&input.alias).unwrap().clone();
    let metadata = std::fs::metadata(&input.deferred).unwrap();
    let changed_mode = metadata.permissions().mode() ^ 0o100;
    std::fs::set_permissions(
        &input.deferred,
        std::fs::Permissions::from_mode(changed_mode),
    )
    .unwrap();
    assert_eq!(
        FileIdentity::from_metadata(&std::fs::metadata(&input.deferred).unwrap()),
        dependency.file_identity
    );
    assert_eq!(
        std::fs::read(&input.deferred).unwrap(),
        dependency.bytes.as_ref()
    );
    assert!(
        alias.revalidate().is_err(),
        "accepted a same-inode, same-byte dependency with a changed stable stamp"
    );
}

#[test]
fn loader_cache_alias_profile_is_zero_or_one_exact_libgcc_alias() {
    assert!(loader_cache_alias_profile_is_exact(&BTreeMap::new()));
    assert!(loader_cache_alias_target_profile_is_exact(&[]));

    let input = CacheAliasInput::new();
    let dependency = input.deferred_image();
    let aliases = input
        .bind(&synthetic_glibc_cache(&[input.entry()]), &dependency)
        .unwrap();
    let mut exact = aliases.values().next().unwrap().clone();
    exact.soname = PROFILED_LOADER_CACHE_ALIAS_SONAME.to_owned();
    exact.raw_path = PathBuf::from(PROFILED_LOADER_CACHE_ALIAS_RAW_PATH);
    exact.dependency.path = PathBuf::from(PROFILED_LOADER_CACHE_ALIAS_DEPENDENCY_PATH);
    assert!(loader_cache_alias_target_profile_is_exact(&[(
        exact.soname.clone(),
        exact.dependency.clone(),
    )]));
    let exact = BTreeMap::from([(exact.raw_path.clone(), exact)]);
    assert!(loader_cache_alias_profile_is_exact(&exact));

    for mutation in ["soname", "raw path", "dependency path"] {
        let mut changed = exact.clone();
        let alias = changed.values_mut().next().unwrap();
        match mutation {
            "soname" => alias.soname.push_str(".changed"),
            "raw path" => alias.raw_path.push("changed"),
            "dependency path" => alias.dependency.path.push("changed"),
            _ => unreachable!(),
        }
        assert!(
            !loader_cache_alias_profile_is_exact(&changed),
            "accepted changed profile field: {mutation}"
        );
        let alias = changed.values().next().unwrap();
        if mutation != "raw path" {
            assert!(
                !loader_cache_alias_target_profile_is_exact(&[(
                    alias.soname.clone(),
                    alias.dependency.clone(),
                )]),
                "accepted changed target profile field: {mutation}"
            );
        }
    }

    let alias = exact.values().next().unwrap().clone();
    let changed_key = BTreeMap::from([(PathBuf::from("/lib64/changed.so.1"), alias)]);
    assert!(
        !loader_cache_alias_profile_is_exact(&changed_key),
        "accepted a changed raw-path map key"
    );

    let mut duplicated = exact;
    let mut second = duplicated.values().next().unwrap().clone();
    second.raw_path = PathBuf::from("/lib64/libgcc_s-second.so.1");
    duplicated.insert(second.raw_path.clone(), second);
    assert!(!loader_cache_alias_profile_is_exact(&duplicated));
}

#[test]
fn glibc_cache_header_count_and_extension_boundaries_are_exact() {
    let input = CacheAliasInput::new();
    let valid = synthetic_glibc_cache(&[input.entry()]);
    assert_eq!(parse_glibc_loader_cache(&valid).unwrap().len(), 1);

    assert_cache_parse_rejected("truncated header", &valid[..GLIBC_CACHE_HEADER_SIZE - 1]);

    let mut changed = valid.clone();
    changed[0] ^= 1;
    assert_cache_parse_rejected("magic", &changed);

    let mut changed = valid.clone();
    changed[28] = 0;
    assert_cache_parse_rejected("endianness flag", &changed);

    let mut changed = valid.clone();
    changed[29] = 1;
    assert_cache_parse_rejected("endianness padding", &changed);

    let mut changed = valid.clone();
    changed[36] = 1;
    assert_cache_parse_rejected("reserved header", &changed);

    let mut changed = valid.clone();
    put32(
        &mut changed,
        20,
        u32::try_from(MAX_GLIBC_CACHE_ENTRIES + 1).unwrap(),
    );
    assert_cache_parse_rejected("entry count bound", &changed);

    let mut changed = valid.clone();
    put32(&mut changed, 20, 2);
    assert_cache_parse_rejected("entry count/table disagreement", &changed);

    let mut changed = valid.clone();
    put32(&mut changed, 20, 0);
    assert_cache_parse_rejected("zero count with retained entry", &changed);

    let mut changed = valid.clone();
    put32(&mut changed, 24, u32::MAX);
    assert_cache_parse_rejected("string table length", &changed);

    let declared_strings = u32::from_le_bytes(valid[24..28].try_into().unwrap());
    for (label, length) in [
        ("short string table", declared_strings - 1),
        ("long string table", declared_strings + 1),
    ] {
        let mut changed = valid.clone();
        put32(&mut changed, 24, length);
        assert_cache_parse_rejected(label, &changed);
    }

    let mut changed = valid.clone();
    changed.push(0);
    assert_cache_parse_rejected("undeclared trailing byte", &changed);

    let mut changed = valid.clone();
    let string_start = GLIBC_CACHE_HEADER_SIZE + GLIBC_CACHE_ENTRY_SIZE;
    put32(&mut changed, 32, u32::try_from(string_start).unwrap());
    assert_cache_parse_rejected("extension overlaps string table", &changed);

    let mut changed = valid.clone();
    let outside = changed.len() + 16;
    put32(&mut changed, 32, u32::try_from(outside).unwrap());
    assert_cache_parse_rejected("extension outside bytes", &changed);

    let changed = synthetic_glibc_cache_with_extension(valid.clone(), 0);
    assert_eq!(parse_glibc_loader_cache(&changed).unwrap().len(), 1);

    let mut unaligned = changed.clone();
    unaligned.push(0);
    let extension =
        usize::try_from(u32::from_le_bytes(changed[32..36].try_into().unwrap())).unwrap();
    let unaligned_offset = extension + 1;
    put32(&mut unaligned, 32, u32::try_from(unaligned_offset).unwrap());
    assert_cache_parse_rejected("unaligned extension", &unaligned);

    let mut nonzero_gap = synthetic_glibc_cache_with_extension(valid.clone(), 4);
    nonzero_gap[valid.len()] = 1;
    assert_cache_parse_rejected("nonzero extension padding", &nonzero_gap);

    let mut changed = synthetic_glibc_cache_with_extension(valid.clone(), 0);
    let extension =
        usize::try_from(u32::from_le_bytes(changed[32..36].try_into().unwrap())).unwrap();
    put32(&mut changed, extension, 0);
    assert_cache_parse_rejected("extension magic", &changed);

    let mut changed = synthetic_glibc_cache_with_extension(valid.clone(), 0);
    let extension =
        usize::try_from(u32::from_le_bytes(changed[32..36].try_into().unwrap())).unwrap();
    put32(&mut changed, extension + 4, 0);
    assert_cache_parse_rejected("zero extension section count", &changed);

    let mut changed = synthetic_glibc_cache_with_extension(valid, 0);
    let extension =
        usize::try_from(u32::from_le_bytes(changed[32..36].try_into().unwrap())).unwrap();
    put32(
        &mut changed,
        extension + 4,
        u32::try_from(MAX_GLIBC_CACHE_EXTENSION_SECTIONS + 1).unwrap(),
    );
    assert_cache_parse_rejected("extension section count bound", &changed);
}

#[test]
fn glibc_cache_offsets_terminators_and_strings_stay_inside_exact_bounds() {
    let input = CacheAliasInput::new();
    let valid = synthetic_glibc_cache(&[input.entry()]);
    let string_start = GLIBC_CACHE_HEADER_SIZE + GLIBC_CACHE_ENTRY_SIZE;
    let string_end = valid.len();
    let key_length = b"libdeferred.so.1".len();
    let value_length = input.alias.as_os_str().as_encoded_bytes().len();

    for (label, field, offset) in [
        ("key before string table", 4, string_start - 1),
        ("key at string-table end", 4, string_end),
        ("value before string table", 8, string_start - 1),
        ("value at string-table end", 8, string_end),
    ] {
        let mut changed = valid.clone();
        put32(
            &mut changed,
            GLIBC_CACHE_HEADER_SIZE + field,
            u32::try_from(offset).unwrap(),
        );
        assert_cache_parse_rejected(label, &changed);
    }
    for (label, field) in [("key offset overflow", 4), ("value offset overflow", 8)] {
        let mut changed = valid.clone();
        put32(&mut changed, GLIBC_CACHE_HEADER_SIZE + field, u32::MAX);
        assert_cache_parse_rejected(label, &changed);
    }

    let mut changed = valid.clone();
    put32(
        &mut changed,
        GLIBC_CACHE_HEADER_SIZE + 4,
        u32::try_from(string_start + key_length).unwrap(),
    );
    assert_cache_parse_rejected("empty key", &changed);

    let value_start = string_start + key_length + 1;
    let mut changed = valid.clone();
    put32(
        &mut changed,
        GLIBC_CACHE_HEADER_SIZE + 8,
        u32::try_from(value_start + value_length).unwrap(),
    );
    assert_cache_parse_rejected("empty value", &changed);

    let mut changed = valid.clone();
    changed[string_start + key_length] = b'x';
    assert_cache_parse_rejected("missing key terminator", &changed);

    let mut changed = valid.clone();
    changed[value_start + value_length] = b'x';
    assert_cache_parse_rejected("missing value terminator", &changed);

    let mut changed = valid.clone();
    changed[string_start..].iter_mut().for_each(|byte| {
        if *byte == 0 {
            *byte = b'x';
        }
    });
    assert_cache_parse_rejected("unterminated strings", &changed);

    for (label, entry) in [
        (
            "control byte in key",
            SyntheticCacheEntry::x86_64(
                b"libdeferred\n.so.1",
                input.alias.as_os_str().as_encoded_bytes(),
            ),
        ),
        (
            "control byte in path",
            SyntheticCacheEntry::x86_64(b"libdeferred.so.1", b"/tmp/lib\n.so"),
        ),
        (
            "relative path",
            SyntheticCacheEntry::x86_64(b"libdeferred.so.1", b"libdeferred.so.1"),
        ),
        (
            "double-slash path",
            SyntheticCacheEntry::x86_64(b"libdeferred.so.1", b"/tmp//libdeferred.so.1"),
        ),
        (
            "parent-component path",
            SyntheticCacheEntry::x86_64(b"libdeferred.so.1", b"/tmp/../libdeferred.so.1"),
        ),
    ] {
        assert_cache_parse_rejected(label, &synthetic_glibc_cache(&[entry]));
    }

    let oversized = [b"/".as_slice(), &vec![b'a'; MAX_GLIBC_CACHE_STRING]].concat();
    assert_cache_parse_rejected(
        "oversized absolute path",
        &synthetic_glibc_cache(&[SyntheticCacheEntry::x86_64(b"libdeferred.so.1", oversized)]),
    );
}

#[test]
fn glibc_cache_deferred_entry_metadata_and_uniqueness_are_exact() {
    let input = CacheAliasInput::new();
    let dependency = input.deferred_image();

    for (label, entry) in [
        ("flags", {
            let mut entry = input.entry();
            entry.flags ^= 1;
            entry
        }),
        ("OS version", {
            let mut entry = input.entry();
            entry.osversion = 1;
            entry
        }),
        ("hwcap", {
            let mut entry = input.entry();
            entry.hwcap = 1;
            entry
        }),
    ] {
        assert!(
            input
                .bind(&synthetic_glibc_cache(&[entry]), &dependency)
                .is_err(),
            "accepted changed deferred cache {label}"
        );
    }

    let entry = input.entry();
    assert!(
        input
            .bind(&synthetic_glibc_cache(&[entry.clone(), entry]), &dependency,)
            .is_err(),
        "accepted duplicate deferred SONAME"
    );

    let entry = input.entry();
    let mut alternative = entry.clone();
    alternative.hwcap = 1;
    assert!(
        input
            .bind(&synthetic_glibc_cache(&[entry, alternative]), &dependency,)
            .is_err(),
        "accepted alternate hwcap record"
    );

    let missing =
        SyntheticCacheEntry::x86_64(b"libother.so.1", input.alias.as_os_str().as_encoded_bytes());
    assert!(
        input
            .bind(&synthetic_glibc_cache(&[missing]), &dependency)
            .is_err(),
        "accepted cache without the deferred SONAME"
    );

    std::fs::write(&input.cache, synthetic_glibc_cache(&[input.entry()])).unwrap();
    let cache = LiteinstLoaderCache::read(&input.cache).unwrap();
    assert!(
        bind_glibc_loader_cache_alias_targets(
            &cache,
            &[
                ("libdeferred.so.1".to_owned(), dependency.clone()),
                ("libdeferred.so.1".to_owned(), dependency),
            ],
        )
        .is_err(),
        "accepted duplicate target SONAME"
    );
}

#[test]
fn glibc_cache_model_excludes_initial_and_unbound_sonames() {
    let input = CacheAliasInput::new();
    let dependency = input.deferred_image();
    let entries = [
        input.entry(),
        SyntheticCacheEntry::x86_64(
            b"libinitial.so.1",
            input.initial.as_os_str().as_encoded_bytes(),
        ),
        SyntheticCacheEntry::x86_64(
            b"libunbound.so.1",
            input.unbound.as_os_str().as_encoded_bytes(),
        ),
    ];
    let aliases = input
        .bind(&synthetic_glibc_cache(&entries), &dependency)
        .unwrap();
    assert_eq!(aliases.len(), 1);
    assert!(aliases.contains_key(&input.alias));
    assert!(!aliases.contains_key(&input.initial));
    assert!(!aliases.contains_key(&input.unbound));

    for soname in [b"libinitial.so.1".as_slice(), b"libunbound.so.1".as_slice()] {
        let collision =
            SyntheticCacheEntry::x86_64(soname, input.alias.as_os_str().as_encoded_bytes());
        assert!(
            input
                .bind(
                    &synthetic_glibc_cache(&[input.entry(), collision]),
                    &dependency,
                )
                .is_err(),
            "accepted a deferred raw alias shared by another SONAME"
        );
    }
}

#[test]
fn glibc_cache_alias_must_rebind_exact_path_bytes_and_identity() {
    for target in ["initial", "unbound"] {
        let input = CacheAliasInput::new();
        let dependency = input.deferred_image();
        let target_path = match target {
            "initial" => input.initial.clone(),
            "unbound" => input.unbound.clone(),
            _ => unreachable!(),
        };
        input.retarget_alias(&target_path);
        assert!(
            input
                .bind(&synthetic_glibc_cache(&[input.entry()]), &dependency)
                .is_err(),
            "accepted alias retargeted to {target} image"
        );
    }

    let input = CacheAliasInput::new();
    let dependency = input.deferred_image();
    std::fs::write(&input.deferred, b"\x7fELFdeferred-v2").unwrap();
    assert!(
        input
            .bind(&synthetic_glibc_cache(&[input.entry()]), &dependency)
            .is_err(),
        "accepted changed dependency bytes"
    );

    let input = CacheAliasInput::new();
    let dependency = input.deferred_image();
    let old = input.directory.join("deferred-old");
    std::fs::rename(&input.deferred, &old).unwrap();
    std::fs::write(&input.deferred, dependency.bytes.as_ref()).unwrap();
    assert!(
        input
            .bind(&synthetic_glibc_cache(&[input.entry()]), &dependency)
            .is_err(),
        "accepted replacement inode at the same path with the same bytes"
    );

    let input = CacheAliasInput::new();
    let dependency = input.deferred_image();
    let copy = input.directory.join("same-bytes-copy");
    std::fs::write(&copy, dependency.bytes.as_ref()).unwrap();
    input.retarget_alias(&copy);
    assert!(
        input
            .bind(&synthetic_glibc_cache(&[input.entry()]), &dependency)
            .is_err(),
        "accepted same bytes at a different canonical path and identity"
    );
}

#[test]
fn glibc_cache_alias_fifo_is_refused_without_blocking() {
    if std::env::var_os(FIFO_CHILD).is_some() {
        let cache_path = PathBuf::from(std::env::var_os(FIFO_CHILD_CACHE).unwrap());
        let deferred_path = PathBuf::from(std::env::var_os(FIFO_CHILD_DEFERRED).unwrap());
        let dependency = LiteinstCallerImage::read(&deferred_path).unwrap();
        std::fs::remove_file(&deferred_path).unwrap();
        make_fifo(&deferred_path);
        let cache = LiteinstLoaderCache::read(&cache_path).unwrap();
        assert!(
            bind_glibc_loader_cache_alias_targets(
                &cache,
                &[("libdeferred.so.1".to_owned(), dependency)],
            )
            .is_err(),
            "accepted a cache alias retargeted to a FIFO"
        );
        return;
    }

    let input = CacheAliasInput::new();
    std::fs::write(&input.cache, synthetic_glibc_cache(&[input.entry()])).unwrap();
    let mut child = std::process::Command::new(std::env::current_exe().unwrap())
        .arg("glibc_cache_alias_fifo_is_refused_without_blocking")
        .arg("--nocapture")
        .env(FIFO_CHILD, "1")
        .env(FIFO_CHILD_CACHE, &input.cache)
        .env(FIFO_CHILD_DEFERRED, &input.deferred)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if std::time::Instant::now() >= deadline {
            child.kill().unwrap();
            let _ = child.wait();
            panic!("FIFO cache-alias refusal child exceeded five-second bound");
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    };
    assert!(status.success(), "FIFO cache-alias refusal child failed");
}
