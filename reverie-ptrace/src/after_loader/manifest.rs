//! Digest-bound reviewed manifests for the after-loader experiment.

use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::ffi::OsString;
use std::fs::File;
use std::io;
use std::io::Read;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::ffi::OsStringExt;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::path::PathBuf;

use goblin::elf::Elf;
use goblin::elf::header;
use goblin::elf::program_header as ph;
use sha2::Digest;
use sha2::Sha256;

use super::FileIdentity;
use super::LiteinstAfterLoaderConfig;
use super::LiteinstAfterLoaderInputs;
use super::LiteinstCallerImage;
use super::LiteinstLoaderCache;
use super::LiteinstLoaderPolicy;
use super::file_stamp;
use super::loader_image_contract;
use super::valid_loader_name;

const MAX_REVIEWED_MANIFEST: usize = 64 * 1024;
const DYNAMIC_X86_64_ET_EXEC_V2: &str = "DynamicX86_64EtExecV2";
const DYNAMIC_X86_64_ET_EXEC_V2_PROVIDER: &str = "libc.so.6";
const LOADER_SEARCH_POLICY: &str = "loader_search=sealed-bundle-only";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum LiteinstAfterLoaderProfileKind {
    DynamicX86_64EtExecV2,
}

impl LiteinstAfterLoaderProfileKind {
    fn manifest_name(self) -> &'static str {
        match self {
            Self::DynamicX86_64EtExecV2 => DYNAMIC_X86_64_ET_EXEC_V2,
        }
    }

    fn parse(value: &str) -> io::Result<Self> {
        match value {
            DYNAMIC_X86_64_ET_EXEC_V2 => Ok(Self::DynamicX86_64EtExecV2),
            _ => Err(io::Error::other(
                "unsupported after-loader manifest profile",
            )),
        }
    }
}

/// An opaque approval for one exact reviewed after-loader manifest.
///
/// The value binds both the manifest profile and the SHA-256 of its complete
/// canonical bytes. It contains no path lookup or fallback policy.
#[derive(Clone, Debug)]
pub struct LiteinstAfterLoaderProfile {
    kind: LiteinstAfterLoaderProfileKind,
    manifest_sha256: [u8; 32],
}

impl LiteinstAfterLoaderProfile {
    /// Record review approval for one dynamic x86-64 `ET_EXEC` schema-3 manifest.
    ///
    /// `manifest_sha256` must be exactly 64 lowercase hexadecimal characters.
    /// It is the SHA-256 of the complete canonical schema-3 document, including
    /// its final newline.
    ///
    /// # Safety
    ///
    /// The caller must have independently reviewed the exact manifest bytes and
    /// every bound ELF for properties that static identity checks cannot prove:
    /// in particular, no audit module, interposer, IFUNC, initializer, or other
    /// loader callback may execute guest code during the controller's private
    /// `dlopen` and host-initializer interval. The reviewed raw cache and
    /// expected-absent preload paths must be the exact paths this interpreter
    /// probes in the authoritative reference, and the cache bytes must be the
    /// reference input. Constructing this value attests to that review;
    /// [`Self::bind_reviewed_manifest`] rechecks all mechanically expressible
    /// identity, graph, environment, and ELF invariants. Schema-3 binding
    /// retains immutable controller copies but does not attest that the current
    /// launch path makes the kernel or loader consume them; callers must not
    /// treat phase-1 binding as that missing execution authority.
    pub unsafe fn review_dynamic_x86_64_et_exec_v2(manifest_sha256: &str) -> io::Result<Self> {
        Ok(Self {
            kind: LiteinstAfterLoaderProfileKind::DynamicX86_64EtExecV2,
            manifest_sha256: parse_sha256(manifest_sha256)?,
        })
    }

    /// Compatibility spelling for callers migrating to schema 3.
    ///
    /// This does not admit schema 2 or its weaker path/role contract. It creates
    /// exactly the same schema-3 approval as
    /// [`Self::review_dynamic_x86_64_et_exec_v2`].
    ///
    /// # Safety
    ///
    /// The caller has the identical review obligations documented on
    /// [`Self::review_dynamic_x86_64_et_exec_v2`].
    pub unsafe fn review_dynamic_x86_64_et_exec_v1(manifest_sha256: &str) -> io::Result<Self> {
        unsafe { Self::review_dynamic_x86_64_et_exec_v2(manifest_sha256) }
    }

    /// Bind the exact manifest approved by this profile without running target
    /// code or consulting `ldd`, `PATH`, or the process environment.
    ///
    /// The canonical schema is, in this exact order:
    ///
    /// ```text
    /// schema=3
    /// profile=DynamicX86_64EtExecV2
    /// loader_cache=<lowercase hex canonical absolute path bytes><TAB><lowercase SHA-256>
    /// system_preload=<lowercase hex canonical absolute path bytes><TAB>absent
    /// loader_search=sealed-bundle-only
    /// executable=<lowercase hex canonical absolute path bytes><TAB><lowercase SHA-256>
    /// interpreter=<DT_SONAME><TAB><lowercase hex path bytes><TAB><lowercase SHA-256>
    /// runtime=<lowercase hex canonical absolute path bytes><TAB><lowercase SHA-256>
    /// runtime_marker=<lowercase hex canonical absolute path bytes><TAB><lowercase SHA-256>
    /// provider=libc.so.6<TAB><lowercase hex path bytes><TAB><lowercase SHA-256>
    /// environment=<lowercase hex key bytes><TAB><lowercase hex value bytes>
    /// image=<DT_SONAME><TAB><lowercase hex canonical absolute path bytes><TAB><lowercase SHA-256>
    /// ```
    ///
    /// There may be zero or more `environment` lines, strictly sorted by their
    /// decoded key bytes, followed by zero or more `image` lines strictly sorted
    /// by SONAME. Paths are raw Unix bytes rather than UTF-8 text. No other
    /// fields, aliases, duplicate roles/nodes, or extra lines are accepted, and
    /// the document has exactly one final newline. Cache and preload paths are
    /// profile data so patched loaders are representable without hard-coded
    /// conventional filenames. The exact cache is sealed like every other
    /// artifact. The loader policy is retained for phase-2 enforcement;
    /// schema-3 binding alone does not claim that the kernel or loader consumes
    /// the sealed bundle or observes the declared absent path.
    pub fn bind_reviewed_manifest(
        &self,
        manifest_path: impl AsRef<Path>,
    ) -> io::Result<LiteinstAfterLoaderConfig> {
        let bytes = read_manifest_bytes(manifest_path.as_ref())?;

        // The approval digest is deliberately checked before UTF-8 decoding,
        // schema dispatch, path lookup, or any ELF parsing.
        let actual_digest = Sha256::digest(&bytes);
        if &actual_digest[..] != self.manifest_sha256.as_slice() {
            return Err(io::Error::other(
                "reviewed after-loader manifest SHA-256 differs",
            ));
        }

        let manifest = ReviewedManifest::parse(&bytes)?;
        if manifest.profile != self.kind {
            return Err(io::Error::other(
                "reviewed after-loader manifest profile differs",
            ));
        }
        manifest.bind(self)
    }
}

#[derive(Debug)]
struct ManifestFile {
    path: PathBuf,
    sha256: [u8; 32],
}

#[derive(Debug)]
struct ManifestImage {
    soname: String,
    file: ManifestFile,
}

#[derive(Debug)]
struct ReviewedManifest {
    profile: LiteinstAfterLoaderProfileKind,
    loader_policy: LiteinstLoaderPolicy,
    loader_cache: ManifestFile,
    executable: ManifestFile,
    interpreter: ManifestImage,
    runtime: ManifestFile,
    runtime_marker: ManifestFile,
    provider: ManifestImage,
    environment: BTreeMap<OsString, OsString>,
    images: Vec<ManifestImage>,
}

impl ReviewedManifest {
    fn parse(bytes: &[u8]) -> io::Result<Self> {
        let text = std::str::from_utf8(bytes)
            .map_err(|_| io::Error::other("after-loader manifest is not canonical UTF-8"))?;
        let body = text
            .strip_suffix('\n')
            .ok_or_else(|| io::Error::other("after-loader manifest lacks final newline"))?;
        if body.is_empty() || body.contains(['\r', '\0']) {
            return Err(io::Error::other(
                "after-loader manifest has noncanonical control bytes",
            ));
        }

        let mut lines = body.split('\n');
        require_exact_line(&mut lines, "schema=3", "manifest schema")?;
        let profile = required_value(&mut lines, "profile=", "manifest profile")?;
        let profile = LiteinstAfterLoaderProfileKind::parse(profile)?;
        let loader_cache = required_file(&mut lines, "loader_cache=", "manifest loader cache")?;
        let system_preload =
            required_absent_path(&mut lines, "system_preload=", "manifest system preload")?;
        require_exact_line(&mut lines, LOADER_SEARCH_POLICY, "loader search policy")?;
        let loader_policy = LiteinstLoaderPolicy::exact_cache_and_absent_preload(
            loader_cache.path.clone(),
            system_preload,
        );
        let executable = required_file(&mut lines, "executable=", "manifest executable")?;
        let interpreter = required_image(&mut lines, "interpreter=", "manifest interpreter")?;
        let runtime = required_file(&mut lines, "runtime=", "manifest runtime")?;
        let runtime_marker =
            required_file(&mut lines, "runtime_marker=", "manifest runtime marker")?;
        let provider = required_image(&mut lines, "provider=", "manifest provider")?;
        if provider.soname != DYNAMIC_X86_64_ET_EXEC_V2_PROVIDER {
            return Err(io::Error::other(
                "manifest provider differs from the reviewed profile",
            ));
        }
        if interpreter.soname == provider.soname {
            return Err(io::Error::other(
                "manifest interpreter and provider roles share one SONAME",
            ));
        }

        let mut environment = BTreeMap::new();
        let mut previous_environment = None::<Vec<u8>>;
        let mut images = Vec::new();
        let mut previous_soname = None::<String>;
        let mut saw_image = false;
        for line in lines {
            if let Some(value) = line.strip_prefix("environment=") {
                if saw_image {
                    return Err(io::Error::other(
                        "manifest environment appears after image graph",
                    ));
                }
                let (key_hex, value_hex) = split_exact_once(value, '\t', "manifest environment")?;
                let key = decode_hex(key_hex, false, "manifest environment key")?;
                let value = decode_hex(value_hex, true, "manifest environment value")?;
                if previous_environment
                    .as_ref()
                    .is_some_and(|previous| previous.as_slice() >= key.as_slice())
                {
                    return Err(io::Error::other(
                        "manifest environment is duplicated or not strictly sorted",
                    ));
                }
                previous_environment = Some(key.clone());
                if environment
                    .insert(OsString::from_vec(key), OsString::from_vec(value))
                    .is_some()
                {
                    return Err(io::Error::other("manifest environment key is duplicated"));
                }
                continue;
            }

            let value = line.strip_prefix("image=").ok_or_else(|| {
                io::Error::other("manifest contains an unknown or misplaced field")
            })?;
            saw_image = true;
            let image = parse_image(value, "manifest image")?;
            let soname = image.soname.as_str();
            if previous_soname
                .as_deref()
                .is_some_and(|previous| previous >= soname)
            {
                return Err(io::Error::other(
                    "manifest images are duplicated or not strictly sorted",
                ));
            }
            previous_soname = Some(soname.to_owned());
            images.push(image);
        }

        if images.len().checked_add(2).is_none_or(|count| count > 32) {
            return Err(io::Error::other(
                "manifest image count is outside the reviewed bound",
            ));
        }
        if images
            .iter()
            .any(|image| image.soname == interpreter.soname || image.soname == provider.soname)
        {
            return Err(io::Error::other(
                "manifest dependency aliases an explicit loader role SONAME",
            ));
        }

        let mut paths = BTreeSet::new();
        for path in std::iter::once(&loader_cache.path)
            .chain(std::iter::once(&executable.path))
            .chain(std::iter::once(&interpreter.file.path))
            .chain(std::iter::once(&runtime.path))
            .chain(std::iter::once(&runtime_marker.path))
            .chain(std::iter::once(&provider.file.path))
            .chain(images.iter().map(|image| &image.file.path))
        {
            if !paths.insert(path.to_path_buf()) {
                return Err(io::Error::other("manifest reuses one canonical file path"));
            }
        }

        Ok(Self {
            profile,
            loader_policy,
            loader_cache,
            executable,
            interpreter,
            runtime,
            runtime_marker,
            provider,
            environment,
            images,
        })
    }

    fn bind(self, profile: &LiteinstAfterLoaderProfile) -> io::Result<LiteinstAfterLoaderConfig> {
        let Self {
            profile: _,
            loader_policy,
            loader_cache: loader_cache_file,
            executable: executable_file,
            interpreter: interpreter_file,
            runtime: runtime_file,
            runtime_marker,
            provider: provider_file,
            environment,
            images,
        } = self;
        let loader_cache = LiteinstLoaderCache::read(&loader_cache_file.path)?;
        if loader_cache.path != loader_cache_file.path {
            return Err(io::Error::other(
                "manifest loader cache path changed while binding",
            ));
        }
        require_digest(
            &loader_cache.bytes,
            &loader_cache_file.sha256,
            "manifest loader cache",
        )?;

        let executable = LiteinstCallerImage::read(&executable_file.path)?;
        if executable.path != executable_file.path {
            return Err(io::Error::other(
                "manifest executable path changed while binding",
            ));
        }
        require_digest(
            &executable.bytes,
            &executable_file.sha256,
            "manifest executable",
        )?;
        let executable_metadata = std::fs::metadata(&executable.path)?;
        if FileIdentity::from_metadata(&executable_metadata) != executable.file_identity
            || executable_metadata.permissions().mode() & 0o111 == 0
        {
            return Err(io::Error::other(
                "manifest executable identity or execute permission differs",
            ));
        }
        loader_image_contract(&executable, header::ET_EXEC)?;
        require_exact_pt_interp(&executable.bytes, &interpreter_file.file.path)?;

        let runtime = LiteinstCallerImage::read_runtime(&runtime_file.path, &runtime_marker.path)?;
        if runtime.path != runtime_file.path {
            return Err(io::Error::other(
                "manifest runtime path changed while binding",
            ));
        }
        require_digest(&runtime.bytes, &runtime_file.sha256, "manifest runtime")?;
        if pt_interp_count(&runtime.bytes)? != 0 {
            return Err(io::Error::other(
                "manifest runtime unexpectedly has PT_INTERP",
            ));
        }
        let marker = runtime
            .marker
            .as_ref()
            .ok_or_else(|| io::Error::other("manifest runtime marker was not retained"))?;
        if marker.path != runtime_marker.path {
            return Err(io::Error::other("manifest runtime marker path differs"));
        }
        require_digest(
            &marker.bytes,
            &runtime_marker.sha256,
            "manifest runtime marker",
        )?;

        let mut identities = BTreeSet::<FileIdentity>::new();
        identities.insert(loader_cache.file_identity);
        if !identities.insert(executable.file_identity)
            || !identities.insert(runtime.file_identity)
            || !identities.insert(marker.file_identity)
        {
            return Err(io::Error::other(
                "manifest cache, executable, runtime or marker roles are aliased",
            ));
        }

        let interpreter =
            bind_manifest_image(interpreter_file, "manifest interpreter", &mut identities)?;
        let provider = bind_manifest_image(provider_file, "manifest provider", &mut identities)?;
        let dependencies = images
            .into_iter()
            .map(|image| bind_manifest_image(image, "manifest dependency", &mut identities))
            .collect::<io::Result<Vec<_>>>()?;

        // This private constructor deliberately repeats the environment, graph,
        // PT_INTERP, runtime-marker, runtime-sealing, and closure checks.
        let config = LiteinstAfterLoaderConfig::new(LiteinstAfterLoaderInputs {
            executable,
            interpreter,
            provider,
            runtime,
            loader_cache,
            dependencies,
            loader_policy,
            environment,
        })?;
        config.diagnostics.record(
            "reviewed manifest bound",
            None,
            format!(
                "profile={} manifest_sha256={}",
                profile.kind.manifest_name(),
                encode_hex(&profile.manifest_sha256),
            ),
        )?;
        Ok(config)
    }
}

fn bind_manifest_image(
    image: ManifestImage,
    label: &str,
    identities: &mut BTreeSet<FileIdentity>,
) -> io::Result<LiteinstCallerImage> {
    let caller_image = LiteinstCallerImage::read(&image.file.path)?;
    if caller_image.path != image.file.path {
        return Err(io::Error::other(format!(
            "{label} path changed while binding"
        )));
    }
    require_digest(&caller_image.bytes, &image.file.sha256, label)?;
    if !identities.insert(caller_image.file_identity) {
        return Err(io::Error::other(format!(
            "{label} aliases another manifest role by file identity"
        )));
    }
    let contract = loader_image_contract(&caller_image, header::ET_DYN)?;
    if pt_interp_count(&caller_image.bytes)? != 0
        || contract.soname.as_deref() != Some(image.soname.as_str())
    {
        return Err(io::Error::other(format!(
            "{label} SONAME or PT_INTERP differs from ELF"
        )));
    }
    Ok(caller_image)
}

fn read_manifest_bytes(path: &Path) -> io::Result<Vec<u8>> {
    let mut file = File::open(path)?;
    let before = file.metadata()?;
    if !before.is_file() {
        return Err(io::Error::other(
            "reviewed after-loader manifest is not a regular file",
        ));
    }
    let mut bytes = Vec::new();
    (&mut file)
        .take(MAX_REVIEWED_MANIFEST as u64 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.is_empty()
        || bytes.len() > MAX_REVIEWED_MANIFEST
        || file_stamp(&before) != file_stamp(&file.metadata()?)
        || file_stamp(&before) != file_stamp(&std::fs::metadata(path)?)
    {
        return Err(io::Error::other(
            "reviewed after-loader manifest is empty, oversized, or changed",
        ));
    }
    Ok(bytes)
}

fn required_value<'a>(
    lines: &mut impl Iterator<Item = &'a str>,
    prefix: &str,
    label: &str,
) -> io::Result<&'a str> {
    lines
        .next()
        .and_then(|line| line.strip_prefix(prefix))
        .filter(|value| !value.is_empty())
        .ok_or_else(|| io::Error::other(format!("{label} is missing or misplaced")))
}

fn require_exact_line<'a>(
    lines: &mut impl Iterator<Item = &'a str>,
    expected: &str,
    label: &str,
) -> io::Result<()> {
    if lines.next() != Some(expected) {
        return Err(io::Error::other(format!(
            "unsupported or misplaced {label}"
        )));
    }
    Ok(())
}

fn required_file<'a>(
    lines: &mut impl Iterator<Item = &'a str>,
    prefix: &str,
    label: &str,
) -> io::Result<ManifestFile> {
    let value = required_value(lines, prefix, label)?;
    let (path, digest) = split_exact_once(value, '\t', label)?;
    Ok(ManifestFile {
        path: canonical_path(path, label)?,
        sha256: parse_sha256(digest)?,
    })
}

fn required_absent_path<'a>(
    lines: &mut impl Iterator<Item = &'a str>,
    prefix: &str,
    label: &str,
) -> io::Result<PathBuf> {
    let value = required_value(lines, prefix, label)?;
    let (path, state) = split_exact_once(value, '\t', label)?;
    if state != "absent" {
        return Err(io::Error::other(format!(
            "{label} state is not exactly absent"
        )));
    }
    canonical_absent_path(path, label)
}

fn required_image<'a>(
    lines: &mut impl Iterator<Item = &'a str>,
    prefix: &str,
    label: &str,
) -> io::Result<ManifestImage> {
    parse_image(required_value(lines, prefix, label)?, label)
}

fn parse_image(value: &str, label: &str) -> io::Result<ManifestImage> {
    let mut parts = value.split('\t');
    let soname = parts
        .next()
        .filter(|value| valid_loader_name(value))
        .ok_or_else(|| io::Error::other(format!("{label} has malformed SONAME")))?;
    let path = parts
        .next()
        .ok_or_else(|| io::Error::other(format!("{label} lacks canonical path bytes")))?;
    let sha256 = parts
        .next()
        .ok_or_else(|| io::Error::other(format!("{label} lacks SHA-256")))?;
    if parts.next().is_some() {
        return Err(io::Error::other(format!("{label} has extra fields")));
    }
    Ok(ManifestImage {
        soname: soname.to_owned(),
        file: ManifestFile {
            path: canonical_path(path, label)?,
            sha256: parse_sha256(sha256)?,
        },
    })
}

fn split_exact_once<'a>(
    value: &'a str,
    delimiter: char,
    label: &str,
) -> io::Result<(&'a str, &'a str)> {
    let (left, right) = value
        .split_once(delimiter)
        .ok_or_else(|| io::Error::other(format!("{label} lacks a field separator")))?;
    if left.is_empty() || right.contains(delimiter) {
        return Err(io::Error::other(format!("{label} has noncanonical fields")));
    }
    Ok((left, right))
}

fn decoded_absolute_path(value: &str, label: &str) -> io::Result<(Vec<u8>, PathBuf)> {
    let bytes = decode_hex(value, false, &format!("{label} path"))?;
    if bytes.contains(&0) {
        return Err(io::Error::other(format!(
            "{label} path contains a NUL byte"
        )));
    }
    let path = PathBuf::from(OsString::from_vec(bytes.clone()));
    if !path.is_absolute() {
        return Err(io::Error::other(format!("{label} path is not absolute")));
    }
    if bytes.ends_with(b"/")
        || bytes.windows(2).any(|pair| pair == b"//")
        || bytes
            .split(|byte| *byte == b'/')
            .any(|component| component == b"." || component == b"..")
    {
        return Err(io::Error::other(format!(
            "{label} path contains a textual alias"
        )));
    }
    Ok((bytes, path))
}

fn canonical_path(value: &str, label: &str) -> io::Result<PathBuf> {
    let (bytes, path) = decoded_absolute_path(value, label)?;
    let canonical = path.canonicalize()?;
    if canonical.as_os_str().as_bytes() != bytes.as_slice()
        || !std::fs::metadata(&canonical)?.is_file()
    {
        return Err(io::Error::other(format!(
            "{label} path is not canonical absolute"
        )));
    }
    Ok(canonical)
}

fn canonical_absent_path(value: &str, label: &str) -> io::Result<PathBuf> {
    let (_, path) = decoded_absolute_path(value, label)?;
    match std::fs::symlink_metadata(&path) {
        Ok(_) => {
            return Err(io::Error::other(format!(
                "{label} expected-absent path exists"
            )));
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::other(format!("{label} path has no parent")))?;
    let canonical_parent = parent.canonicalize()?;
    if canonical_parent.as_os_str().as_bytes() != parent.as_os_str().as_bytes() {
        return Err(io::Error::other(format!(
            "{label} parent path is not canonical absolute"
        )));
    }
    Ok(path)
}

fn parse_sha256(value: &str) -> io::Result<[u8; 32]> {
    let bytes = decode_hex(value, false, "SHA-256")?;
    bytes
        .try_into()
        .map_err(|_| io::Error::other("SHA-256 is not exactly 32 bytes"))
}

fn decode_hex(value: &str, allow_empty: bool, label: &str) -> io::Result<Vec<u8>> {
    if (!allow_empty && value.is_empty())
        || !value.len().is_multiple_of(2)
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(io::Error::other(format!(
            "{label} is not canonical lowercase hexadecimal"
        )));
    }
    value
        .as_bytes()
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| {
            let high = hex_nibble(pair[0]);
            let low = hex_nibble(pair[1]);
            Ok((high << 4) | low)
        })
        .collect()
}

fn hex_nibble(byte: u8) -> u8 {
    match byte {
        b'0'..=b'9' => byte - b'0',
        b'a'..=b'f' => byte - b'a' + 10,
        _ => unreachable!("decode_hex validates every nibble"),
    }
}

fn encode_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        encoded.push(HEX[usize::from(byte >> 4)] as char);
        encoded.push(HEX[usize::from(byte & 0xf)] as char);
    }
    encoded
}

fn require_digest(bytes: &[u8], expected: &[u8; 32], label: &str) -> io::Result<()> {
    if &Sha256::digest(bytes)[..] != expected.as_slice() {
        return Err(io::Error::other(format!("{label} SHA-256 differs")));
    }
    Ok(())
}

fn pt_interp_count(bytes: &[u8]) -> io::Result<usize> {
    let elf = Elf::parse(bytes)
        .map_err(|error| io::Error::other(format!("manifest ELF parse failed: {error}")))?;
    Ok(elf
        .program_headers
        .iter()
        .filter(|header| header.p_type == ph::PT_INTERP)
        .count())
}

fn elf_u16(bytes: &[u8], at: usize, label: &str) -> io::Result<u16> {
    let raw = bytes
        .get(
            at..at
                .checked_add(2)
                .ok_or_else(|| io::Error::other(format!("manifest ELF {label} offset overflow")))?,
        )
        .ok_or_else(|| io::Error::other(format!("manifest ELF {label} is truncated")))?;
    Ok(u16::from_le_bytes(raw.try_into().unwrap()))
}

fn elf_u32(bytes: &[u8], at: usize, label: &str) -> io::Result<u32> {
    let raw = bytes
        .get(
            at..at
                .checked_add(4)
                .ok_or_else(|| io::Error::other(format!("manifest ELF {label} offset overflow")))?,
        )
        .ok_or_else(|| io::Error::other(format!("manifest ELF {label} is truncated")))?;
    Ok(u32::from_le_bytes(raw.try_into().unwrap()))
}

fn elf_u64(bytes: &[u8], at: usize, label: &str) -> io::Result<u64> {
    let raw = bytes
        .get(
            at..at
                .checked_add(8)
                .ok_or_else(|| io::Error::other(format!("manifest ELF {label} offset overflow")))?,
        )
        .ok_or_else(|| io::Error::other(format!("manifest ELF {label} is truncated")))?;
    Ok(u64::from_le_bytes(raw.try_into().unwrap()))
}

/// Require the sole ELF64 `PT_INTERP` payload to be exactly the manifest's raw
/// Unix path bytes followed by one NUL. This deliberately does not use
/// `Elf::interpreter`: that convenience view is a parsed C string and cannot
/// prove that the segment contains no ignored suffix or textual path alias.
fn require_exact_pt_interp(bytes: &[u8], expected: &Path) -> io::Result<()> {
    const ELF64_HEADER_SIZE: usize = 64;
    const ELF64_PROGRAM_HEADER_SIZE: usize = 56;
    const MAX_PROGRAM_HEADERS: usize = 128;

    if bytes.len() < ELF64_HEADER_SIZE
        || bytes.get(..7) != Some(&b"\x7fELF\x02\x01\x01"[..])
        || elf_u16(bytes, 16, "type")? != header::ET_EXEC
        || elf_u16(bytes, 18, "machine")? != header::EM_X86_64
        || usize::from(elf_u16(bytes, 52, "header size")?) != ELF64_HEADER_SIZE
    {
        return Err(io::Error::other(
            "manifest executable is not canonical x86-64 ELF64 ET_EXEC",
        ));
    }

    let program_header_offset = usize::try_from(elf_u64(bytes, 32, "program-header offset")?)
        .map_err(|_| io::Error::other("manifest ELF program-header offset is not representable"))?;
    let program_header_size = usize::from(elf_u16(bytes, 54, "program-header size")?);
    let program_header_count = usize::from(elf_u16(bytes, 56, "program-header count")?);
    if program_header_size != ELF64_PROGRAM_HEADER_SIZE
        || program_header_count == 0
        || program_header_count > MAX_PROGRAM_HEADERS
    {
        return Err(io::Error::other(
            "manifest ELF program-header table is outside the fixed bound",
        ));
    }
    let table_size = program_header_size
        .checked_mul(program_header_count)
        .ok_or_else(|| io::Error::other("manifest ELF program-header table size overflow"))?;
    let table_end = program_header_offset
        .checked_add(table_size)
        .ok_or_else(|| io::Error::other("manifest ELF program-header table range overflow"))?;
    if bytes.get(program_header_offset..table_end).is_none() {
        return Err(io::Error::other(
            "manifest ELF program-header table is outside executable bytes",
        ));
    }

    let mut interpreter = None;
    for index in 0..program_header_count {
        let at =
            program_header_offset
                .checked_add(index.checked_mul(program_header_size).ok_or_else(|| {
                    io::Error::other("manifest ELF program-header index overflow")
                })?)
                .ok_or_else(|| io::Error::other("manifest ELF program-header offset overflow"))?;
        if elf_u32(bytes, at, "program-header type")? != ph::PT_INTERP {
            continue;
        }
        if interpreter.is_some() {
            return Err(io::Error::other(
                "manifest executable has multiple PT_INTERP segments",
            ));
        }
        let offset = usize::try_from(elf_u64(bytes, at + 8, "PT_INTERP offset")?)
            .map_err(|_| io::Error::other("manifest PT_INTERP offset is not representable"))?;
        let size = usize::try_from(elf_u64(bytes, at + 32, "PT_INTERP size")?)
            .map_err(|_| io::Error::other("manifest PT_INTERP size is not representable"))?;
        let end = offset
            .checked_add(size)
            .ok_or_else(|| io::Error::other("manifest PT_INTERP range overflow"))?;
        let raw = bytes
            .get(offset..end)
            .ok_or_else(|| io::Error::other("manifest PT_INTERP is outside executable bytes"))?;
        interpreter = Some(raw);
    }

    let raw = interpreter.ok_or_else(|| io::Error::other("manifest executable lacks PT_INTERP"))?;
    if raw.len() < 2 {
        return Err(io::Error::other(
            "manifest PT_INTERP is shorter than two bytes",
        ));
    }
    if raw.last() != Some(&0) {
        return Err(io::Error::other("manifest PT_INTERP lacks its final NUL"));
    }
    let path = &raw[..raw.len() - 1];
    if path.contains(&0) {
        return Err(io::Error::other(
            "manifest PT_INTERP contains an embedded NUL or ignored suffix",
        ));
    }
    if path != expected.as_os_str().as_bytes() {
        return Err(io::Error::other(
            "manifest executable PT_INTERP raw bytes differ",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::ffi::OsStringExt;
    use std::os::unix::fs::PermissionsExt;
    use std::process::Command;
    use std::process::ExitStatus;
    use std::process::Stdio;
    use std::sync::atomic::AtomicU64;
    use std::sync::atomic::Ordering;
    use std::time::Duration;
    use std::time::Instant;

    use goblin::elf::dynamic;
    use goblin::elf::program_header as ph;
    use goblin::elf::sym;

    use super::*;

    static SEQUENCE: AtomicU64 = AtomicU64::new(0);
    const BIND_CHILD: &str = "REVERIE_REVIEWED_MANIFEST_BIND_CHILD";
    const BIND_CHILD_MANIFEST: &str = "REVERIE_REVIEWED_MANIFEST_BIND_CHILD_MANIFEST";
    const BIND_CHILD_DIGEST: &str = "REVERIE_REVIEWED_MANIFEST_BIND_CHILD_DIGEST";
    const BIND_CHILD_OK: &str = "reviewed-manifest-bind-child-ok";
    const BIND_CHILD_AMBIENT_PATH: &str = "/ambient/path/deliberately/unusable";
    const MAX_CHILD_OUTPUT: usize = 32 * 1024;

    fn put16(bytes: &mut [u8], at: usize, value: u16) {
        bytes[at..at + 2].copy_from_slice(&value.to_le_bytes());
    }

    fn put32(bytes: &mut [u8], at: usize, value: u32) {
        bytes[at..at + 4].copy_from_slice(&value.to_le_bytes());
    }

    fn put64(bytes: &mut [u8], at: usize, value: u64) {
        bytes[at..at + 8].copy_from_slice(&value.to_le_bytes());
    }

    fn loader_elf(
        elf_type: u16,
        soname: Option<&str>,
        needed: &[&str],
        interpreter: Option<&Path>,
    ) -> Vec<u8> {
        const FILE_SIZE: usize = 0x2000;
        const DYNAMIC_OFFSET: usize = 0x400;
        const INTERPRETER_OFFSET: usize = 0x500;
        const STRINGS_OFFSET: usize = 0x800;

        let base = if elf_type == header::ET_EXEC {
            0x400000_u64
        } else {
            0
        };
        let mut bytes = vec![0; FILE_SIZE];
        bytes[..7].copy_from_slice(b"\x7fELF\x02\x01\x01");
        put16(&mut bytes, 16, elf_type);
        put16(&mut bytes, 18, header::EM_X86_64);
        put32(&mut bytes, 20, 1);
        put64(&mut bytes, 24, base + 0x1000);
        put64(&mut bytes, 32, 64);
        put16(&mut bytes, 52, 64);
        put16(&mut bytes, 54, 56);
        put16(&mut bytes, 56, if interpreter.is_some() { 3 } else { 2 });

        put32(&mut bytes, 64, ph::PT_LOAD);
        put32(&mut bytes, 68, ph::PF_R | ph::PF_X);
        put64(&mut bytes, 72, 0);
        put64(&mut bytes, 80, base);
        put64(&mut bytes, 96, FILE_SIZE as u64);
        put64(&mut bytes, 104, FILE_SIZE as u64);
        put64(&mut bytes, 112, 0x1000);

        let mut strings = vec![0_u8];
        let mut push_string = |value: &str| {
            let offset = strings.len() as u64;
            strings.extend_from_slice(value.as_bytes());
            strings.push(0);
            offset
        };
        let soname_offset = soname.map(&mut push_string);
        let needed_offsets = needed
            .iter()
            .map(|value| push_string(value))
            .collect::<Vec<_>>();
        bytes[STRINGS_OFFSET..STRINGS_OFFSET + strings.len()].copy_from_slice(&strings);

        let mut entries = vec![
            (dynamic::DT_STRTAB, base + STRINGS_OFFSET as u64),
            (dynamic::DT_STRSZ, strings.len() as u64),
        ];
        if let Some(offset) = soname_offset {
            entries.push((dynamic::DT_SONAME, offset));
        }
        entries.extend(
            needed_offsets
                .into_iter()
                .map(|offset| (dynamic::DT_NEEDED, offset)),
        );
        entries.push((dynamic::DT_NULL, 0));

        let dynamic_size = entries.len() * 16;
        let dynamic_header = 64 + 56;
        put32(&mut bytes, dynamic_header, ph::PT_DYNAMIC);
        put32(&mut bytes, dynamic_header + 4, ph::PF_R);
        put64(&mut bytes, dynamic_header + 8, DYNAMIC_OFFSET as u64);
        put64(
            &mut bytes,
            dynamic_header + 16,
            base + DYNAMIC_OFFSET as u64,
        );
        put64(&mut bytes, dynamic_header + 32, dynamic_size as u64);
        put64(&mut bytes, dynamic_header + 40, dynamic_size as u64);
        put64(&mut bytes, dynamic_header + 48, 8);
        for (index, (tag, value)) in entries.into_iter().enumerate() {
            put64(&mut bytes, DYNAMIC_OFFSET + index * 16, tag);
            put64(&mut bytes, DYNAMIC_OFFSET + index * 16 + 8, value);
        }

        if let Some(interpreter) = interpreter {
            let interpreter = interpreter.as_os_str().as_bytes();
            assert!(interpreter.len() + 1 < STRINGS_OFFSET - INTERPRETER_OFFSET);
            bytes[INTERPRETER_OFFSET..INTERPRETER_OFFSET + interpreter.len()]
                .copy_from_slice(interpreter);
            let interpreter_header = 64 + 2 * 56;
            put32(&mut bytes, interpreter_header, ph::PT_INTERP);
            put32(&mut bytes, interpreter_header + 4, ph::PF_R);
            put64(
                &mut bytes,
                interpreter_header + 8,
                INTERPRETER_OFFSET as u64,
            );
            put64(
                &mut bytes,
                interpreter_header + 16,
                base + INTERPRETER_OFFSET as u64,
            );
            put64(
                &mut bytes,
                interpreter_header + 32,
                (interpreter.len() + 1) as u64,
            );
            put64(
                &mut bytes,
                interpreter_header + 40,
                (interpreter.len() + 1) as u64,
            );
            put64(&mut bytes, interpreter_header + 48, 1);
        }
        bytes
    }

    fn append_dynamic_tag(bytes: &mut [u8], tag: u64, value: u64) {
        let elf = Elf::parse(bytes).unwrap();
        let header_index = elf
            .program_headers
            .iter()
            .position(|header| header.p_type == ph::PT_DYNAMIC)
            .unwrap();
        let header = usize::try_from(elf.header.e_phoff).unwrap()
            + header_index * usize::from(elf.header.e_phentsize);
        let offset = usize::try_from(elf.program_headers[header_index].p_offset).unwrap();
        let size = usize::try_from(elf.program_headers[header_index].p_filesz).unwrap();
        assert_eq!(
            u64::from_le_bytes(
                bytes[offset + size - 16..offset + size - 8]
                    .try_into()
                    .unwrap()
            ),
            dynamic::DT_NULL
        );
        put64(bytes, offset + size - 16, tag);
        put64(bytes, offset + size - 8, value);
        put64(bytes, offset + size, dynamic::DT_NULL);
        put64(bytes, offset + size + 8, 0);
        put64(bytes, header + 32, (size + 16) as u64);
        put64(bytes, header + 40, (size + 16) as u64);
    }

    fn add_synthetic_runtime_needed(bytes: &mut [u8], name: &str) {
        const STRINGS_OFFSET: usize = 0x500;
        const DYNAMIC_OFFSET: usize = 0x2100;
        const STRSZ_VALUE_OFFSET: usize = DYNAMIC_OFFSET + 16 + 8;

        let old_length = usize::try_from(u64::from_le_bytes(
            bytes[STRSZ_VALUE_OFFSET..STRSZ_VALUE_OFFSET + 8]
                .try_into()
                .unwrap(),
        ))
        .unwrap();
        assert_eq!(bytes[STRINGS_OFFSET + old_length - 1], 0);
        let end = STRINGS_OFFSET + old_length + name.len() + 1;
        assert!(end < 0x1000);
        bytes[STRINGS_OFFSET + old_length..end - 1].copy_from_slice(name.as_bytes());
        bytes[end - 1] = 0;
        put64(
            bytes,
            STRSZ_VALUE_OFFSET,
            u64::try_from(old_length + name.len() + 1).unwrap(),
        );
        append_dynamic_tag(bytes, dynamic::DT_NEEDED, old_length as u64);
    }

    // This is the same constructor-disabled runtime shape exercised by the
    // parent module's ELF qualification tests. It is parsed but never loaded or
    // executed by these manifest tests.
    fn runtime_elf() -> Vec<u8> {
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
            let header = 64 + index * 56;
            put32(&mut bytes, header, kind);
            put32(&mut bytes, header + 4, flags);
            put64(&mut bytes, header + 8, at);
            put64(&mut bytes, header + 16, at);
            put64(&mut bytes, header + 32, size);
            put64(&mut bytes, header + 40, size);
            put64(&mut bytes, header + 48, 8);
        }
        let strings = b"\0reverie_liteinst_initialize_host\0reverie_liteinst_initialize\0";
        bytes[0x500..0x500 + strings.len()].copy_from_slice(strings);
        let legacy_name = b"\0reverie_liteinst_initialize_host\0".len() as u32;
        for (index, (name, address)) in [(1, 0x1100), (legacy_name, 0x1120)].into_iter().enumerate()
        {
            let symbol = 0x618 + index * 24;
            put32(&mut bytes, symbol, name);
            bytes[symbol + 4] = (sym::STB_GLOBAL << 4) | sym::STT_FUNC;
            bytes[symbol + 5] = sym::STV_DEFAULT;
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

    fn set_executable(path: &Path) {
        let mut permissions = std::fs::metadata(path).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(path, permissions).unwrap();
    }

    fn digest_file(path: &Path) -> String {
        encode_hex(&Sha256::digest(std::fs::read(path).unwrap()))
    }

    fn digest_text(text: &str) -> String {
        encode_hex(&Sha256::digest(text.as_bytes()))
    }

    struct Fixture {
        directory: PathBuf,
        cache: PathBuf,
        preload: PathBuf,
        executable: PathBuf,
        interpreter: PathBuf,
        loader: PathBuf,
        provider: PathBuf,
        unused: PathBuf,
        extra: PathBuf,
        runtime: PathBuf,
        marker: PathBuf,
        manifest: PathBuf,
    }

    impl Fixture {
        fn new() -> Self {
            let directory = std::env::temp_dir().join(format!(
                "liteinst-reviewed-manifest-{}-{}",
                std::process::id(),
                SEQUENCE.fetch_add(1, Ordering::SeqCst),
            ));
            std::fs::create_dir(&directory).unwrap();
            let directory = directory.canonicalize().unwrap();
            let cache = directory.join("ld-review.cache");
            let preload = directory.join("ld-review.preload");
            let executable = directory.join("reviewed-et-exec");
            let interpreter_directory = directory.join("compat");
            std::fs::create_dir(&interpreter_directory).unwrap();
            let interpreter = interpreter_directory.join("ld-review.so");
            let loader = interpreter.clone();
            let provider = directory.join("libc.so.6");
            let unused = directory.join("libunused.so");
            let extra = directory.join("libzzextra.so");
            let runtime = directory.join("runtime.so");
            let marker = directory.join("runtime.marker");
            let manifest = directory.join("reviewed.manifest");

            std::fs::write(&cache, b"synthetic reviewed loader cache\n").unwrap();
            std::fs::write(
                &loader,
                loader_elf(header::ET_DYN, Some("ld-review.so"), &[], None),
            )
            .unwrap();
            std::fs::write(
                &provider,
                loader_elf(header::ET_DYN, Some("libc.so.6"), &[], None),
            )
            .unwrap();
            std::fs::write(
                &unused,
                loader_elf(header::ET_DYN, Some("libunused.so"), &[], None),
            )
            .unwrap();
            std::fs::write(
                &extra,
                loader_elf(header::ET_DYN, Some("libzzextra.so"), &[], None),
            )
            .unwrap();
            std::fs::write(
                &executable,
                loader_elf(
                    header::ET_EXEC,
                    None,
                    &["libc.so.6", "libunused.so"],
                    Some(&interpreter),
                ),
            )
            .unwrap();
            let runtime_bytes = runtime_elf();
            std::fs::write(&runtime, &runtime_bytes).unwrap();
            std::fs::write(
                &marker,
                LiteinstCallerImage::runtime_stage_marker(&runtime_bytes).unwrap(),
            )
            .unwrap();
            for path in [&executable, &loader, &provider, &unused, &extra, &runtime] {
                set_executable(path);
            }

            Self {
                directory,
                cache,
                preload,
                executable,
                interpreter,
                loader,
                provider,
                unused,
                extra,
                runtime,
                marker,
                manifest,
            }
        }

        fn canonical_manifest(&self) -> String {
            let environment = BTreeMap::from([
                (b"LITEINST_CALLER_SENTINEL".to_vec(), b"preserved".to_vec()),
                (b"PATH".to_vec(), b"/definitely/unusable/tool/path".to_vec()),
            ]);
            let images = BTreeMap::from([("libunused.so", &self.unused)]);
            let mut manifest = format!(
                "schema=3\nprofile={DYNAMIC_X86_64_ET_EXEC_V2}\nloader_cache={}\t{}\nsystem_preload={}\tabsent\n{LOADER_SEARCH_POLICY}\nexecutable={}\t{}\ninterpreter=ld-review.so\t{}\t{}\nruntime={}\t{}\nruntime_marker={}\t{}\nprovider=libc.so.6\t{}\t{}\n",
                encode_hex(self.cache.as_os_str().as_bytes()),
                digest_file(&self.cache),
                encode_hex(self.preload.as_os_str().as_bytes()),
                encode_hex(self.executable.as_os_str().as_bytes()),
                digest_file(&self.executable),
                encode_hex(self.interpreter.as_os_str().as_bytes()),
                digest_file(&self.interpreter),
                encode_hex(self.runtime.as_os_str().as_bytes()),
                digest_file(&self.runtime),
                encode_hex(self.marker.as_os_str().as_bytes()),
                digest_file(&self.marker),
                encode_hex(self.provider.as_os_str().as_bytes()),
                digest_file(&self.provider),
            );
            for (key, value) in environment {
                manifest.push_str(&format!(
                    "environment={}\t{}\n",
                    encode_hex(&key),
                    encode_hex(&value),
                ));
            }
            for (soname, path) in images {
                manifest.push_str(&format!(
                    "image={soname}\t{}\t{}\n",
                    encode_hex(path.as_os_str().as_bytes()),
                    digest_file(path),
                ));
            }
            manifest
        }

        fn write_manifest(&self, manifest: &str) {
            std::fs::write(&self.manifest, manifest).unwrap();
        }

        fn approve(manifest: &str) -> LiteinstAfterLoaderProfile {
            // SAFETY: each test either constructs the complete fixed synthetic
            // ELF graph and inert cache above or deliberately corrupts one
            // declared property; no cache/ELF is loader-consumed and no target
            // code is executed.
            unsafe {
                LiteinstAfterLoaderProfile::review_dynamic_x86_64_et_exec_v2(&digest_text(manifest))
            }
            .unwrap()
        }

        fn bind_reapproved(&self, manifest: &str) -> io::Result<LiteinstAfterLoaderConfig> {
            self.write_manifest(manifest);
            Self::approve(manifest).bind_reviewed_manifest(&self.manifest)
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.directory);
        }
    }

    fn replace_line(manifest: &str, prefix: &str, replacement: &str) -> String {
        let mut found = false;
        let mut result = String::new();
        for line in manifest.lines() {
            if !found && line.starts_with(prefix) {
                result.push_str(replacement);
                found = true;
            } else {
                result.push_str(line);
            }
            result.push('\n');
        }
        assert!(found, "manifest lacks line prefix {prefix:?}");
        result
    }

    fn pt_interp_header(bytes: &[u8]) -> usize {
        let header_index = {
            let elf = Elf::parse(bytes).unwrap();
            elf.program_headers
                .iter()
                .enumerate()
                .find(|(_, header)| header.p_type == ph::PT_INTERP)
                .unwrap()
                .0
        };
        let program_header_offset =
            usize::try_from(elf_u64(bytes, 32, "test phoff").unwrap()).unwrap();
        let program_header_size = usize::from(elf_u16(bytes, 54, "test phentsize").unwrap());
        program_header_offset + header_index * program_header_size
    }

    fn set_pt_interp_payload(bytes: &mut [u8], payload: &[u8]) {
        const MAX_SYNTHETIC_INTERPRETER: usize = 512;

        let header = pt_interp_header(bytes);
        let payload_offset =
            usize::try_from(elf_u64(bytes, header + 8, "test PT_INTERP offset").unwrap()).unwrap();
        assert!(payload.len() <= MAX_SYNTHETIC_INTERPRETER);
        bytes[payload_offset..payload_offset + MAX_SYNTHETIC_INTERPRETER].fill(0);
        bytes[payload_offset..payload_offset + payload.len()].copy_from_slice(payload);
        put64(bytes, header + 32, payload.len() as u64);
        put64(bytes, header + 40, payload.len() as u64);
    }

    fn collect_bounded(mut reader: impl std::io::Read) -> io::Result<(Vec<u8>, bool)> {
        let mut output = Vec::new();
        let mut overflow = false;
        let mut chunk = [0_u8; 1024];
        loop {
            let count = reader.read(&mut chunk)?;
            if count == 0 {
                break;
            }
            let retained = count.min(MAX_CHILD_OUTPUT.saturating_sub(output.len()));
            output.extend_from_slice(&chunk[..retained]);
            overflow |= retained != count;
        }
        Ok((output, overflow))
    }

    fn run_child_bounded(mut command: Command) -> (ExitStatus, Vec<u8>, Vec<u8>) {
        command.stdout(Stdio::piped()).stderr(Stdio::piped());
        let mut child = command.spawn().unwrap();
        let stdout = child.stdout.take().unwrap();
        let stderr = child.stderr.take().unwrap();
        let stdout_reader = std::thread::spawn(move || collect_bounded(stdout));
        let stderr_reader = std::thread::spawn(move || collect_bounded(stderr));
        let deadline = Instant::now() + Duration::from_secs(30);
        let status = loop {
            if let Some(status) = child.try_wait().unwrap() {
                break status;
            }
            if Instant::now() >= deadline {
                child.kill().unwrap();
                let _ = child.wait();
                let _ = stdout_reader.join();
                let _ = stderr_reader.join();
                panic!("reviewed-manifest child exceeded 30-second bound");
            }
            std::thread::sleep(Duration::from_millis(10));
        };
        let (stdout, stdout_overflow) = stdout_reader.join().unwrap().unwrap();
        let (stderr, stderr_overflow) = stderr_reader.join().unwrap().unwrap();
        assert!(!stdout_overflow, "child stdout exceeded byte bound");
        assert!(!stderr_overflow, "child stderr exceeded byte bound");
        (status, stdout, stderr)
    }

    #[test]
    fn reviewed_manifest_binds_complete_profile_with_unusable_path() {
        if std::env::var_os(BIND_CHILD).is_some() {
            assert_eq!(std::env::var_os(BIND_CHILD), Some(OsString::from("1")));
            assert_eq!(
                std::env::var_os("PATH"),
                Some(OsString::from(BIND_CHILD_AMBIENT_PATH))
            );
            let manifest = PathBuf::from(std::env::var_os(BIND_CHILD_MANIFEST).unwrap());
            assert!(manifest.is_absolute());
            let digest = std::env::var(BIND_CHILD_DIGEST).unwrap();
            // SAFETY: the parent invocation prepared the fixed synthetic ELF
            // graph and passed the digest of its exact canonical manifest.
            let profile =
                unsafe { LiteinstAfterLoaderProfile::review_dynamic_x86_64_et_exec_v2(&digest) }
                    .unwrap();
            let config = profile.bind_reviewed_manifest(&manifest).unwrap();
            assert_eq!(
                config.environment.get(&OsString::from("PATH")),
                Some(&OsString::from("/definitely/unusable/tool/path")),
            );
            assert_eq!(config.initial_dependencies.len(), 2);
            assert!(config.deferred_dependencies.is_empty());
            assert_eq!(config.immutable_bundle().iter().count(), 7);
            assert_eq!(
                config.loader_policy().loader_cache_path(),
                config
                    .immutable_bundle()
                    .artifact(&crate::after_loader::ImmutableAfterLoaderRole::LoaderCache)
                    .unwrap()
                    .logical_path()
            );
            assert_eq!(
                config.runtime_marker_path(),
                config
                    .immutable_bundle()
                    .artifact(&crate::after_loader::ImmutableAfterLoaderRole::RuntimeMarker)
                    .unwrap()
                    .logical_path()
            );
            assert!(
                std::fs::symlink_metadata(config.loader_policy().system_preload_path())
                    .is_err_and(|error| error.kind() == io::ErrorKind::NotFound)
            );
            assert!(
                config
                    .diagnostics()
                    .observations()
                    .iter()
                    .any(|observation| observation.operation == "reviewed manifest bound")
            );
            println!("{BIND_CHILD_OK}");
            return;
        }

        let parent_environment = [
            ("PATH", std::env::var_os("PATH")),
            (BIND_CHILD, std::env::var_os(BIND_CHILD)),
            (BIND_CHILD_MANIFEST, std::env::var_os(BIND_CHILD_MANIFEST)),
            (BIND_CHILD_DIGEST, std::env::var_os(BIND_CHILD_DIGEST)),
        ];
        let fixture = Fixture::new();
        let manifest = fixture.canonical_manifest();
        fixture.write_manifest(&manifest);
        let test_module = module_path!()
            .split_once("::")
            .map(|(_, module)| module)
            .unwrap_or(module_path!());
        let test_name =
            format!("{test_module}::reviewed_manifest_binds_complete_profile_with_unusable_path");
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .env_clear()
            .env("PATH", BIND_CHILD_AMBIENT_PATH)
            .env(BIND_CHILD, "1")
            .env(BIND_CHILD_MANIFEST, &fixture.manifest)
            .env(BIND_CHILD_DIGEST, digest_text(&manifest))
            .arg(&test_name)
            .arg("--exact")
            .arg("--nocapture")
            .arg("--quiet")
            .arg("--test-threads=1");
        let (status, stdout, stderr) = run_child_bounded(command);
        assert_eq!(
            status.code(),
            Some(0),
            "child failed: stdout={} stderr={}",
            String::from_utf8_lossy(&stdout),
            String::from_utf8_lossy(&stderr),
        );
        assert!(stderr.is_empty(), "child wrote unexpected stderr");
        assert_eq!(
            stdout
                .windows(BIND_CHILD_OK.len())
                .filter(|window| *window == BIND_CHILD_OK.as_bytes())
                .count(),
            1,
            "child did not emit exactly one success marker: {}",
            String::from_utf8_lossy(&stdout),
        );
        assert!(
            stdout.len() <= MAX_CHILD_OUTPUT,
            "child stdout exceeded retained bound"
        );
        for (key, value) in parent_environment {
            assert_eq!(
                std::env::var_os(key),
                value,
                "self-subprocess binding mutated parent environment key {key:?}"
            );
        }
    }

    #[test]
    fn reviewed_manifest_checks_approval_digest_before_schema_or_paths() {
        let fixture = Fixture::new();
        let approved = fixture.canonical_manifest();
        let profile = Fixture::approve(&approved);
        fixture.write_manifest("schema=1\nexecutable=/path/that/must/not/be-opened\n");
        let error = profile
            .bind_reviewed_manifest(&fixture.manifest)
            .unwrap_err();
        assert_eq!(
            error.to_string(),
            "reviewed after-loader manifest SHA-256 differs"
        );
        assert!(
            unsafe {
                LiteinstAfterLoaderProfile::review_dynamic_x86_64_et_exec_v2(
                    &digest_text(&approved).to_ascii_uppercase(),
                )
            }
            .is_err()
        );
    }

    #[test]
    fn reviewed_manifest_refuses_schema_profile_order_encoding_and_unknown_fields() {
        let fixture = Fixture::new();
        let manifest = fixture.canonical_manifest();
        let first_environment = manifest.find("environment=").unwrap();
        let first_image = manifest.find("image=").unwrap();
        let environment = &manifest[first_environment..first_image];
        let cache_line = manifest
            .lines()
            .find(|line| line.starts_with("loader_cache="))
            .unwrap();
        let preload_line = manifest
            .lines()
            .find(|line| line.starts_with("system_preload="))
            .unwrap();
        let mut reversed_environment = environment.lines().collect::<Vec<_>>();
        reversed_environment.reverse();
        let reversed_environment = format!("{}\n", reversed_environment.join("\n"));

        for invalid in [
            manifest.replacen("schema=3", "schema=2", 1),
            manifest.replacen(DYNAMIC_X86_64_ET_EXEC_V2, "DynamicX86_64EtDynV2", 1),
            manifest.replacen(cache_line, "loader_cache=ambient", 1),
            manifest.replacen(preload_line, "system_preload=unchecked", 1),
            manifest.replacen(LOADER_SEARCH_POLICY, "loader_search=ambient-defaults", 1),
            manifest.replacen("provider=", "unknown=field\nprovider=", 1),
            manifest.replacen(environment, &reversed_environment, 1),
            manifest.replacen("environment=4c", "environment=4C", 1),
            manifest.replacen("executable=2f", "executable=2F", 1),
            manifest.replacen("runtime=2f", "runtime=2", 1),
            manifest.trim_end().to_owned(),
            format!("{manifest}\n"),
        ] {
            assert!(
                fixture.bind_reapproved(&invalid).is_err(),
                "accepted noncanonical manifest: {invalid:?}"
            );
        }
    }

    #[test]
    fn schema3_requires_exact_cache_absent_system_preload_and_sealed_bundle_search() {
        let fixture = Fixture::new();
        let manifest = fixture.canonical_manifest();
        let cache_line = manifest
            .lines()
            .find(|line| line.starts_with("loader_cache="))
            .unwrap();
        let preload_line = manifest
            .lines()
            .find(|line| line.starts_with("system_preload="))
            .unwrap();
        let zeros = "0".repeat(64);
        for (label, invalid) in [
            (
                "unbound loader cache",
                manifest.replacen(cache_line, "loader_cache=ambient", 1),
            ),
            (
                "wrong loader cache digest",
                replace_line(
                    &manifest,
                    "loader_cache=",
                    &format!(
                        "loader_cache={}\t{zeros}",
                        encode_hex(fixture.cache.as_os_str().as_bytes())
                    ),
                ),
            ),
            (
                "system preload",
                manifest.replacen(
                    preload_line,
                    &preload_line.replacen("\tabsent", "\tpresent", 1),
                    1,
                ),
            ),
            (
                "ambient loader search",
                manifest.replacen(LOADER_SEARCH_POLICY, "loader_search=ambient-defaults", 1),
            ),
        ] {
            assert!(
                fixture.bind_reapproved(&invalid).is_err(),
                "accepted {label} policy"
            );
        }

        let alternate_preload = fixture.directory.join("alternate-system-preload");
        let changed_path = replace_line(
            &manifest,
            "system_preload=",
            &format!(
                "system_preload={}\tabsent",
                encode_hex(alternate_preload.as_os_str().as_bytes())
            ),
        );
        let profile = Fixture::approve(&manifest);
        fixture.write_manifest(&changed_path);
        assert_eq!(
            profile
                .bind_reviewed_manifest(&fixture.manifest)
                .unwrap_err()
                .to_string(),
            "reviewed after-loader manifest SHA-256 differs"
        );

        let alternate_cache = fixture.directory.join("alternate-loader-cache");
        std::fs::write(&alternate_cache, std::fs::read(&fixture.cache).unwrap()).unwrap();
        let changed_cache_path = replace_line(
            &manifest,
            "loader_cache=",
            &format!(
                "loader_cache={}\t{}",
                encode_hex(alternate_cache.as_os_str().as_bytes()),
                digest_file(&alternate_cache),
            ),
        );
        fixture.write_manifest(&changed_cache_path);
        assert_eq!(
            profile
                .bind_reviewed_manifest(&fixture.manifest)
                .unwrap_err()
                .to_string(),
            "reviewed after-loader manifest SHA-256 differs"
        );

        fixture.write_manifest(&manifest);
        std::fs::write(&fixture.preload, b"unexpected system preload\n").unwrap();
        assert!(
            Fixture::approve(&manifest)
                .bind_reviewed_manifest(&fixture.manifest)
                .is_err()
        );
    }

    #[test]
    fn reviewed_manifest_refuses_wrong_artifact_marker_runtime_image_and_pt_interp() {
        let fixture = Fixture::new();
        let manifest = fixture.canonical_manifest();
        let zeros = "0".repeat(64);
        let marker_digest_wrong = replace_line(
            &manifest,
            "runtime_marker=",
            &format!(
                "runtime_marker={}\t{zeros}",
                encode_hex(fixture.marker.as_os_str().as_bytes())
            ),
        );
        let runtime_digest_wrong = replace_line(
            &manifest,
            "runtime=",
            &format!(
                "runtime={}\t{zeros}",
                encode_hex(fixture.runtime.as_os_str().as_bytes())
            ),
        );
        let image_digest_wrong = replace_line(
            &manifest,
            "image=libunused.so\t",
            &format!(
                "image=libunused.so\t{}\t{zeros}",
                encode_hex(fixture.unused.as_os_str().as_bytes())
            ),
        );
        let pt_interp_wrong = replace_line(
            &manifest,
            "interpreter=",
            &format!(
                "interpreter=ld-review.so\t{}\t{}",
                encode_hex(fixture.provider.as_os_str().as_bytes()),
                digest_file(&fixture.provider),
            ),
        );
        let et_exec_wrong = replace_line(
            &manifest,
            "executable=",
            &format!(
                "executable={}\t{}",
                encode_hex(fixture.extra.as_os_str().as_bytes()),
                digest_file(&fixture.extra)
            ),
        );

        for invalid in [
            marker_digest_wrong,
            runtime_digest_wrong,
            image_digest_wrong,
            pt_interp_wrong,
            et_exec_wrong,
        ] {
            assert!(fixture.bind_reapproved(&invalid).is_err());
        }
    }

    #[test]
    fn schema3_binds_raw_non_utf8_paths_and_pt_interp_bytes() {
        let fixture = Fixture::new();
        let non_utf8_name = OsString::from_vec(b"ld-review-\xff.so".to_vec());
        let interpreter = fixture.directory.join("compat").join(non_utf8_name);
        let system_preload = fixture
            .directory
            .join(OsString::from_vec(b"ld-review-\xfe.preload".to_vec()));
        std::fs::write(&interpreter, std::fs::read(&fixture.interpreter).unwrap()).unwrap();
        set_executable(&interpreter);

        let mut executable = std::fs::read(&fixture.executable).unwrap();
        let mut payload = interpreter.as_os_str().as_bytes().to_vec();
        payload.push(0);
        set_pt_interp_payload(&mut executable, &payload);
        std::fs::write(&fixture.executable, &executable).unwrap();

        let manifest = fixture.canonical_manifest();
        let manifest = replace_line(
            &manifest,
            "interpreter=",
            &format!(
                "interpreter=ld-review.so\t{}\t{}",
                encode_hex(interpreter.as_os_str().as_bytes()),
                digest_file(&interpreter),
            ),
        );
        let manifest = replace_line(
            &manifest,
            "system_preload=",
            &format!(
                "system_preload={}\tabsent",
                encode_hex(system_preload.as_os_str().as_bytes())
            ),
        );
        let config = fixture.bind_reapproved(&manifest).unwrap();
        assert_eq!(
            config.interpreter.path.as_os_str().as_bytes(),
            interpreter.as_os_str().as_bytes()
        );
        assert!(require_exact_pt_interp(&executable, &interpreter).is_ok());
        let sealed = config
            .immutable_bundle()
            .artifact(&crate::after_loader::ImmutableAfterLoaderRole::Interpreter)
            .unwrap();
        assert_eq!(
            sealed.logical_path().as_os_str().as_bytes(),
            interpreter.as_os_str().as_bytes()
        );
        assert_eq!(
            config
                .loader_policy()
                .system_preload_path()
                .as_os_str()
                .as_bytes(),
            system_preload.as_os_str().as_bytes()
        );
    }

    #[test]
    fn schema3_refuses_interpreter_provider_role_aliases() {
        let fixture = Fixture::new();
        let manifest = fixture.canonical_manifest();
        let provider_line = manifest
            .lines()
            .find(|line| line.starts_with("provider="))
            .unwrap();
        let same_path = manifest.replacen(
            provider_line,
            &format!(
                "provider=libc.so.6\t{}\t{}",
                encode_hex(fixture.interpreter.as_os_str().as_bytes()),
                digest_file(&fixture.interpreter),
            ),
            1,
        );
        let same_soname = manifest.replacen("interpreter=ld-review.so", "interpreter=libc.so.6", 1);

        let hardlink = fixture.directory.join("provider-interpreter-hardlink.so");
        std::fs::hard_link(&fixture.interpreter, &hardlink).unwrap();
        let same_identity = manifest.replacen(
            provider_line,
            &format!(
                "provider=libc.so.6\t{}\t{}",
                encode_hex(hardlink.as_os_str().as_bytes()),
                digest_file(&hardlink),
            ),
            1,
        );
        assert!(fixture.bind_reapproved(&same_path).is_err());
        assert!(fixture.bind_reapproved(&same_soname).is_err());
        let error = fixture.bind_reapproved(&same_identity).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("aliases another manifest role by file identity"),
            "unexpected role-identity refusal: {error}"
        );
    }

    #[test]
    fn schema3_bundle_seals_every_role_and_survives_source_mutation() {
        let fixture = Fixture::new();
        let config = fixture
            .bind_reapproved(&fixture.canonical_manifest())
            .unwrap();
        let expected_roles = BTreeSet::from([
            crate::after_loader::ImmutableAfterLoaderRole::LoaderCache,
            crate::after_loader::ImmutableAfterLoaderRole::Executable,
            crate::after_loader::ImmutableAfterLoaderRole::Interpreter,
            crate::after_loader::ImmutableAfterLoaderRole::Provider,
            crate::after_loader::ImmutableAfterLoaderRole::InitialDependency(
                "libunused.so".to_owned(),
            ),
            crate::after_loader::ImmutableAfterLoaderRole::Runtime,
            crate::after_loader::ImmutableAfterLoaderRole::RuntimeMarker,
        ]);
        assert_eq!(
            config
                .immutable_bundle()
                .iter()
                .map(|(role, _)| role.clone())
                .collect::<BTreeSet<_>>(),
            expected_roles
        );
        for (role, artifact) in config.immutable_bundle().iter() {
            assert_eq!(artifact.role(), role);
            assert_eq!(
                unsafe { libc::fcntl(artifact.raw_fd(), libc::F_GET_SEALS) },
                crate::after_loader::IMMUTABLE_FILE_SEALS
            );
            let metadata = std::fs::metadata(artifact.sealed_source()).unwrap();
            assert_eq!(metadata.len(), artifact.bytes().len() as u64);
            assert_eq!(
                crate::after_loader::FileIdentity::from_metadata(&metadata),
                artifact.sealed_identity()
            );
            assert_eq!(
                std::fs::read(artifact.sealed_source()).unwrap(),
                artifact.bytes()
            );
            let digest: [u8; 32] = Sha256::digest(artifact.bytes()).into();
            assert_eq!(artifact.sha256(), digest);
            assert_ne!(artifact.source_identity(), artifact.sealed_identity());
        }

        let provider = config
            .immutable_bundle()
            .artifact(&crate::after_loader::ImmutableAfterLoaderRole::Provider)
            .unwrap()
            .clone();
        let retained = provider.bytes().to_vec();
        std::fs::write(&fixture.provider, b"mutated after schema-3 binding").unwrap();
        assert_eq!(provider.bytes(), retained.as_slice());
        assert_eq!(std::fs::read(provider.sealed_source()).unwrap(), retained);

        let cache = config
            .immutable_bundle()
            .artifact(&crate::after_loader::ImmutableAfterLoaderRole::LoaderCache)
            .unwrap()
            .clone();
        let retained_cache = cache.bytes().to_vec();
        std::fs::write(&fixture.cache, b"mutated cache after schema-3 binding").unwrap();
        assert_eq!(cache.bytes(), retained_cache.as_slice());
        assert_eq!(
            std::fs::read(cache.sealed_source()).unwrap(),
            retained_cache
        );
    }

    #[test]
    fn schema3_rejects_unreviewed_loader_selector_tags_in_every_role() {
        let fixture = Fixture::new();
        let original_provider = std::fs::read(&fixture.provider).unwrap();
        for (tag, value) in [
            (dynamic::DT_RPATH, 1),
            (dynamic::DT_RUNPATH, 1),
            (dynamic::DT_AUDIT, 1),
            (dynamic::DT_DEPAUDIT, 1),
            (dynamic::DT_CONFIG, 1),
            (dynamic::DT_GNU_LIBLIST, 1),
            (crate::after_loader::DT_GNU_PRELINKED, 1),
            (crate::after_loader::DT_GNU_CONFLICTSZ, 1),
            (crate::after_loader::DT_GNU_LIBLISTSZ, 1),
            (crate::after_loader::DT_FEATURE_1, 1),
            (crate::after_loader::DT_POSFLAG_1, 1),
            (crate::after_loader::DT_SYMINSZ, 1),
            (crate::after_loader::DT_SYMINENT, 1),
            (crate::after_loader::DT_GNU_CONFLICT, 1),
            (crate::after_loader::DT_SYMINFO, 1),
            (crate::after_loader::DT_AUXILIARY, 1),
            (crate::after_loader::DT_FILTER, 1),
            (dynamic::DT_SYMBOLIC, 0),
            (dynamic::DT_FLAGS, dynamic::DF_ORIGIN),
            (dynamic::DT_FLAGS_1, dynamic::DF_1_NODEFLIB),
            (dynamic::DT_FLAGS_1, dynamic::DF_1_PIE),
        ] {
            let mut selected = original_provider.clone();
            append_dynamic_tag(&mut selected, tag, value);
            let elf = Elf::parse(&selected).unwrap();
            assert!(
                crate::after_loader::reject_unreviewed_loader_selectors(&elf).is_err(),
                "accepted loader selector tag {tag:#x} value {value:#x}"
            );
            std::fs::write(&fixture.provider, &selected).unwrap();
            let manifest = replace_line(
                &fixture.canonical_manifest(),
                "provider=",
                &format!(
                    "provider=libc.so.6\t{}\t{}",
                    encode_hex(fixture.provider.as_os_str().as_bytes()),
                    digest_file(&fixture.provider),
                ),
            );
            assert!(fixture.bind_reapproved(&manifest).is_err());
            std::fs::write(&fixture.provider, &original_provider).unwrap();
        }

        for (path, prefix, role_prefix) in [
            (&fixture.executable, "executable=", "executable="),
            (
                &fixture.interpreter,
                "interpreter=",
                "interpreter=ld-review.so\t",
            ),
            (
                &fixture.unused,
                "image=libunused.so\t",
                "image=libunused.so\t",
            ),
        ] {
            let original = std::fs::read(path).unwrap();
            let mut selected = original.clone();
            append_dynamic_tag(&mut selected, dynamic::DT_RPATH, 1);
            std::fs::write(path, &selected).unwrap();
            let manifest = replace_line(
                &fixture.canonical_manifest(),
                prefix,
                &format!(
                    "{role_prefix}{}\t{}",
                    encode_hex(path.as_os_str().as_bytes()),
                    digest_file(path),
                ),
            );
            assert!(fixture.bind_reapproved(&manifest).is_err());
            std::fs::write(path, original).unwrap();
        }

        let mut runtime = std::fs::read(&fixture.runtime).unwrap();
        append_dynamic_tag(&mut runtime, dynamic::DT_RUNPATH, 1);
        assert!(LiteinstCallerImage::runtime_stage_marker(&runtime).is_err());
    }

    #[test]
    fn schema3_flags_keep_initial_noopen_but_refuse_every_dlopen_blocker() {
        let fixture = Fixture::new();
        let mut initial = std::fs::read(&fixture.provider).unwrap();
        append_dynamic_tag(&mut initial, dynamic::DT_FLAGS_1, dynamic::DF_1_NOOPEN);
        std::fs::write(&fixture.provider, initial).unwrap();
        let initial_config = fixture
            .bind_reapproved(&fixture.canonical_manifest())
            .expect("an already-loaded initial dependency may carry DF_1_NOOPEN");
        assert!(
            initial_config
                .initial_dependencies
                .iter()
                .all(|image| image.path != fixture.extra)
        );
        assert!(initial_config.deferred_dependencies.is_empty());

        for flag in [dynamic::DF_1_NOOPEN, dynamic::DF_1_PIE] {
            let runtime_fixture = Fixture::new();
            let mut runtime = std::fs::read(&runtime_fixture.runtime).unwrap();
            append_dynamic_tag(&mut runtime, dynamic::DT_FLAGS_1, flag);
            assert!(
                LiteinstCallerImage::runtime_stage_marker(&runtime).is_err(),
                "runtime stage marker accepted dlopen blocker {flag:#x}"
            );

            let deferred_fixture = Fixture::new();
            let mut runtime = std::fs::read(&deferred_fixture.runtime).unwrap();
            add_synthetic_runtime_needed(&mut runtime, "libzzextra.so");
            std::fs::write(&deferred_fixture.runtime, &runtime).unwrap();
            std::fs::write(
                &deferred_fixture.marker,
                LiteinstCallerImage::runtime_stage_marker(&runtime).unwrap(),
            )
            .unwrap();
            let mut deferred = std::fs::read(&deferred_fixture.extra).unwrap();
            append_dynamic_tag(&mut deferred, dynamic::DT_FLAGS_1, flag);
            std::fs::write(&deferred_fixture.extra, deferred).unwrap();
            let mut manifest = deferred_fixture.canonical_manifest();
            manifest.push_str(&format!(
                "image=libzzextra.so\t{}\t{}\n",
                encode_hex(deferred_fixture.extra.as_os_str().as_bytes()),
                digest_file(&deferred_fixture.extra),
            ));
            assert!(
                deferred_fixture.bind_reapproved(&manifest).is_err(),
                "deferred dependency accepted dlopen blocker {flag:#x}"
            );
        }

        for flag in [
            dynamic::DF_1_NOW,
            dynamic::DF_1_NODELETE,
            dynamic::DF_1_NODUMP,
        ] {
            let fixture = Fixture::new();
            let mut runtime = std::fs::read(&fixture.runtime).unwrap();
            add_synthetic_runtime_needed(&mut runtime, "libzzextra.so");
            append_dynamic_tag(&mut runtime, dynamic::DT_FLAGS_1, flag);
            std::fs::write(&fixture.runtime, &runtime).unwrap();
            std::fs::write(
                &fixture.marker,
                LiteinstCallerImage::runtime_stage_marker(&runtime).unwrap(),
            )
            .unwrap();
            let mut deferred = std::fs::read(&fixture.extra).unwrap();
            append_dynamic_tag(&mut deferred, dynamic::DT_FLAGS_1, flag);
            std::fs::write(&fixture.extra, deferred).unwrap();
            let mut manifest = fixture.canonical_manifest();
            manifest.push_str(&format!(
                "image=libzzextra.so\t{}\t{}\n",
                encode_hex(fixture.extra.as_os_str().as_bytes()),
                digest_file(&fixture.extra),
            ));
            let config = fixture
                .bind_reapproved(&manifest)
                .unwrap_or_else(|error| panic!("ordinary flag {flag:#x} was refused: {error}"));
            assert_eq!(config.deferred_dependencies.len(), 1);
            assert_eq!(config.deferred_dependencies[0].path, fixture.extra);
        }
    }

    #[test]
    fn reviewed_manifest_refuses_nonexact_pt_interp_payloads_and_text_aliases() {
        let fixture = Fixture::new();
        let manifest = fixture.canonical_manifest();
        let valid_executable = std::fs::read(&fixture.executable).unwrap();
        let expected = fixture.interpreter.as_os_str().as_bytes().to_vec();

        let mut embedded_nul = expected.clone();
        embedded_nul.insert(embedded_nul.len() / 2, 0);
        embedded_nul.push(0);
        let mut non_utf8 = expected.clone();
        *non_utf8.last_mut().unwrap() = 0xff;
        non_utf8.push(0);
        let mut extra_after_nul = expected.clone();
        extra_after_nul.extend_from_slice(b"\0ignored\0");
        let mut valid_payload = expected.clone();
        valid_payload.push(0);
        let mut repeated_separator =
            format!("{}/compat//ld-review.so", fixture.directory.display()).into_bytes();
        repeated_separator.push(0);
        let mut dot_component =
            format!("{}/compat/./ld-review.so", fixture.directory.display()).into_bytes();
        dot_component.push(0);
        let mut parent_component = format!(
            "{}/compat/../compat/ld-review.so",
            fixture.directory.display()
        )
        .into_bytes();
        parent_component.push(0);
        let mut trailing_separator =
            format!("{}/compat/ld-review.so/", fixture.directory.display()).into_bytes();
        trailing_separator.push(0);

        for (label, payload) in [
            ("shorter than two bytes", vec![0]),
            ("missing final NUL", expected.clone()),
            ("extra bytes after NUL", extra_after_nul),
            ("embedded NUL", embedded_nul),
            ("nonmatching non-UTF8 path", non_utf8),
            ("repeated path separator", repeated_separator),
            ("dot path component", dot_component),
            ("parent path component", parent_component),
            ("trailing path separator", trailing_separator),
        ] {
            let mut executable = valid_executable.clone();
            set_pt_interp_payload(&mut executable, &payload);
            assert!(
                require_exact_pt_interp(&executable, &fixture.interpreter).is_err(),
                "direct PT_INTERP checker accepted {label}"
            );
            std::fs::write(&fixture.executable, &executable).unwrap();
            let invalid = replace_line(
                &manifest,
                "executable=",
                &format!(
                    "executable={}\t{}",
                    encode_hex(fixture.executable.as_os_str().as_bytes()),
                    digest_file(&fixture.executable),
                ),
            );
            assert!(
                fixture.bind_reapproved(&invalid).is_err(),
                "binder accepted PT_INTERP with {label}"
            );
        }

        for (label, field) in [
            ("out-of-range p_offset", 8_usize),
            ("out-of-range p_filesz", 32_usize),
        ] {
            let mut executable = valid_executable.clone();
            let header = pt_interp_header(&executable);
            put64(&mut executable, header + field, u64::MAX);
            assert!(
                require_exact_pt_interp(&executable, &fixture.interpreter).is_err(),
                "direct PT_INTERP checker accepted {label}"
            );
            std::fs::write(&fixture.executable, &executable).unwrap();
            let invalid = replace_line(
                &manifest,
                "executable=",
                &format!(
                    "executable={}\t{}",
                    encode_hex(fixture.executable.as_os_str().as_bytes()),
                    digest_file(&fixture.executable),
                ),
            );
            assert!(
                fixture.bind_reapproved(&invalid).is_err(),
                "binder accepted PT_INTERP with {label}"
            );
        }

        let mut missing_interpreter = valid_executable.clone();
        let missing_header = pt_interp_header(&missing_interpreter);
        put32(&mut missing_interpreter, missing_header, ph::PT_NULL);
        let mut duplicate_interpreter = valid_executable.clone();
        let duplicate_header = pt_interp_header(&duplicate_interpreter);
        let program_header_offset =
            usize::try_from(elf_u64(&duplicate_interpreter, 32, "test phoff").unwrap()).unwrap();
        let program_header_size =
            usize::from(elf_u16(&duplicate_interpreter, 54, "test phentsize").unwrap());
        let program_header_count =
            usize::from(elf_u16(&duplicate_interpreter, 56, "test phnum").unwrap());
        let duplicate_at = program_header_offset + program_header_count * program_header_size;
        let duplicate_bytes = duplicate_interpreter
            [duplicate_header..duplicate_header + program_header_size]
            .to_vec();
        duplicate_interpreter[duplicate_at..duplicate_at + program_header_size]
            .copy_from_slice(&duplicate_bytes);
        put16(
            &mut duplicate_interpreter,
            56,
            u16::try_from(program_header_count + 1).unwrap(),
        );
        for (label, executable) in [
            ("missing sole segment", missing_interpreter),
            ("duplicate segment", duplicate_interpreter),
        ] {
            assert!(
                require_exact_pt_interp(&executable, &fixture.interpreter).is_err(),
                "direct PT_INTERP checker accepted {label}"
            );
            std::fs::write(&fixture.executable, &executable).unwrap();
            let invalid = replace_line(
                &manifest,
                "executable=",
                &format!(
                    "executable={}\t{}",
                    encode_hex(fixture.executable.as_os_str().as_bytes()),
                    digest_file(&fixture.executable),
                ),
            );
            assert!(
                fixture.bind_reapproved(&invalid).is_err(),
                "binder accepted PT_INTERP with {label}"
            );
        }

        std::fs::write(&fixture.executable, &valid_executable).unwrap();
        assert!(require_exact_pt_interp(&valid_executable, &fixture.interpreter).is_ok());
        assert_eq!(
            valid_payload.len(),
            fixture.interpreter.as_os_str().as_bytes().len() + 1
        );
        for (label, alias) in [
            (
                "repeated separator",
                format!("{}/compat//ld-review.so", fixture.directory.display()),
            ),
            (
                "dot component",
                format!("{}/compat/./ld-review.so", fixture.directory.display()),
            ),
            (
                "parent component",
                format!(
                    "{}/compat/../compat/ld-review.so",
                    fixture.directory.display()
                ),
            ),
            (
                "trailing separator",
                format!("{}/compat/ld-review.so/", fixture.directory.display()),
            ),
        ] {
            let invalid = replace_line(
                &manifest,
                "interpreter=",
                &format!(
                    "interpreter=ld-review.so\t{}\t{}",
                    encode_hex(alias.as_bytes()),
                    digest_file(&fixture.interpreter),
                ),
            );
            assert!(
                fixture.bind_reapproved(&invalid).is_err(),
                "binder accepted PT_INTERP {label} alias"
            );
        }
    }

    #[test]
    fn reviewed_manifest_refuses_noncanonical_artifact_path_spellings() {
        let fixture = Fixture::new();
        let manifest = fixture.canonical_manifest();
        let image_line = manifest
            .lines()
            .find(|line| line.starts_with("image=libunused.so\t"))
            .unwrap();
        let path_hex = |path: String| encode_hex(path.as_bytes());
        let repeated_cache_separator = replace_line(
            &manifest,
            "loader_cache=",
            &format!(
                "loader_cache={}\t{}",
                path_hex(format!("{}//ld-review.cache", fixture.directory.display())),
                digest_file(&fixture.cache),
            ),
        );
        let dot_preload_component = replace_line(
            &manifest,
            "system_preload=",
            &format!(
                "system_preload={}\tabsent",
                path_hex(format!(
                    "{}/./ld-review.preload",
                    fixture.directory.display()
                )),
            ),
        );
        let repeated_executable_separator = replace_line(
            &manifest,
            "executable=",
            &format!(
                "executable={}\t{}",
                path_hex(format!("{}//reviewed-et-exec", fixture.directory.display())),
                digest_file(&fixture.executable),
            ),
        );
        let dot_runtime_component = replace_line(
            &manifest,
            "runtime=",
            &format!(
                "runtime={}\t{}",
                path_hex(format!("{}/./runtime.so", fixture.directory.display())),
                digest_file(&fixture.runtime),
            ),
        );
        let parent_marker_component = replace_line(
            &manifest,
            "runtime_marker=",
            &format!(
                "runtime_marker={}\t{}",
                path_hex(format!(
                    "{}/compat/../runtime.marker",
                    fixture.directory.display()
                )),
                digest_file(&fixture.marker),
            ),
        );
        let dot_image_component = manifest.replacen(
            image_line,
            &format!(
                "image=libunused.so\t{}\t{}",
                path_hex(format!("{}/./libunused.so", fixture.directory.display())),
                digest_file(&fixture.unused),
            ),
            1,
        );
        let trailing_image_separator = manifest.replacen(
            image_line,
            &format!(
                "image=libunused.so\t{}\t{}",
                path_hex(format!("{}/libunused.so/", fixture.directory.display())),
                digest_file(&fixture.unused),
            ),
            1,
        );

        for (label, invalid) in [
            ("repeated cache separator", repeated_cache_separator),
            ("dot preload component", dot_preload_component),
            (
                "repeated executable separator",
                repeated_executable_separator,
            ),
            ("dot runtime component", dot_runtime_component),
            ("parent marker component", parent_marker_component),
            ("dot image component", dot_image_component),
            ("trailing image separator", trailing_image_separator),
        ] {
            assert!(
                fixture.bind_reapproved(&invalid).is_err(),
                "binder accepted {label}"
            );
        }
    }

    #[test]
    fn reviewed_manifest_refuses_provider_environment_graph_duplicates_and_aliases() {
        let fixture = Fixture::new();
        let manifest = fixture.canonical_manifest();
        let wrong_provider = manifest.replacen("provider=libc.so.6", "provider=ld-review.so", 1);
        let malformed_environment =
            manifest.replacen("environment=50415448\t", "environment=5041544\t", 1);
        let forbidden_loader_environment = manifest.replacen(
            "environment=4c495445494e53545f43414c4c45525f53454e54494e454c\t",
            &format!(
                "environment={}\t{}\nenvironment=4c495445494e53545f43414c4c45525f53454e54494e454c\t",
                encode_hex(b"LD_PRELOAD"),
                encode_hex(b"/tmp/forbidden.so"),
            ),
            1,
        );
        let provider_line = manifest
            .lines()
            .find(|line| line.starts_with("provider="))
            .unwrap();
        let cache_line = manifest
            .lines()
            .find(|line| line.starts_with("loader_cache="))
            .unwrap();
        let interpreter_line = manifest
            .lines()
            .find(|line| line.starts_with("interpreter="))
            .unwrap();
        let image_line = manifest
            .lines()
            .find(|line| line.starts_with("image=libunused.so\t"))
            .unwrap();
        let environment_line = manifest
            .lines()
            .find(|line| {
                line.starts_with("environment=4c495445494e53545f43414c4c45525f53454e54494e454c\t")
            })
            .unwrap();
        let duplicate_environment = manifest.replacen(
            environment_line,
            &format!("{environment_line}\n{environment_line}"),
            1,
        );
        let duplicate_image =
            manifest.replacen(image_line, &format!("{image_line}\n{image_line}"), 1);
        let aliased_path = manifest.replacen(
            image_line,
            &format!(
                "image=libunused.so\t{}\t{}",
                encode_hex(fixture.loader.as_os_str().as_bytes()),
                digest_file(&fixture.loader),
            ),
            1,
        );
        let aliased_role_path = manifest.replacen(
            provider_line,
            &format!(
                "provider=libc.so.6\t{}\t{}",
                encode_hex(fixture.interpreter.as_os_str().as_bytes()),
                digest_file(&fixture.interpreter),
            ),
            1,
        );
        let duplicate_role = manifest.replacen(
            interpreter_line,
            &format!("{interpreter_line}\n{interpreter_line}"),
            1,
        );
        let unreachable_image = format!(
            "{}image=libzzextra.so\t{}\t{}\n",
            manifest,
            encode_hex(fixture.extra.as_os_str().as_bytes()),
            digest_file(&fixture.extra),
        );
        let missing_dependency_node = manifest
            .lines()
            .filter(|line| !line.starts_with("image=libunused.so\t"))
            .fold(String::new(), |mut output, line| {
                output.push_str(line);
                output.push('\n');
                output
            });
        let missing_provider_node = manifest
            .lines()
            .filter(|line| !line.starts_with("provider="))
            .fold(String::new(), |mut output, line| {
                output.push_str(line);
                output.push('\n');
                output
            });
        let missing_interpreter_node = manifest
            .lines()
            .filter(|line| !line.starts_with("interpreter="))
            .fold(String::new(), |mut output, line| {
                output.push_str(line);
                output.push('\n');
                output
            });
        let hardlink = fixture.directory.join("libunused-hardlink.so");
        std::fs::hard_link(&fixture.provider, &hardlink).unwrap();
        let identity_alias = manifest.replacen(
            image_line,
            &format!(
                "image=libunused.so\t{}\t{}",
                encode_hex(hardlink.as_os_str().as_bytes()),
                digest_file(&hardlink),
            ),
            1,
        );
        let cache_hardlink = fixture.directory.join("loader-cache-provider-hardlink");
        std::fs::hard_link(&fixture.provider, &cache_hardlink).unwrap();
        let cache_identity_alias = manifest.replacen(
            cache_line,
            &format!(
                "loader_cache={}\t{}",
                encode_hex(cache_hardlink.as_os_str().as_bytes()),
                digest_file(&cache_hardlink),
            ),
            1,
        );

        for invalid in [
            wrong_provider,
            malformed_environment,
            forbidden_loader_environment,
            duplicate_environment,
            duplicate_image,
            aliased_path,
            aliased_role_path,
            duplicate_role,
            identity_alias,
            cache_identity_alias,
            unreachable_image,
            missing_dependency_node,
            missing_provider_node,
            missing_interpreter_node,
        ] {
            assert!(fixture.bind_reapproved(&invalid).is_err());
        }
    }
}
