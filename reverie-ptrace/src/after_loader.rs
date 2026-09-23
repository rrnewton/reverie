//! Inputs and retained diagnostics for the experimental one-task host caller.
use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::ffi::OsStr;
use std::ffi::OsString;
use std::io::Read;
use std::io::Seek;
use std::io::Write;
use std::io::{self};
use std::os::fd::AsRawFd;
use std::os::fd::FromRawFd;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::ffi::OsStringExt;
use std::os::unix::fs::MetadataExt;
use std::os::unix::fs::OpenOptionsExt;
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

mod manifest;

pub use manifest::LiteinstAfterLoaderProfile;

pub(crate) const MAX_CALLER_FILE: usize = 32 * 1024 * 1024;
pub(crate) const MAX_LOADER_CACHE_FILE: usize = 64 * 1024 * 1024;
pub(crate) const MAX_RUNTIME_FILE: usize = 64 * 1024 * 1024;
pub(crate) const MAX_RUNTIME_LOAD_SPAN: u64 = 128 * 1024 * 1024;
const RUNTIME_LOAD_PAGE: u64 = 4096;
const MAX_STAGE_MARKER: usize = 1024;
const HOST_INITIALIZER: &str = "reverie_liteinst_initialize_host";
const LEGACY_INITIALIZER: &str = "reverie_liteinst_initialize";
pub(crate) const IMMUTABLE_FILE_SEALS: i32 =
    libc::F_SEAL_WRITE | libc::F_SEAL_GROW | libc::F_SEAL_SHRINK | libc::F_SEAL_SEAL;
pub(crate) const RUNTIME_SEALS: i32 = IMMUTABLE_FILE_SEALS;

const GLIBC_CACHE_MAGIC: &[u8; 20] = b"glibc-ld.so.cache1.1";
const GLIBC_CACHE_HEADER_SIZE: usize = 48;
const GLIBC_CACHE_ENTRY_SIZE: usize = 24;
const GLIBC_CACHE_LITTLE_ENDIAN: u8 = 2;
const GLIBC_CACHE_ENTRY_FLAGS_X86_64: u32 = 0x303;
const MAX_GLIBC_CACHE_ENTRIES: usize = 16_384;
const MAX_GLIBC_CACHE_STRING: usize = 4095;
const GLIBC_CACHE_EXTENSION_HEADER_SIZE: usize = 8;
const GLIBC_CACHE_EXTENSION_MAGIC: u32 = 0xeaa4_2174;
const GLIBC_CACHE_EXTENSION_SECTION_SIZE: usize = 16;
const MAX_GLIBC_CACHE_EXTENSION_SECTIONS: usize = 2;
const PROFILED_LOADER_CACHE_ALIAS_SONAME: &str = "libgcc_s.so.1";
const PROFILED_LOADER_CACHE_ALIAS_RAW_PATH: &str = "/lib64/libgcc_s.so.1";
const PROFILED_LOADER_CACHE_ALIAS_DEPENDENCY_PATH: &str = "/usr/lib64/libgcc_s-11-20240719.so.1";

/// Exact loader-input policy declared by a schema-3 manifest.
///
/// The paths are profile data, not conventional glibc names: patched loaders
/// may probe different cache and preload paths. The eventual launch path must
/// expose the bundle's exact sealed cache at `loader_cache_path`, make
/// `system_preload_path` absent (not merely empty or unreadable), and retain
/// immutable evidence for every requested image. Only the explicit
/// `ProfiledStableRealFileAlias` variant retains the reviewed deferred libgcc
/// cache alias as an authenticated real file so its kernel mapping pathname
/// retains native glibc semantics; `SealedBundleOnly` admits no such alias.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct LiteinstLoaderPolicy {
    loader_cache_path: PathBuf,
    system_preload_path: PathBuf,
    loader_search: LiteinstLoaderSearchPolicy,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum LiteinstLoaderSearchPolicy {
    SealedBundleOnly,
    ProfiledStableRealFileAlias,
}

impl LiteinstLoaderPolicy {
    pub(crate) fn exact_cache_and_absent_preload(
        loader_cache_path: PathBuf,
        system_preload_path: PathBuf,
    ) -> Self {
        Self {
            loader_cache_path,
            system_preload_path,
            loader_search: LiteinstLoaderSearchPolicy::SealedBundleOnly,
        }
    }

    pub(crate) fn exact_cache_absent_preload_and_profiled_real_alias(
        loader_cache_path: PathBuf,
        system_preload_path: PathBuf,
    ) -> Self {
        Self {
            loader_cache_path,
            system_preload_path,
            loader_search: LiteinstLoaderSearchPolicy::ProfiledStableRealFileAlias,
        }
    }

    pub(crate) fn loader_cache_path(&self) -> &Path {
        &self.loader_cache_path
    }

    pub(crate) fn system_preload_path(&self) -> &Path {
        &self.system_preload_path
    }

    fn diagnostic(&self) -> &'static str {
        match self.loader_search {
            LiteinstLoaderSearchPolicy::SealedBundleOnly => {
                "loader_cache=sealed-exact system_preload=absent loader_search=sealed-bundle-only"
            }
            LiteinstLoaderSearchPolicy::ProfiledStableRealFileAlias => {
                "loader_cache=sealed-exact system_preload=absent loader_search=profiled-stable-real-file-alias"
            }
        }
    }

    fn admits_profiled_real_file_alias(&self) -> bool {
        self.loader_search == LiteinstLoaderSearchPolicy::ProfiledStableRealFileAlias
    }
}

pub(crate) type StableFileStamp = (u64, u64, u64, i64, i64, i64, i64);

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
pub(crate) struct LiteinstLoaderCache {
    path: PathBuf,
    bytes: Arc<[u8]>,
    file_identity: FileIdentity,
}

impl LiteinstLoaderCache {
    pub(crate) fn read(path: &Path) -> io::Result<Self> {
        let path = path.canonicalize()?;
        let mut file = std::fs::File::open(&path)?;
        let before = file.metadata()?;
        let mut bytes = Vec::new();
        (&mut file)
            .take(MAX_LOADER_CACHE_FILE as u64 + 1)
            .read_to_end(&mut bytes)?;
        if !before.is_file()
            || bytes.len() > MAX_LOADER_CACHE_FILE
            || file_stamp(&before) != file_stamp(&file.metadata()?)
            || file_stamp(&before) != file_stamp(&std::fs::metadata(&path)?)
        {
            return Err(io::Error::other(
                "loader cache is not a bounded unchanged regular file",
            ));
        }
        Ok(Self {
            path,
            bytes: bytes.into(),
            file_identity: FileIdentity::from_metadata(&before),
        })
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    pub(crate) fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    pub(crate) fn file_identity(&self) -> FileIdentity {
        self.file_identity
    }
}

#[derive(Clone, Copy, Debug)]
struct GlibcLoaderCacheEntry<'a> {
    flags: u32,
    key: &'a [u8],
    value: &'a [u8],
    osversion: u32,
    hwcap: u64,
}

fn glibc_cache_u32(bytes: &[u8], offset: usize, label: &str) -> io::Result<u32> {
    let end = offset
        .checked_add(4)
        .ok_or_else(|| io::Error::other(format!("glibc loader cache {label} offset overflow")))?;
    let value = bytes
        .get(offset..end)
        .ok_or_else(|| io::Error::other(format!("glibc loader cache {label} is truncated")))?;
    Ok(u32::from_le_bytes(value.try_into().unwrap()))
}

fn glibc_cache_u64(bytes: &[u8], offset: usize, label: &str) -> io::Result<u64> {
    let end = offset
        .checked_add(8)
        .ok_or_else(|| io::Error::other(format!("glibc loader cache {label} offset overflow")))?;
    let value = bytes
        .get(offset..end)
        .ok_or_else(|| io::Error::other(format!("glibc loader cache {label} is truncated")))?;
    Ok(u64::from_le_bytes(value.try_into().unwrap()))
}

fn glibc_cache_string<'a>(
    bytes: &'a [u8],
    offset: u32,
    string_table: (usize, usize),
    label: &str,
) -> io::Result<&'a [u8]> {
    let offset = usize::try_from(offset).map_err(|_| {
        io::Error::other(format!("glibc loader cache {label} is not representable"))
    })?;
    if offset < string_table.0 || offset >= string_table.1 {
        return Err(io::Error::other(format!(
            "glibc loader cache {label} is outside the declared string table"
        )));
    }
    let tail = bytes
        .get(offset..string_table.1)
        .ok_or_else(|| io::Error::other(format!("glibc loader cache {label} range differs")))?;
    let search_length = tail.len().min(MAX_GLIBC_CACHE_STRING + 1);
    let length = tail[..search_length]
        .iter()
        .position(|byte| *byte == 0)
        .ok_or_else(|| {
            io::Error::other(format!(
                "glibc loader cache {label} lacks a bounded NUL inside the declared string table"
            ))
        })?;
    let value = &tail[..length];
    if value.is_empty() || value.len() > MAX_GLIBC_CACHE_STRING {
        return Err(io::Error::other(format!(
            "glibc loader cache {label} is empty or exceeds its bound"
        )));
    }
    Ok(value)
}

fn glibc_cache_alias_path(bytes: &[u8]) -> io::Result<PathBuf> {
    if bytes.len() > MAX_GLIBC_CACHE_STRING
        || bytes.first() != Some(&b'/')
        || bytes.last() == Some(&b'/')
        || bytes.iter().any(|byte| byte.is_ascii_control())
        || bytes.windows(2).any(|pair| pair == b"//")
        || bytes
            .split(|byte| *byte == b'/')
            .any(|component| component == b"." || component == b"..")
    {
        return Err(io::Error::other(
            "glibc loader cache value is not one bounded absolute Unix path",
        ));
    }
    Ok(PathBuf::from(OsString::from_vec(bytes.to_vec())))
}

fn validate_glibc_cache_extension(bytes: &[u8], extension_offset: usize) -> io::Result<()> {
    if glibc_cache_u32(bytes, extension_offset, "extension magic")? != GLIBC_CACHE_EXTENSION_MAGIC {
        return Err(io::Error::other(
            "glibc loader cache extension magic differs",
        ));
    }
    let section_count = usize::try_from(glibc_cache_u32(
        bytes,
        extension_offset + 4,
        "extension section count",
    )?)
    .map_err(|_| io::Error::other("glibc loader cache extension count is not representable"))?;
    if section_count == 0 || section_count > MAX_GLIBC_CACHE_EXTENSION_SECTIONS {
        return Err(io::Error::other(
            "glibc loader cache extension count is outside its bound",
        ));
    }
    let section_bytes = section_count
        .checked_mul(GLIBC_CACHE_EXTENSION_SECTION_SIZE)
        .ok_or_else(|| io::Error::other("glibc loader cache extension table size overflow"))?;
    let section_table = extension_offset
        .checked_add(GLIBC_CACHE_EXTENSION_HEADER_SIZE)
        .ok_or_else(|| io::Error::other("glibc loader cache extension table offset overflow"))?;
    let section_table_end = section_table
        .checked_add(section_bytes)
        .ok_or_else(|| io::Error::other("glibc loader cache extension table range overflow"))?;
    if section_table_end > bytes.len() {
        return Err(io::Error::other(
            "glibc loader cache extension table is truncated",
        ));
    }

    let mut tags = BTreeSet::new();
    let mut ranges = Vec::with_capacity(section_count);
    for index in 0..section_count {
        let at = section_table
            .checked_add(
                index
                    .checked_mul(GLIBC_CACHE_EXTENSION_SECTION_SIZE)
                    .ok_or_else(|| {
                        io::Error::other("glibc loader cache extension section index overflow")
                    })?,
            )
            .ok_or_else(|| {
                io::Error::other("glibc loader cache extension section offset overflow")
            })?;
        let tag = glibc_cache_u32(bytes, at, "extension section tag")?;
        let flags = glibc_cache_u32(bytes, at + 4, "extension section flags")?;
        if tag >= MAX_GLIBC_CACHE_EXTENSION_SECTIONS as u32 || flags != 0 || !tags.insert(tag) {
            return Err(io::Error::other(
                "glibc loader cache extension section metadata differs",
            ));
        }
        let start = usize::try_from(glibc_cache_u32(bytes, at + 8, "extension section offset")?)
            .map_err(|_| {
                io::Error::other("glibc loader cache extension section offset is not representable")
            })?;
        let length = usize::try_from(glibc_cache_u32(bytes, at + 12, "extension section size")?)
            .map_err(|_| {
                io::Error::other("glibc loader cache extension section size is not representable")
            })?;
        let end = start
            .checked_add(length)
            .ok_or_else(|| io::Error::other("glibc loader cache extension section overflow"))?;
        if length == 0 || start < section_table_end || end > bytes.len() {
            return Err(io::Error::other(
                "glibc loader cache extension section range differs",
            ));
        }
        ranges.push((start, end));
    }
    ranges.sort_unstable();
    let mut cursor = section_table_end;
    for (start, end) in ranges {
        if start < cursor || bytes[cursor..start].iter().any(|byte| *byte != 0) {
            return Err(io::Error::other(
                "glibc loader cache extension sections overlap or have nonzero padding",
            ));
        }
        cursor = end;
    }
    if cursor != bytes.len() {
        return Err(io::Error::other(
            "glibc loader cache extension has undeclared trailing bytes",
        ));
    }
    Ok(())
}

fn parse_glibc_loader_cache(bytes: &[u8]) -> io::Result<Vec<GlibcLoaderCacheEntry<'_>>> {
    if bytes.len() > MAX_LOADER_CACHE_FILE {
        return Err(io::Error::other(
            "glibc loader cache exceeds its byte bound",
        ));
    }
    if bytes.len() < GLIBC_CACHE_HEADER_SIZE
        || bytes.get(..GLIBC_CACHE_MAGIC.len()) != Some(GLIBC_CACHE_MAGIC)
    {
        return Err(io::Error::other(
            "loader cache is not exact glibc new-format 1.1",
        ));
    }
    if bytes[28] != GLIBC_CACHE_LITTLE_ENDIAN || bytes[29..32] != [0; 3] || bytes[36..48] != [0; 12]
    {
        return Err(io::Error::other(
            "glibc loader cache header is not fixed little-endian",
        ));
    }

    let entry_count = usize::try_from(glibc_cache_u32(bytes, 20, "entry count")?)
        .map_err(|_| io::Error::other("glibc loader cache entry count is not representable"))?;
    if entry_count > MAX_GLIBC_CACHE_ENTRIES {
        return Err(io::Error::other(
            "glibc loader cache entry count exceeds its bound",
        ));
    }
    let entry_bytes = entry_count
        .checked_mul(GLIBC_CACHE_ENTRY_SIZE)
        .ok_or_else(|| io::Error::other("glibc loader cache entry table size overflow"))?;
    let string_start = GLIBC_CACHE_HEADER_SIZE
        .checked_add(entry_bytes)
        .ok_or_else(|| io::Error::other("glibc loader cache entry table range overflow"))?;
    let string_length = usize::try_from(glibc_cache_u32(bytes, 24, "string-table length")?)
        .map_err(|_| io::Error::other("glibc loader cache string length is not representable"))?;
    let string_end = string_start
        .checked_add(string_length)
        .ok_or_else(|| io::Error::other("glibc loader cache string table range overflow"))?;
    if string_end > bytes.len() {
        return Err(io::Error::other(
            "glibc loader cache declared string table is truncated",
        ));
    }

    let extension_offset = usize::try_from(glibc_cache_u32(bytes, 32, "extension offset")?)
        .map_err(|_| {
            io::Error::other("glibc loader cache extension offset is not representable")
        })?;
    if extension_offset == 0 {
        if string_end != bytes.len() {
            return Err(io::Error::other(
                "glibc loader cache has undeclared trailing bytes",
            ));
        }
    } else {
        // New-format caches may carry a digest-bound extension after their
        // declared string table. Alias authority never comes from that opaque
        // extension: every admitted target has one unique ordinary entry and
        // that entry must carry an exact zero hwcap selector below.
        let extension_header_end = extension_offset
            .checked_add(GLIBC_CACHE_EXTENSION_HEADER_SIZE)
            .ok_or_else(|| io::Error::other("glibc loader cache extension range overflow"))?;
        if extension_offset < string_end
            || extension_offset % 4 != 0
            || extension_header_end > bytes.len()
            || bytes[string_end..extension_offset]
                .iter()
                .any(|byte| *byte != 0)
        {
            return Err(io::Error::other(
                "glibc loader cache extension boundary is not exact",
            ));
        }
        validate_glibc_cache_extension(bytes, extension_offset)?;
    }

    let mut entries = Vec::with_capacity(entry_count);
    for index in 0..entry_count {
        let offset = GLIBC_CACHE_HEADER_SIZE
            .checked_add(
                index
                    .checked_mul(GLIBC_CACHE_ENTRY_SIZE)
                    .ok_or_else(|| io::Error::other("glibc loader cache entry index overflow"))?,
            )
            .ok_or_else(|| io::Error::other("glibc loader cache entry offset overflow"))?;
        let key = glibc_cache_string(
            bytes,
            glibc_cache_u32(bytes, offset + 4, "key offset")?,
            (string_start, string_end),
            "key",
        )?;
        let key = std::str::from_utf8(key)
            .ok()
            .filter(|key| valid_loader_name(key))
            .ok_or_else(|| io::Error::other("glibc loader cache key is not an exact SONAME"))?;
        let value = glibc_cache_string(
            bytes,
            glibc_cache_u32(bytes, offset + 8, "value offset")?,
            (string_start, string_end),
            "value",
        )?;
        glibc_cache_alias_path(value)?;
        entries.push(GlibcLoaderCacheEntry {
            flags: glibc_cache_u32(bytes, offset, "entry flags")?,
            key: key.as_bytes(),
            value,
            osversion: glibc_cache_u32(bytes, offset + 12, "entry OS version")?,
            hwcap: glibc_cache_u64(bytes, offset + 16, "entry hwcap")?,
        });
    }
    Ok(entries)
}

#[derive(Clone, Debug)]
pub(crate) struct LiteinstLoaderCacheAlias {
    soname: String,
    raw_path: PathBuf,
    raw_link: PathBuf,
    raw_link_stamp: StableFileStamp,
    dependency_stamp: StableFileStamp,
    dependency: LiteinstCallerImage,
}

impl LiteinstLoaderCacheAlias {
    pub(crate) fn soname(&self) -> &str {
        &self.soname
    }

    pub(crate) fn raw_path(&self) -> &Path {
        &self.raw_path
    }

    pub(crate) fn raw_link(&self) -> &Path {
        &self.raw_link
    }

    pub(crate) fn raw_link_stamp(&self) -> StableFileStamp {
        self.raw_link_stamp
    }

    pub(crate) fn dependency_stamp(&self) -> StableFileStamp {
        self.dependency_stamp
    }

    pub(crate) fn dependency(&self) -> &LiteinstCallerImage {
        &self.dependency
    }

    pub(crate) fn revalidate(&self) -> io::Result<()> {
        let observed =
            observe_glibc_loader_cache_alias(&self.raw_path, &self.dependency, &self.soname)?;
        if observed.raw_link != self.raw_link
            || observed.raw_link_stamp != self.raw_link_stamp
            || observed.dependency_stamp != self.dependency_stamp
        {
            return Err(io::Error::other(format!(
                "loader cache alias symlink or dependency stable stamp changed for {}",
                self.soname
            )));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct LoaderCacheAliasEvidence {
    raw_link: PathBuf,
    raw_link_stamp: StableFileStamp,
    dependency_stamp: StableFileStamp,
}

#[cfg(test)]
fn bind_glibc_loader_cache_alias_targets(
    cache: &LiteinstLoaderCache,
    targets: &[(String, LiteinstCallerImage)],
) -> io::Result<BTreeMap<PathBuf, LiteinstLoaderCacheAlias>> {
    bind_glibc_loader_cache_alias_targets_with_profile(cache, targets, false)
}

fn bind_glibc_loader_cache_alias_targets_with_profile(
    cache: &LiteinstLoaderCache,
    targets: &[(String, LiteinstCallerImage)],
    require_exact_profile: bool,
) -> io::Result<BTreeMap<PathBuf, LiteinstLoaderCacheAlias>> {
    if require_exact_profile && !loader_cache_alias_target_profile_is_exact(targets) {
        return Err(io::Error::other(
            "loader cache targets differ from the zero-or-one reviewed libgcc profile",
        ));
    }
    let entries = parse_glibc_loader_cache(&cache.bytes)?;
    let mut target_names = BTreeSet::new();
    let mut aliases = BTreeMap::new();
    for (soname, dependency) in targets {
        if !valid_loader_name(soname) || !target_names.insert(soname.as_str()) {
            return Err(io::Error::other(
                "deferred loader cache target SONAME is invalid or duplicated",
            ));
        }
        let mut matching = entries
            .iter()
            .filter(|entry| entry.key == soname.as_bytes());
        let entry = matching.next().ok_or_else(|| {
            io::Error::other(format!(
                "loader cache lacks exact deferred dependency {soname}"
            ))
        })?;
        if matching.next().is_some() {
            return Err(io::Error::other(format!(
                "loader cache has duplicate or alternative entries for {soname}"
            )));
        }
        if entries
            .iter()
            .filter(|candidate| candidate.value == entry.value)
            .count()
            != 1
        {
            return Err(io::Error::other(format!(
                "loader cache raw alias is shared by another SONAME for {soname}"
            )));
        }
        if entry.flags != GLIBC_CACHE_ENTRY_FLAGS_X86_64 || entry.osversion != 0 || entry.hwcap != 0
        {
            return Err(io::Error::other(format!(
                "loader cache metadata differs for deferred dependency {soname}"
            )));
        }
        let raw_path = glibc_cache_alias_path(entry.value)?;
        if require_exact_profile && raw_path != Path::new(PROFILED_LOADER_CACHE_ALIAS_RAW_PATH) {
            return Err(io::Error::other(
                "loader cache raw alias differs from the reviewed libgcc profile",
            ));
        }
        let evidence = observe_glibc_loader_cache_alias(&raw_path, dependency, soname)?;
        let alias = LiteinstLoaderCacheAlias {
            soname: soname.clone(),
            raw_path: raw_path.clone(),
            raw_link: evidence.raw_link,
            raw_link_stamp: evidence.raw_link_stamp,
            dependency_stamp: evidence.dependency_stamp,
            dependency: dependency.clone(),
        };
        alias.revalidate()?;
        if aliases.insert(raw_path, alias).is_some() {
            return Err(io::Error::other(
                "loader cache reuses one raw alias for multiple deferred dependencies",
            ));
        }
    }
    Ok(aliases)
}

fn observe_glibc_loader_cache_alias(
    raw_path: &Path,
    dependency: &LiteinstCallerImage,
    soname: &str,
) -> io::Result<LoaderCacheAliasEvidence> {
    let raw_link_before = std::fs::read_link(raw_path)?;
    let raw_metadata_before = std::fs::symlink_metadata(raw_path)?;
    if !raw_metadata_before.file_type().is_symlink() {
        return Err(io::Error::other(format!(
            "loader cache alias is not a symlink for {soname}"
        )));
    }
    let canonical_before = raw_path.canonicalize()?;
    if canonical_before != dependency.path {
        return Err(io::Error::other(format!(
            "loader cache alias canonical path differs for {soname}"
        )));
    }

    // O_PATH obtains only a path reference, so a cache-selected FIFO or device
    // cannot block or perform ordinary open side effects before fstat proves
    // that the object reached through the raw alias is the exact bound regular
    // file. Opening the raw alias, rather than its earlier canonical spelling,
    // pins the resolution that is actually being authorized.
    let pinned = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_PATH | libc::O_CLOEXEC)
        .open(raw_path)?;
    let before = pinned.metadata()?;
    let dependency_before = std::fs::metadata(&dependency.path)?;
    if !before.is_file()
        || before.len() != dependency.bytes.len() as u64
        || FileIdentity::from_metadata(&before) != dependency.file_identity
        || file_stamp(&before) != file_stamp(&dependency_before)
    {
        return Err(io::Error::other(format!(
            "loader cache alias file identity differs for {soname}"
        )));
    }

    let proc_path = format!("/proc/self/fd/{}", pinned.as_raw_fd());
    let mut file = std::fs::File::open(proc_path)?;
    let opened = file.metadata()?;
    let mut bytes = Vec::new();
    (&mut file)
        .take(
            (dependency.bytes.len() as u64)
                .checked_add(1)
                .ok_or_else(|| io::Error::other("loader cache alias byte bound overflow"))?,
        )
        .read_to_end(&mut bytes)?;
    let after = file.metadata()?;
    let pinned_after = pinned.metadata()?;
    let alias_after = std::fs::metadata(raw_path)?;
    let dependency_after = std::fs::metadata(&dependency.path)?;
    let raw_metadata_after = std::fs::symlink_metadata(raw_path)?;
    let raw_link_after = std::fs::read_link(raw_path)?;
    let canonical_after = raw_path.canonicalize()?;
    if bytes.as_slice() != dependency.bytes.as_ref()
        || canonical_after != dependency.path
        || raw_link_after != raw_link_before
        || file_stamp(&raw_metadata_after) != file_stamp(&raw_metadata_before)
        || file_stamp(&before) != file_stamp(&opened)
        || file_stamp(&before) != file_stamp(&after)
        || file_stamp(&before) != file_stamp(&pinned_after)
        || file_stamp(&before) != file_stamp(&alias_after)
        || file_stamp(&before) != file_stamp(&dependency_after)
    {
        return Err(io::Error::other(format!(
            "loader cache alias complete bytes or stable identity differ for {soname}"
        )));
    }
    Ok(LoaderCacheAliasEvidence {
        raw_link: raw_link_before,
        raw_link_stamp: file_stamp(&raw_metadata_before),
        dependency_stamp: file_stamp(&dependency_before),
    })
}

fn bind_glibc_loader_cache_aliases(
    cache: &LiteinstLoaderCache,
    deferred_dependencies: &[LiteinstCallerImage],
    require_exact_profile: bool,
) -> io::Result<BTreeMap<PathBuf, LiteinstLoaderCacheAlias>> {
    if !require_exact_profile {
        return Ok(BTreeMap::new());
    }
    let targets = deferred_dependencies
        .iter()
        .map(|dependency| {
            dependency
                .dynamic_soname()
                .map(|soname| (soname, dependency.clone()))
                .ok_or_else(|| io::Error::other("deferred loader cache target lacks DT_SONAME"))
        })
        .collect::<io::Result<Vec<_>>>()?;
    let aliases =
        bind_glibc_loader_cache_alias_targets_with_profile(cache, &targets, require_exact_profile)?;
    if require_exact_profile
        && (aliases.len() != 1 || !loader_cache_alias_profile_is_exact(&aliases))
    {
        return Err(io::Error::other(
            "loader cache aliases differ from the one exact reviewed libgcc profile",
        ));
    }
    Ok(aliases)
}

fn loader_cache_alias_target_profile_is_exact(targets: &[(String, LiteinstCallerImage)]) -> bool {
    match targets {
        [] => true,
        [(soname, dependency)] => {
            soname == PROFILED_LOADER_CACHE_ALIAS_SONAME
                && dependency.path.as_path()
                    == Path::new(PROFILED_LOADER_CACHE_ALIAS_DEPENDENCY_PATH)
        }
        _ => false,
    }
}

fn loader_cache_alias_profile_is_exact(
    aliases: &BTreeMap<PathBuf, LiteinstLoaderCacheAlias>,
) -> bool {
    match aliases.len() {
        0 => true,
        1 => aliases.first_key_value().is_some_and(|(raw_path, alias)| {
            raw_path == Path::new(PROFILED_LOADER_CACHE_ALIAS_RAW_PATH)
                && alias.soname() == PROFILED_LOADER_CACHE_ALIAS_SONAME
                && alias.raw_path() == raw_path
                && alias.dependency().path.as_path()
                    == Path::new(PROFILED_LOADER_CACHE_ALIAS_DEPENDENCY_PATH)
        }),
        _ => false,
    }
}

/// Exact role of one immutable schema-3 input.
///
/// These roles describe reviewed controller inputs. Phase 1 deliberately does
/// not claim that exec or the dynamic loader consumes their sealed copies.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) enum ImmutableAfterLoaderRole {
    LoaderCache,
    Executable,
    Interpreter,
    Provider,
    InitialDependency(String),
    DeferredDependency(String),
    Runtime,
    RuntimeMarker,
}

/// One byte-exact immutable copy retained for the lifetime of the configuration.
#[derive(Debug)]
pub(crate) struct ImmutableAfterLoaderArtifact {
    role: ImmutableAfterLoaderRole,
    logical_path: PathBuf,
    source_identity: FileIdentity,
    sealed_source: PathBuf,
    sealed_identity: FileIdentity,
    _sha256: [u8; 32],
    _bytes: Arc<[u8]>,
    file: Arc<std::fs::File>,
}

impl ImmutableAfterLoaderArtifact {
    fn prepare(
        role: ImmutableAfterLoaderRole,
        logical_path: &Path,
        source_identity: FileIdentity,
        bytes: Arc<[u8]>,
    ) -> io::Result<Self> {
        let expected_digest: [u8; 32] = Sha256::digest(bytes.as_ref()).into();
        // SAFETY: the static name is NUL terminated, the flags are supported by
        // the existing runtime path, and a successful descriptor has one owner.
        let descriptor = unsafe {
            libc::memfd_create(
                c"reverie-after-loader-immutable".as_ptr(),
                libc::MFD_CLOEXEC | libc::MFD_ALLOW_SEALING,
            )
        };
        if descriptor < 0 {
            return Err(io::Error::last_os_error());
        }
        let mut file = unsafe { std::fs::File::from_raw_fd(descriptor) };
        file.write_all(&bytes)?;
        if unsafe { libc::fcntl(file.as_raw_fd(), libc::F_ADD_SEALS, IMMUTABLE_FILE_SEALS) } != 0 {
            return Err(io::Error::last_os_error());
        }
        if unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GET_SEALS) } != IMMUTABLE_FILE_SEALS {
            return Err(io::Error::other("immutable after-loader seals differ"));
        }
        let metadata = file.metadata()?;
        if !metadata.is_file() || metadata.len() != bytes.len() as u64 {
            return Err(io::Error::other(
                "immutable after-loader length or file type differs",
            ));
        }
        file.rewind()?;
        let mut readback = Vec::new();
        (&mut file)
            .take(
                u64::try_from(bytes.len())
                    .map_err(|_| io::Error::other("immutable byte length is not representable"))?
                    .checked_add(1)
                    .ok_or_else(|| io::Error::other("immutable readback bound overflow"))?,
            )
            .read_to_end(&mut readback)?;
        let readback_digest: [u8; 32] = Sha256::digest(&readback).into();
        if readback.as_slice() != bytes.as_ref() || readback_digest != expected_digest {
            return Err(io::Error::other(
                "immutable after-loader readback or digest differs",
            ));
        }
        let sealed_identity = FileIdentity::from_metadata(&metadata);
        let sealed_source = PathBuf::from(format!(
            "/proc/{}/fd/{}",
            std::process::id(),
            file.as_raw_fd()
        ));
        let linked_metadata = std::fs::metadata(&sealed_source)?;
        if !linked_metadata.is_file()
            || linked_metadata.len() != bytes.len() as u64
            || FileIdentity::from_metadata(&linked_metadata) != sealed_identity
        {
            return Err(io::Error::other(
                "immutable after-loader proc source identity differs",
            ));
        }
        Ok(Self {
            role,
            logical_path: logical_path.to_path_buf(),
            source_identity,
            sealed_source,
            sealed_identity,
            _sha256: expected_digest,
            _bytes: bytes,
            file: Arc::new(file),
        })
    }

    #[cfg(test)]
    pub(crate) fn role(&self) -> &ImmutableAfterLoaderRole {
        &self.role
    }

    pub(crate) fn logical_path(&self) -> &Path {
        &self.logical_path
    }

    pub(crate) fn source_identity(&self) -> FileIdentity {
        self.source_identity
    }

    pub(crate) fn sealed_source(&self) -> &Path {
        &self.sealed_source
    }

    #[cfg(test)]
    pub(crate) fn raw_fd(&self) -> i32 {
        self.file.as_raw_fd()
    }

    pub(crate) fn bytes(&self) -> &[u8] {
        &self._bytes
    }

    pub(crate) fn sealed_identity(&self) -> FileIdentity {
        self.sealed_identity
    }

    pub(crate) fn seals(&self) -> i32 {
        IMMUTABLE_FILE_SEALS
    }

    #[cfg(test)]
    pub(crate) fn sha256(&self) -> [u8; 32] {
        self._sha256
    }
}

/// Complete immutable controller-side copy of one reviewed schema-3 input set.
#[derive(Debug)]
pub(crate) struct ImmutableAfterLoaderBundle {
    artifacts: BTreeMap<ImmutableAfterLoaderRole, Arc<ImmutableAfterLoaderArtifact>>,
}

impl ImmutableAfterLoaderBundle {
    fn from_artifacts(artifacts: Vec<Arc<ImmutableAfterLoaderArtifact>>) -> io::Result<Arc<Self>> {
        let mut by_role = BTreeMap::new();
        let mut logical_paths = BTreeSet::new();
        let mut source_identities = BTreeSet::new();
        let mut sealed_identities = BTreeSet::new();
        for artifact in artifacts {
            if !logical_paths.insert(artifact.logical_path.clone())
                || !source_identities.insert(artifact.source_identity)
                || !sealed_identities.insert(artifact.sealed_identity)
                || by_role.insert(artifact.role.clone(), artifact).is_some()
            {
                return Err(io::Error::other(
                    "immutable after-loader role, path or identity is aliased",
                ));
            }
        }
        Ok(Arc::new(Self { artifacts: by_role }))
    }

    #[cfg(test)]
    pub(crate) fn artifact(
        &self,
        role: &ImmutableAfterLoaderRole,
    ) -> Option<&Arc<ImmutableAfterLoaderArtifact>> {
        self.artifacts.get(role)
    }

    pub(crate) fn iter(
        &self,
    ) -> impl Iterator<
        Item = (
            &ImmutableAfterLoaderRole,
            &Arc<ImmutableAfterLoaderArtifact>,
        ),
    > {
        self.artifacts.iter()
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
    reject_unreviewed_loader_selectors(&elf)?;
    reject_dlopen_refusing_flags(dynamic_flags_1(&elf), "staged runtime")?;
    validate_runtime_load_geometry(&elf, bytes.len())?;
    crate::target_loader::validate_host_runtime_elf(bytes)?;

    let mut host_initializer = None;
    let mut legacy_addresses = Vec::new();
    let mut legacy_symbols = Vec::new();
    for (index, symbol) in elf.dynsyms.iter().enumerate() {
        let Some(name) = elf.dynstrtab.get_at(symbol.st_name) else {
            continue;
        };
        if name == HOST_INITIALIZER
            && (symbol.st_bind() != sym::STB_GLOBAL
                || symbol.st_type() != sym::STT_FUNC
                || symbol.st_other != sym::STV_DEFAULT
                || symbol.st_shndx == section_header::SHN_UNDEF as usize
                || symbol.st_shndx >= section_header::SHN_LORESERVE as usize
                || symbol.st_value == 0
                || symbol.st_size == 0
                || !symbol_is_file_backed_executable(&elf, &symbol)
                || host_initializer.replace(index).is_some())
        {
            return Err(io::Error::other(
                "staged runtime host initializer is not one ordinary export",
            ));
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
            dynamic::DT_INIT_ARRAYSZ if array_size.replace(entry.d_val).is_some() => {
                return Err(io::Error::other("duplicate runtime init-array size"));
            }
            _ => {}
        }
    }
    let (array_address, array_size) = match (array_address, array_size) {
        (None, None) => return Ok(()),
        (Some(0), Some(0)) => return Ok(()),
        (Some(address), Some(size)) if address != 0 && size != 0 && size.is_multiple_of(8) => {
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

    for (slot, raw) in array.as_chunks::<8>().0.iter().enumerate() {
        let slot_address = array_address
            .checked_add((slot * 8) as u64)
            .ok_or_else(|| io::Error::other("runtime init-array address overflow"))?;
        let raw = u64::from_le_bytes(*raw);
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
    backing: Arc<ImmutableAfterLoaderArtifact>,
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
        let backing = Arc::new(ImmutableAfterLoaderArtifact::prepare(
            ImmutableAfterLoaderRole::Runtime,
            &stage.path,
            stage.file_identity,
            stage.bytes.clone(),
        )?);
        let file = backing.file.try_clone()?;
        let image = LiteinstCallerImage {
            path: backing.sealed_source.clone(),
            bytes: stage.bytes.clone(),
            file_identity: backing.sealed_identity,
            marker: None,
        };
        Ok(Self {
            file,
            image,
            backing,
        })
    }
}

struct ImmutableBundleInputs<'a> {
    loader_cache: &'a LiteinstLoaderCache,
    executable: &'a LiteinstCallerImage,
    interpreter: &'a LiteinstCallerImage,
    provider: &'a LiteinstCallerImage,
    runtime: &'a LiteinstCallerImage,
    dependencies: &'a [LiteinstCallerImage],
    initial_dependencies: &'a [LiteinstCallerImage],
    deferred_dependencies: &'a [LiteinstCallerImage],
    sealed_runtime: &'a Arc<SealedRuntime>,
}

fn prepare_immutable_bundle(
    inputs: ImmutableBundleInputs<'_>,
) -> io::Result<Arc<ImmutableAfterLoaderBundle>> {
    let ImmutableBundleInputs {
        loader_cache,
        executable,
        interpreter,
        provider,
        runtime,
        dependencies,
        initial_dependencies,
        deferred_dependencies,
        sealed_runtime,
    } = inputs;
    let marker = runtime
        .marker
        .as_ref()
        .ok_or_else(|| io::Error::other("runtime stage marker absent from immutable bundle"))?;
    let mut artifacts = vec![
        Arc::new(ImmutableAfterLoaderArtifact::prepare(
            ImmutableAfterLoaderRole::LoaderCache,
            &loader_cache.path,
            loader_cache.file_identity,
            loader_cache.bytes.clone(),
        )?),
        Arc::new(ImmutableAfterLoaderArtifact::prepare(
            ImmutableAfterLoaderRole::Executable,
            &executable.path,
            executable.file_identity,
            executable.bytes.clone(),
        )?),
        Arc::new(ImmutableAfterLoaderArtifact::prepare(
            ImmutableAfterLoaderRole::Interpreter,
            &interpreter.path,
            interpreter.file_identity,
            interpreter.bytes.clone(),
        )?),
        Arc::new(ImmutableAfterLoaderArtifact::prepare(
            ImmutableAfterLoaderRole::Provider,
            &provider.path,
            provider.file_identity,
            provider.bytes.clone(),
        )?),
        sealed_runtime.backing.clone(),
        Arc::new(ImmutableAfterLoaderArtifact::prepare(
            ImmutableAfterLoaderRole::RuntimeMarker,
            &marker.path,
            marker.file_identity,
            marker.bytes.clone(),
        )?),
    ];
    for dependency in dependencies {
        let soname = dependency
            .dynamic_soname()
            .ok_or_else(|| io::Error::other("immutable dependency lacks DT_SONAME"))?;
        let initial = initial_dependencies
            .iter()
            .any(|candidate| candidate.file_identity == dependency.file_identity);
        let deferred = deferred_dependencies
            .iter()
            .any(|candidate| candidate.file_identity == dependency.file_identity);
        let role = match (initial, deferred) {
            (true, false) => ImmutableAfterLoaderRole::InitialDependency(soname),
            (false, true) => ImmutableAfterLoaderRole::DeferredDependency(soname),
            _ => {
                return Err(io::Error::other(
                    "immutable dependency has missing or ambiguous loader phase",
                ));
            }
        };
        artifacts.push(Arc::new(ImmutableAfterLoaderArtifact::prepare(
            role,
            &dependency.path,
            dependency.file_identity,
            dependency.bytes.clone(),
        )?));
    }
    ImmutableAfterLoaderBundle::from_artifacts(artifacts)
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
    pub(crate) _interpreter: LiteinstCallerImage,
    pub(crate) provider: LiteinstCallerImage,
    pub(crate) runtime: LiteinstCallerImage,
    pub(crate) sealed_runtime: Arc<SealedRuntime>,
    pub(crate) _immutable_bundle: Arc<ImmutableAfterLoaderBundle>,
    _loader_policy: LiteinstLoaderPolicy,
    loader_cache: LiteinstLoaderCache,
    loader_cache_aliases: BTreeMap<PathBuf, LiteinstLoaderCacheAlias>,
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

struct LiteinstAfterLoaderInputs {
    executable: LiteinstCallerImage,
    interpreter: LiteinstCallerImage,
    provider: LiteinstCallerImage,
    runtime: LiteinstCallerImage,
    loader_cache: LiteinstLoaderCache,
    dependencies: Vec<LiteinstCallerImage>,
    loader_policy: LiteinstLoaderPolicy,
    environment: BTreeMap<OsString, OsString>,
}

impl LiteinstAfterLoaderConfig {
    /// Finish binding inputs already admitted by a reviewed manifest profile.
    ///
    /// This stays private so callers cannot bypass the digest-bound review in
    /// [`LiteinstAfterLoaderProfile::bind_reviewed_manifest`]. It deliberately
    /// retains all of the original graph, environment, runtime-marker and
    /// sealing checks as independent rechecks after manifest validation.
    fn new(inputs: LiteinstAfterLoaderInputs) -> io::Result<Self> {
        let LiteinstAfterLoaderInputs {
            executable,
            interpreter,
            provider,
            runtime,
            loader_cache,
            dependencies,
            loader_policy,
            environment,
        } = inputs;
        if dependencies
            .len()
            .checked_add(2)
            .is_none_or(|count| count > 32)
            || executable.bytes.len() > MAX_CALLER_FILE
            || interpreter.bytes.len() > MAX_CALLER_FILE
            || provider.bytes.len() > MAX_CALLER_FILE
            || dependencies
                .iter()
                .any(|image| image.bytes.len() > MAX_CALLER_FILE)
            || runtime.bytes.len() > MAX_RUNTIME_FILE
            || loader_cache.bytes.len() > MAX_LOADER_CACHE_FILE
        {
            return Err(io::Error::other("caller dependency bound exceeded"));
        }
        if loader_policy.loader_cache_path() != loader_cache.path.as_path() {
            return Err(io::Error::other(
                "loader policy cache path differs from bound artifact",
            ));
        }
        match std::fs::symlink_metadata(loader_policy.system_preload_path()) {
            Ok(_) => {
                return Err(io::Error::other(
                    "loader policy expected-absent system preload exists",
                ));
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        if environment.len() > 256
            || environment.iter().any(|(key, value)| {
                let key = key.as_bytes();
                key.is_empty()
                    || key.contains(&0)
                    || key.contains(&b'=')
                    || value.as_bytes().contains(&0)
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
        let (initial_dependencies, deferred_dependencies) = partition_loader_images(
            &executable,
            &interpreter,
            &provider,
            &runtime,
            &dependencies,
        )?;
        let loader_cache_aliases = bind_glibc_loader_cache_aliases(
            &loader_cache,
            &deferred_dependencies,
            loader_policy.admits_profiled_real_file_alias(),
        )?;
        let immutable_bundle = prepare_immutable_bundle(ImmutableBundleInputs {
            loader_cache: &loader_cache,
            executable: &executable,
            interpreter: &interpreter,
            provider: &provider,
            runtime: &runtime,
            dependencies: &dependencies,
            initial_dependencies: &initial_dependencies,
            deferred_dependencies: &deferred_dependencies,
            sealed_runtime: &sealed_runtime,
        })?;
        let dependencies = std::iter::once(interpreter.clone())
            .chain(dependencies)
            .collect::<Vec<_>>();
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
            "immutable reviewed bundle retained",
            None,
            format!(
                "artifact_count={} {} consumption=phase-2-unenforced",
                immutable_bundle.iter().count(),
                loader_policy.diagnostic(),
            ),
        )?;
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
            _interpreter: interpreter,
            provider,
            runtime,
            sealed_runtime,
            _immutable_bundle: immutable_bundle,
            _loader_policy: loader_policy,
            loader_cache,
            loader_cache_aliases,
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

    /// Return the exact canonical Unix path of the manifest-bound runtime
    /// marker. The marker bytes and digest were authenticated during binding;
    /// this accessor grants no construction or rebinding authority.
    pub fn runtime_marker_path(&self) -> &Path {
        &self
            .runtime
            .marker
            .as_ref()
            .expect("bound after-loader runtime always retains its marker")
            .path
    }

    pub(crate) fn loader_cache(&self) -> &LiteinstLoaderCache {
        &self.loader_cache
    }

    pub(crate) fn loader_cache_alias_for_path(
        &self,
        raw_path: &[u8],
    ) -> Option<&LiteinstLoaderCacheAlias> {
        self._loader_policy
            .admits_profiled_real_file_alias()
            .then(|| {
                self.loader_cache_aliases
                    .get(Path::new(OsStr::from_bytes(raw_path)))
            })
            .flatten()
    }

    pub(crate) fn loader_cache_aliases(&self) -> impl Iterator<Item = &LiteinstLoaderCacheAlias> {
        self.loader_cache_aliases
            .values()
            .filter(|_| self._loader_policy.admits_profiled_real_file_alias())
    }

    pub(crate) fn immutable_loader_cache(&self) -> &ImmutableAfterLoaderArtifact {
        self._immutable_bundle
            .artifacts
            .get(&ImmutableAfterLoaderRole::LoaderCache)
            .expect("bound after-loader bundle always retains its loader cache")
    }

    #[cfg(test)]
    pub(crate) fn immutable_bundle(&self) -> &Arc<ImmutableAfterLoaderBundle> {
        &self._immutable_bundle
    }

    #[cfg(test)]
    pub(crate) fn loader_policy(&self) -> &LiteinstLoaderPolicy {
        &self._loader_policy
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct LoaderImageContract {
    soname: Option<String>,
    needed: BTreeSet<String>,
    interpreter: Option<PathBuf>,
    flags_1: u64,
}

fn valid_loader_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._+-".contains(&byte))
}

const DT_FEATURE_1: u64 = 0x6fff_fdfc;
const DT_POSFLAG_1: u64 = 0x6fff_fdfd;
const DT_GNU_PRELINKED: u64 = 0x6fff_fdf5;
const DT_GNU_CONFLICTSZ: u64 = 0x6fff_fdf6;
const DT_GNU_LIBLISTSZ: u64 = 0x6fff_fdf7;
const DT_SYMINSZ: u64 = 0x6fff_fdfe;
const DT_SYMINENT: u64 = 0x6fff_fdff;
const DT_GNU_CONFLICT: u64 = 0x6fff_fef8;
const DT_SYMINFO: u64 = 0x6fff_feff;
const DT_AUXILIARY: u64 = 0x7fff_fffd;
const DT_FILTER: u64 = 0x7fff_ffff;
const REVIEWED_NON_SELECTOR_FLAGS: u64 =
    dynamic::DF_TEXTREL | dynamic::DF_BIND_NOW | dynamic::DF_STATIC_TLS;
const REVIEWED_NON_SELECTOR_FLAGS_1: u64 =
    dynamic::DF_1_NOW | dynamic::DF_1_NODELETE | dynamic::DF_1_NOOPEN | dynamic::DF_1_NODUMP;
const DLOPEN_REFUSING_FLAGS_1: u64 = dynamic::DF_1_NOOPEN;

fn dynamic_flags_1(elf: &Elf<'_>) -> u64 {
    elf.dynamic
        .as_ref()
        .into_iter()
        .flat_map(|table| table.dyns.iter())
        .filter(|entry| entry.d_tag == dynamic::DT_FLAGS_1)
        .fold(0, |flags, entry| flags | entry.d_val)
}

fn reject_dlopen_refusing_flags(flags_1: u64, role: &str) -> io::Result<()> {
    if flags_1 & DLOPEN_REFUSING_FLAGS_1 != 0 {
        return Err(io::Error::other(format!(
            "{role} carries DT_FLAGS_1 bits that refuse ordinary dlopen"
        )));
    }
    Ok(())
}

fn reject_unreviewed_loader_selectors(elf: &Elf<'_>) -> io::Result<()> {
    let Some(table) = elf.dynamic.as_ref() else {
        return Ok(());
    };
    for entry in &table.dyns {
        let forbidden_tag = matches!(
            entry.d_tag,
            dynamic::DT_RPATH
                | dynamic::DT_RUNPATH
                | dynamic::DT_AUDIT
                | dynamic::DT_DEPAUDIT
                | dynamic::DT_CONFIG
                | dynamic::DT_GNU_LIBLIST
                | DT_GNU_PRELINKED
                | DT_GNU_CONFLICTSZ
                | DT_GNU_LIBLISTSZ
                | DT_FEATURE_1
                | DT_POSFLAG_1
                | DT_SYMINSZ
                | DT_SYMINENT
                | DT_GNU_CONFLICT
                | DT_SYMINFO
                | DT_AUXILIARY
                | DT_FILTER
                | dynamic::DT_SYMBOLIC
        );
        let forbidden_flags =
            entry.d_tag == dynamic::DT_FLAGS && entry.d_val & !REVIEWED_NON_SELECTOR_FLAGS != 0;
        let forbidden_flags_1 =
            entry.d_tag == dynamic::DT_FLAGS_1 && entry.d_val & !REVIEWED_NON_SELECTOR_FLAGS_1 != 0;
        if forbidden_tag || forbidden_flags || forbidden_flags_1 {
            return Err(io::Error::other(format!(
                "bound ELF contains unreviewed loader selector tag {:#x}",
                entry.d_tag
            )));
        }
    }
    Ok(())
}

fn raw_loader_interpreter(elf: &Elf<'_>, bytes: &[u8]) -> io::Result<Option<PathBuf>> {
    let mut interpreter = None;
    for header in &elf.program_headers {
        if header.p_type != ph::PT_INTERP {
            continue;
        }
        if interpreter.is_some() {
            return Err(io::Error::other(
                "bound executable has multiple PT_INTERP segments",
            ));
        }
        let offset = usize::try_from(header.p_offset)
            .map_err(|_| io::Error::other("bound PT_INTERP offset is not representable"))?;
        let length = usize::try_from(header.p_filesz)
            .map_err(|_| io::Error::other("bound PT_INTERP size is not representable"))?;
        let end = offset
            .checked_add(length)
            .ok_or_else(|| io::Error::other("bound PT_INTERP range overflow"))?;
        let raw = bytes
            .get(offset..end)
            .ok_or_else(|| io::Error::other("bound PT_INTERP is outside ELF bytes"))?;
        if raw.len() < 2 || raw.last() != Some(&0) || raw[..raw.len() - 1].contains(&0) {
            return Err(io::Error::other(
                "bound PT_INTERP lacks one exact nonempty NUL-terminated path",
            ));
        }
        interpreter = Some(PathBuf::from(OsString::from_vec(
            raw[..raw.len() - 1].to_vec(),
        )));
    }
    Ok(interpreter)
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
    reject_unreviewed_loader_selectors(&elf)?;
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
    for dependency in elf.libraries.iter().copied() {
        if !valid_loader_name(dependency) || !needed.insert(dependency.to_owned()) {
            return Err(io::Error::other(
                "bound loader image has malformed or duplicate DT_NEEDED",
            ));
        }
    }
    let interpreter = raw_loader_interpreter(&elf, &image.bytes)?;
    if interpreter.as_ref().is_some_and(|path| !path.is_absolute()) {
        return Err(io::Error::other(
            "bound executable has non-absolute PT_INTERP",
        ));
    }
    Ok(LoaderImageContract {
        soname,
        needed,
        interpreter,
        flags_1: dynamic_flags_1(&elf),
    })
}

/// Admit an interpreter declaration on the provider only when it is an inert,
/// lexically clean alias for the exact explicit interpreter already bound by
/// the manifest. Linux consults `PT_INTERP` for the main executable; it ignores
/// this segment when libc is mapped as a `DT_NEEDED` provider. Some modern
/// glibc builds nevertheless retain the segment so libc can be executed
/// directly. Every other loader dependency remains forbidden from carrying it.
fn validate_provider_interpreter_contract(
    provider: &LoaderImageContract,
    interpreter: &LiteinstCallerImage,
) -> io::Result<()> {
    let Some(path) = provider.interpreter.as_ref() else {
        return Ok(());
    };
    let raw = path.as_os_str().as_bytes();
    if raw.len() < 2
        || raw.first() != Some(&b'/')
        || raw.last() == Some(&b'/')
        || raw.windows(2).any(|window| window == b"//")
        || raw
            .split(|byte| *byte == b'/')
            .any(|component| component == b"." || component == b"..")
    {
        return Err(io::Error::other(
            "bound provider PT_INTERP path is not lexically canonical",
        ));
    }

    let canonical_before = path.canonicalize()?;
    if canonical_before.as_os_str().as_bytes() != interpreter.path.as_os_str().as_bytes() {
        return Err(io::Error::other(
            "bound provider PT_INTERP does not resolve to the exact interpreter role",
        ));
    }
    // `O_PATH` follows the named alias and gives us an fstat-capable identity
    // handle without consuming a FIFO or invoking a device's ordinary open
    // behavior if the path is swapped between the two canonicalization probes.
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_PATH | libc::O_CLOEXEC)
        .open(path)?;
    let opened = file.metadata()?;
    let canonical_after = path.canonicalize()?;
    let expected_now = std::fs::metadata(&interpreter.path)?;
    if !opened.is_file()
        || canonical_after.as_os_str().as_bytes() != interpreter.path.as_os_str().as_bytes()
        || FileIdentity::from_metadata(&opened) != interpreter.file_identity
        || FileIdentity::from_metadata(&expected_now) != interpreter.file_identity
    {
        return Err(io::Error::other(
            "bound provider PT_INTERP does not resolve to the exact interpreter role",
        ));
    }
    Ok(())
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
    interpreter: &LiteinstCallerImage,
    provider: &LiteinstCallerImage,
    runtime: &LiteinstCallerImage,
    dependencies: &[LiteinstCallerImage],
) -> io::Result<(Vec<LiteinstCallerImage>, Vec<LiteinstCallerImage>)> {
    let executable_contract = loader_image_contract(executable, header::ET_EXEC)?;
    let interpreter_contract = loader_image_contract(interpreter, header::ET_DYN)?;
    let runtime_contract = loader_image_contract(runtime, header::ET_DYN)?;
    if runtime_contract.interpreter.is_some() {
        return Err(io::Error::other("bound runtime unexpectedly has PT_INTERP"));
    }

    if interpreter_contract.interpreter.is_some() {
        return Err(io::Error::other(
            "bound interpreter unexpectedly has PT_INTERP",
        ));
    }
    let mut images = BTreeMap::<String, (&LiteinstCallerImage, LoaderImageContract)>::new();
    for image in std::iter::once(interpreter)
        .chain(std::iter::once(provider))
        .chain(dependencies.iter())
    {
        let contract = loader_image_contract(image, header::ET_DYN)?;
        if !std::ptr::eq(image, provider) && contract.interpreter.is_some() {
            return Err(io::Error::other(
                "bound non-provider loader dependency unexpectedly has PT_INTERP",
            ));
        }
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
    validate_provider_interpreter_contract(&images[&provider_name].1, interpreter)?;
    let graph = images
        .iter()
        .map(|(name, (_, contract))| (name.clone(), contract.needed.clone()))
        .collect::<BTreeMap<_, _>>();

    let executable_interpreter = executable_contract
        .interpreter
        .as_ref()
        .ok_or_else(|| io::Error::other("bound executable lacks PT_INTERP"))?;
    let interpreter_name = interpreter_contract
        .soname
        .clone()
        .ok_or_else(|| io::Error::other("bound interpreter lacks DT_SONAME"))?;
    if executable_interpreter.as_os_str().as_bytes() != interpreter.path.as_os_str().as_bytes()
        || images
            .get(&interpreter_name)
            .map(|(image, _)| image.file_identity)
            != Some(interpreter.file_identity)
    {
        return Err(io::Error::other(
            "bound PT_INTERP differs from the explicit interpreter role",
        ));
    }
    if interpreter_name == provider_name
        || interpreter.path == provider.path
        || interpreter.file_identity == provider.file_identity
    {
        return Err(io::Error::other(
            "bound interpreter and provider roles are aliased",
        ));
    }

    let mut initial_roots = executable_contract.needed;
    initial_roots.insert(interpreter_name);
    let (initial, deferred) =
        partition_loader_dependency_names(&initial_roots, &runtime_contract.needed, &graph)?;
    reject_dlopen_refusing_flags(runtime_contract.flags_1, "bound runtime")?;
    for name in &deferred {
        let contract = &images
            .get(name)
            .expect("deferred closure contains only bound graph nodes")
            .1;
        reject_dlopen_refusing_flags(
            contract.flags_1,
            &format!("deferred loader dependency {name}"),
        )?;
    }
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
