//! Canonical schema-4 syntax for stable after-loader inputs.
//!
//! This module deliberately performs no filesystem access. It authenticates
//! the byte grammar and the complete structural plan before a later binder
//! opens a cover, store artifact, or logical target.

use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::ffi::OsString;
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::ffi::OsStringExt;
use std::path::Path;
use std::path::PathBuf;

use super::FileIdentity;
use super::stable_store::StableVerityExpectation;

const MAX_REVIEWED_MANIFEST: usize = 64 * 1024;
const MAX_COVERS: usize = 32;
const MAX_LOADER_IMAGES: usize = 32;
const MAX_ENVIRONMENT: usize = 256;
const MAX_ENVIRONMENT_BYTES: usize = 1024 * 1024;
const PROFILE: &str = "DynamicX86_64EtExecV3";
const STORE_POLICY: &str = "store_policy=fs-verity-sha256-local-ro-noatime-single-link-v1";
const COVER_POLICY: &str = "cover_policy=local-ro-noatime-broker-epoch-v2";
const XATTR_POLICY: &str = "xattr_policy=absent-v1";
const LOADER_SEARCH_POLICY: &str = "loader_search=mounted-bundle-only";
const PROVIDER_SONAME: &str = "libc.so.6";

/// Loader phase declared for one dependency artifact.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(super) enum DependencyPhase {
    /// The executable entry image already requires this dependency.
    Initial,
    /// Only the private runtime load may introduce this dependency.
    Deferred,
}

/// Exact semantic role of one schema-4 store artifact.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(super) enum ManifestRole {
    /// Dynamic-loader cache bytes.
    LoaderCache,
    /// Guest executable selected by `execve`.
    Executable,
    /// Exact ELF interpreter and its SONAME.
    Interpreter(String),
    /// Reviewed libc provider and its SONAME.
    Provider(String),
    /// One dependency assigned to an exact loader phase.
    Dependency {
        /// Manifest-declared loader phase.
        phase: DependencyPhase,
        /// Exact ELF `DT_SONAME`.
        soname: String,
    },
    /// Constructor-disabled LiteInst runtime.
    Runtime,
    /// Controller-only staging evidence for the runtime.
    RuntimeMarker,
}

impl ManifestRole {
    /// Whether the stable source must admit a private executable mapping.
    pub(super) const fn requires_executable_source(&self) -> bool {
        !matches!(self, Self::LoaderCache | Self::RuntimeMarker)
    }

    fn soname(&self) -> Option<&str> {
        match self {
            Self::Interpreter(soname)
            | Self::Provider(soname)
            | Self::Dependency { soname, .. } => Some(soname),
            Self::LoaderCache | Self::Executable | Self::Runtime | Self::RuntimeMarker => None,
        }
    }
}

/// Whether an artifact is mounted for the guest or retained only by the controller.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum ArtifactScope {
    /// The artifact must be mounted at this exact logical path.
    Guest(PathBuf),
    /// The artifact must never acquire a guest path, mount, or inherited fd.
    ControllerOnly,
}

impl ArtifactScope {
    /// Exact guest path, or `None` for a controller-only artifact.
    pub(super) fn guest_path(&self) -> Option<&Path> {
        match self {
            Self::Guest(path) => Some(path),
            Self::ControllerOnly => None,
        }
    }
}

/// One canonical stable-cover declaration.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct ManifestCover {
    logical_root: PathBuf,
    source_path: PathBuf,
    identity: FileIdentity,
    metadata_sha256: [u8; 32],
    tree_sha256: [u8; 32],
}

impl ManifestCover {
    /// Exact guest-visible root covered during the private loader epoch.
    pub(super) fn logical_root(&self) -> &Path {
        &self.logical_root
    }

    /// Exact persistent source directory.
    pub(super) fn source_path(&self) -> &Path {
        &self.source_path
    }

    /// Manifest-bound source directory identity.
    pub(super) const fn identity(&self) -> FileIdentity {
        self.identity
    }

    /// Digest of the complete stable root/filesystem metadata snapshot.
    pub(super) const fn metadata_sha256(&self) -> [u8; 32] {
        self.metadata_sha256
    }

    /// Digest of the complete stable cover inventory.
    pub(super) const fn tree_sha256(&self) -> [u8; 32] {
        self.tree_sha256
    }
}

/// One canonical stable-store artifact declaration.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct ManifestArtifact {
    role: ManifestRole,
    scope: ArtifactScope,
    store_path: PathBuf,
    expected: StableVerityExpectation,
}

impl ManifestArtifact {
    /// Exact semantic role.
    pub(super) fn role(&self) -> &ManifestRole {
        &self.role
    }

    /// Guest or controller-only scope.
    pub(super) fn scope(&self) -> &ArtifactScope {
        &self.scope
    }

    /// Exact persistent fs-verity source path.
    pub(super) fn store_path(&self) -> &Path {
        &self.store_path
    }

    /// Complete manifest-bound source expectation.
    pub(super) const fn expectation(&self) -> StableVerityExpectation {
        self.expected
    }
}

/// A fully parsed schema-4 plan that has not yet consulted the filesystem.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct ReviewedManifestV4 {
    system_preload: PathBuf,
    covers: Vec<ManifestCover>,
    artifacts: Vec<ManifestArtifact>,
    environment: BTreeMap<OsString, OsString>,
}

impl ReviewedManifestV4 {
    /// Parse and structurally validate exact canonical schema-4 bytes.
    pub(super) fn parse(bytes: &[u8]) -> io::Result<Self> {
        if bytes.is_empty() || bytes.len() > MAX_REVIEWED_MANIFEST {
            return Err(invalid("schema-4 manifest is empty or oversized"));
        }
        let text = std::str::from_utf8(bytes)
            .map_err(|_| invalid("schema-4 manifest is not canonical UTF-8"))?;
        let body = text
            .strip_suffix('\n')
            .ok_or_else(|| invalid("schema-4 manifest lacks its final newline"))?;
        if body.is_empty() || body.contains(['\r', '\0']) || body.ends_with('\n') {
            return Err(invalid("schema-4 manifest has noncanonical control bytes"));
        }

        let mut lines = body.split('\n').peekable();
        require_line(&mut lines, "schema=4", "schema")?;
        require_line(&mut lines, &format!("profile={PROFILE}"), "profile")?;
        require_line(&mut lines, STORE_POLICY, "store policy")?;
        require_line(&mut lines, COVER_POLICY, "cover policy")?;
        require_line(&mut lines, XATTR_POLICY, "xattr policy")?;
        require_line(&mut lines, LOADER_SEARCH_POLICY, "loader search policy")?;

        let preload = required_value(&mut lines, "system_preload=", "system preload")?;
        let [preload_path, preload_state] = exact_fields::<2>(preload, "system preload")?;
        if preload_state != "absent" {
            return Err(invalid("system preload state is not exactly absent"));
        }
        let system_preload = lexical_absolute_path(preload_path, "system preload")?;

        let mut covers = Vec::new();
        while lines.peek().is_some_and(|line| line.starts_with("cover=")) {
            let line = lines.next().unwrap();
            covers.push(parse_cover(line.strip_prefix("cover=").unwrap(), "cover")?);
        }
        if covers.is_empty() || covers.len() > MAX_COVERS {
            return Err(invalid("cover count is outside the schema-4 bound"));
        }

        let mut artifacts = Vec::new();
        while lines
            .peek()
            .is_some_and(|line| line.starts_with("artifact="))
        {
            let line = lines.next().unwrap();
            artifacts.push(parse_artifact(
                line.strip_prefix("artifact=").unwrap(),
                "artifact",
            )?);
        }

        let mut environment = BTreeMap::new();
        let mut previous_key = None::<Vec<u8>>;
        for line in lines {
            let value = line
                .strip_prefix("environment=")
                .ok_or_else(|| invalid("schema-4 manifest has an unknown or misplaced field"))?;
            let [key, value] = exact_fields::<2>(value, "environment")?;
            let key = decode_hex(key, false, "environment key")?;
            let value = decode_hex(value, true, "environment value")?;
            if !valid_environment_key(&key) {
                return Err(invalid("environment key is outside the reviewed policy"));
            }
            if value.contains(&0) {
                return Err(invalid("environment value contains a NUL byte"));
            }
            if previous_key
                .as_ref()
                .is_some_and(|previous| previous.as_slice() >= key.as_slice())
            {
                return Err(invalid("environment is duplicated or not strictly sorted"));
            }
            previous_key = Some(key.clone());
            if environment
                .insert(OsString::from_vec(key), OsString::from_vec(value))
                .is_some()
            {
                return Err(invalid("environment key is duplicated"));
            }
        }
        if environment.len() > MAX_ENVIRONMENT
            || environment
                .iter()
                .map(|(key, value)| key.len() + value.len() + 2)
                .sum::<usize>()
                > MAX_ENVIRONMENT_BYTES
        {
            return Err(invalid("environment is outside the reviewed bound"));
        }

        validate_structure(&system_preload, &covers, &artifacts)?;
        Ok(Self {
            system_preload,
            covers,
            artifacts,
            environment,
        })
    }

    /// Exact path that must remain absent in the mounted view.
    pub(super) fn system_preload(&self) -> &Path {
        &self.system_preload
    }

    /// Canonically ordered cover declarations.
    pub(super) fn covers(&self) -> &[ManifestCover] {
        &self.covers
    }

    /// Canonically ordered artifact declarations.
    pub(super) fn artifacts(&self) -> &[ManifestArtifact] {
        &self.artifacts
    }

    /// Exact reviewed guest environment.
    pub(super) fn environment(&self) -> &BTreeMap<OsString, OsString> {
        &self.environment
    }
}

fn parse_cover(value: &str, label: &str) -> io::Result<ManifestCover> {
    let [logical_root, source_path, device, inode, metadata, tree] =
        exact_fields::<6>(value, label)?;
    let logical_root = lexical_absolute_path(logical_root, "cover logical root")?;
    let source_path = lexical_absolute_path(source_path, "cover source path")?;
    if is_root(&logical_root) || is_root(&source_path) {
        return Err(invalid("cover root paths may not be filesystem root"));
    }
    Ok(ManifestCover {
        logical_root,
        source_path,
        identity: parse_identity(device, inode, "cover identity")?,
        metadata_sha256: parse_sha256(metadata, "cover metadata SHA-256")?,
        tree_sha256: parse_sha256(tree, "cover tree SHA-256")?,
    })
}

fn parse_artifact(value: &str, label: &str) -> io::Result<ManifestArtifact> {
    let [
        role,
        scope,
        loader_name,
        logical_path,
        store_path,
        content,
        verity,
        device,
        inode,
        metadata,
    ] = exact_fields::<10>(value, label)?;
    let role = match role {
        "loader_cache" => fixed_role(loader_name, ManifestRole::LoaderCache, "loader cache")?,
        "executable" => fixed_role(loader_name, ManifestRole::Executable, "executable")?,
        "interpreter" => ManifestRole::Interpreter(parse_loader_name(loader_name, "interpreter")?),
        "provider" => {
            let name = parse_loader_name(loader_name, "provider")?;
            if name != PROVIDER_SONAME {
                return Err(invalid("provider SONAME differs from reviewed profile"));
            }
            ManifestRole::Provider(name)
        }
        "initial_dependency" => ManifestRole::Dependency {
            phase: DependencyPhase::Initial,
            soname: parse_loader_name(loader_name, "initial dependency")?,
        },
        "deferred_dependency" => ManifestRole::Dependency {
            phase: DependencyPhase::Deferred,
            soname: parse_loader_name(loader_name, "deferred dependency")?,
        },
        "runtime" => fixed_role(loader_name, ManifestRole::Runtime, "runtime")?,
        "runtime_marker" => fixed_role(loader_name, ManifestRole::RuntimeMarker, "runtime marker")?,
        _ => return Err(invalid("artifact role is unknown")),
    };

    let scope = match (&role, scope, logical_path) {
        (ManifestRole::RuntimeMarker, "controller_only", "-") => ArtifactScope::ControllerOnly,
        (ManifestRole::RuntimeMarker, _, _) => {
            return Err(invalid(
                "runtime marker must be controller-only and pathless",
            ));
        }
        (_, "guest", path) if path != "-" => {
            let path = lexical_absolute_path(path, "artifact logical path")?;
            if is_root(&path) {
                return Err(invalid("artifact logical path may not be filesystem root"));
            }
            ArtifactScope::Guest(path)
        }
        _ => return Err(invalid("guest artifact has wrong scope or logical path")),
    };
    let store_path = lexical_absolute_path(store_path, "artifact store path")?;
    if is_root(&store_path) {
        return Err(invalid("artifact store path may not be filesystem root"));
    }
    Ok(ManifestArtifact {
        role,
        scope,
        store_path,
        expected: StableVerityExpectation::new(
            parse_sha256(content, "artifact content SHA-256")?,
            parse_sha256(verity, "artifact verity SHA-256")?,
            parse_identity(device, inode, "artifact identity")?,
            parse_sha256(metadata, "artifact metadata SHA-256")?,
        ),
    })
}

fn fixed_role(loader_name: &str, role: ManifestRole, label: &str) -> io::Result<ManifestRole> {
    if loader_name != "-" {
        return Err(invalid(format!("{label} must not declare a loader name")));
    }
    Ok(role)
}

fn parse_loader_name(value: &str, label: &str) -> io::Result<String> {
    if value.is_empty()
        || value == "-"
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._+-".contains(&byte))
    {
        return Err(invalid(format!("{label} loader name is malformed")));
    }
    Ok(value.to_owned())
}

fn validate_structure(
    system_preload: &Path,
    covers: &[ManifestCover],
    artifacts: &[ManifestArtifact],
) -> io::Result<()> {
    let mut previous_root = None::<&[u8]>;
    let mut logical_roots = BTreeSet::new();
    let mut source_paths = BTreeSet::new();
    let mut source_identities = BTreeSet::new();
    for cover in covers {
        let root = cover.logical_root.as_os_str().as_bytes();
        if previous_root.is_some_and(|previous| previous >= root) {
            return Err(invalid("cover roots are duplicated or not strictly sorted"));
        }
        previous_root = Some(root);
        if !logical_roots.insert(cover.logical_root.clone())
            || !source_paths.insert(cover.source_path.clone())
            || !source_identities.insert(cover.identity)
        {
            return Err(invalid("cover root, source path, or identity is aliased"));
        }
    }
    for (index, left) in covers.iter().enumerate() {
        for right in &covers[index + 1..] {
            if paths_overlap(&left.logical_root, &right.logical_root) {
                return Err(invalid("cover logical roots overlap"));
            }
        }
    }

    validate_artifact_order(artifacts)?;
    let mut guest_paths = BTreeSet::new();
    let mut store_paths = BTreeSet::new();
    let mut roles = BTreeSet::new();
    let mut sonames = BTreeSet::new();
    for artifact in artifacts {
        if !roles.insert(artifact.role.clone())
            || !store_paths.insert(artifact.store_path.clone())
            || !source_paths.insert(artifact.store_path.clone())
            || !source_identities.insert(artifact.expected.identity())
        {
            return Err(invalid(
                "artifact role, store path, or source identity is aliased",
            ));
        }
        if let Some(soname) = artifact.role.soname()
            && !sonames.insert(soname.to_owned())
        {
            return Err(invalid("artifact SONAME is aliased across roles or phases"));
        }
        if let Some(path) = artifact.scope.guest_path() {
            if !guest_paths.insert(path.to_path_buf()) {
                return Err(invalid("guest artifact logical path is aliased"));
            }
            if cover_count(path, covers) != 1 {
                return Err(invalid(
                    "guest artifact is not a strict descendant of exactly one cover",
                ));
            }
            if paths_overlap(path, system_preload) {
                return Err(invalid(
                    "system preload path overlaps a guest artifact path",
                ));
            }
        }
    }
    for (index, left) in guest_paths.iter().enumerate() {
        for right in guest_paths.iter().skip(index + 1) {
            if paths_overlap(left, right) {
                return Err(invalid("guest artifact logical paths overlap"));
            }
        }
    }
    if cover_count(system_preload, covers) != 1 {
        return Err(invalid(
            "system preload is not a strict descendant of exactly one cover",
        ));
    }
    for source in &source_paths {
        if covers
            .iter()
            .any(|cover| paths_overlap(source, &cover.logical_root))
        {
            return Err(invalid("source namespace overlaps a guest logical cover"));
        }
    }
    for (index, left) in source_paths.iter().enumerate() {
        for right in source_paths.iter().skip(index + 1) {
            if paths_overlap(left, right) {
                return Err(invalid("stable source paths overlap"));
            }
        }
    }
    Ok(())
}

fn validate_artifact_order(artifacts: &[ManifestArtifact]) -> io::Result<()> {
    if artifacts.len() < 6 {
        return Err(invalid("schema-4 artifact list lacks fixed roles"));
    }
    if !matches!(&artifacts[0].role, ManifestRole::LoaderCache)
        || !matches!(&artifacts[1].role, ManifestRole::Executable)
        || !matches!(&artifacts[2].role, ManifestRole::Interpreter(_))
        || !matches!(&artifacts[3].role, ManifestRole::Provider(_))
    {
        return Err(invalid(
            "schema-4 fixed artifact roles are missing or reordered",
        ));
    }
    let mut index = 4;
    let mut initial_previous = None::<&str>;
    while let Some(ManifestArtifact {
        role:
            ManifestRole::Dependency {
                phase: DependencyPhase::Initial,
                soname,
            },
        ..
    }) = artifacts.get(index)
    {
        if initial_previous.is_some_and(|previous| previous >= soname.as_str()) {
            return Err(invalid("initial dependencies are not strictly sorted"));
        }
        initial_previous = Some(soname);
        index += 1;
    }
    let mut deferred_previous = None::<&str>;
    while let Some(ManifestArtifact {
        role:
            ManifestRole::Dependency {
                phase: DependencyPhase::Deferred,
                soname,
            },
        ..
    }) = artifacts.get(index)
    {
        if deferred_previous.is_some_and(|previous| previous >= soname.as_str()) {
            return Err(invalid("deferred dependencies are not strictly sorted"));
        }
        deferred_previous = Some(soname);
        index += 1;
    }
    if !artifacts
        .get(index)
        .is_some_and(|artifact| matches!(&artifact.role, ManifestRole::Runtime))
        || !artifacts
            .get(index + 1)
            .is_some_and(|artifact| matches!(&artifact.role, ManifestRole::RuntimeMarker))
        || index + 2 != artifacts.len()
    {
        return Err(invalid(
            "dependency phases or terminal artifact roles are missing or reordered",
        ));
    }
    let loader_images = artifacts
        .iter()
        .filter(|artifact| artifact.role.soname().is_some())
        .count();
    if loader_images > MAX_LOADER_IMAGES {
        return Err(invalid("loader image count exceeds the reviewed bound"));
    }
    Ok(())
}

fn cover_count(path: &Path, covers: &[ManifestCover]) -> usize {
    covers
        .iter()
        .filter(|cover| strict_descendant(path, &cover.logical_root))
        .count()
}

fn strict_descendant(path: &Path, root: &Path) -> bool {
    path != root && path.starts_with(root)
}

fn paths_overlap(left: &Path, right: &Path) -> bool {
    left.starts_with(right) || right.starts_with(left)
}

fn is_root(path: &Path) -> bool {
    path.as_os_str().as_bytes() == b"/"
}

fn lexical_absolute_path(value: &str, label: &str) -> io::Result<PathBuf> {
    let bytes = decode_hex(value, false, &format!("{label} path"))?;
    if bytes.first() != Some(&b'/')
        || bytes.contains(&0)
        || bytes.len() > 1 && bytes.ends_with(b"/")
        || bytes.windows(2).any(|pair| pair == b"//")
        || bytes
            .split(|byte| *byte == b'/')
            .skip(1)
            .any(|component| component.is_empty() || component == b"." || component == b"..")
    {
        return Err(invalid(format!(
            "{label} path is not lexically canonical absolute"
        )));
    }
    Ok(PathBuf::from(OsString::from_vec(bytes)))
}

fn parse_identity(device: &str, inode: &str, label: &str) -> io::Result<FileIdentity> {
    Ok(FileIdentity {
        device: parse_hex_u64(device, &format!("{label} device"))?,
        inode: parse_hex_u64(inode, &format!("{label} inode"))?,
    })
}

fn parse_hex_u64(value: &str, label: &str) -> io::Result<u64> {
    if value.len() != 16 || !canonical_hex(value) {
        return Err(invalid(format!(
            "{label} is not exactly 16 lowercase hexadecimal digits"
        )));
    }
    u64::from_str_radix(value, 16).map_err(|_| invalid(format!("{label} is not a u64")))
}

fn parse_sha256(value: &str, label: &str) -> io::Result<[u8; 32]> {
    decode_hex(value, false, label)?
        .try_into()
        .map_err(|_| invalid(format!("{label} is not exactly 32 bytes")))
}

fn decode_hex(value: &str, allow_empty: bool, label: &str) -> io::Result<Vec<u8>> {
    if (!allow_empty && value.is_empty()) || !value.len().is_multiple_of(2) || !canonical_hex(value)
    {
        return Err(invalid(format!(
            "{label} is not canonical lowercase hexadecimal"
        )));
    }
    value
        .as_bytes()
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| Ok((hex_nibble(pair[0]) << 4) | hex_nibble(pair[1])))
        .collect()
}

fn canonical_hex(value: &str) -> bool {
    value
        .bytes()
        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn hex_nibble(byte: u8) -> u8 {
    match byte {
        b'0'..=b'9' => byte - b'0',
        b'a'..=b'f' => byte - b'a' + 10,
        _ => unreachable!("canonical_hex validated every nibble"),
    }
}

fn valid_environment_key(key: &[u8]) -> bool {
    !key.is_empty()
        && !key.contains(&0)
        && !key.contains(&b'=')
        && !key.starts_with(b"LD_")
        && key != b"GLIBC_TUNABLES"
        && !key.starts_with(b"MALLOC_")
        && key != b"GCONV_PATH"
        && key != b"LOCPATH"
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
        .ok_or_else(|| invalid(format!("{label} is missing or misplaced")))
}

fn require_line<'a>(
    lines: &mut impl Iterator<Item = &'a str>,
    expected: &str,
    label: &str,
) -> io::Result<()> {
    if lines.next() != Some(expected) {
        return Err(invalid(format!("unsupported or misplaced {label}")));
    }
    Ok(())
}

fn exact_fields<'a, const N: usize>(value: &'a str, label: &str) -> io::Result<[&'a str; N]> {
    value
        .split('\t')
        .collect::<Vec<_>>()
        .try_into()
        .map_err(|_| invalid(format!("{label} has a noncanonical field count")))
}

fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(value: &[u8]) -> String {
        const DIGITS: &[u8; 16] = b"0123456789abcdef";
        let mut output = String::with_capacity(value.len() * 2);
        for byte in value {
            output.push(DIGITS[usize::from(byte >> 4)] as char);
            output.push(DIGITS[usize::from(byte & 0x0f)] as char);
        }
        output
    }

    fn digest(byte: u8) -> String {
        hex(&[byte; 32])
    }

    fn artifact(
        role: &str,
        scope: &str,
        name: &str,
        logical: Option<&str>,
        store: &str,
        identity: u64,
    ) -> String {
        format!(
            "artifact={role}\t{scope}\t{name}\t{}\t{}\t{}\t{}\t{:016x}\t{:016x}\t{}\n",
            logical.map_or_else(|| "-".to_owned(), |path| hex(path.as_bytes())),
            hex(store.as_bytes()),
            digest(identity as u8),
            digest(identity.wrapping_add(1) as u8),
            2_u64,
            identity,
            digest(identity.wrapping_add(2) as u8),
        )
    }

    fn valid_manifest() -> String {
        let mut text = format!(
            "schema=4\nprofile={PROFILE}\n{STORE_POLICY}\n{COVER_POLICY}\n{XATTR_POLICY}\n{LOADER_SEARCH_POLICY}\nsystem_preload={}\tabsent\ncover={}\t{}\t{:016x}\t{:016x}\t{}\t{}\n",
            hex(b"/guest/etc/ld.so.preload"),
            hex(b"/guest"),
            hex(b"/store/covers/guest"),
            1_u64,
            1_u64,
            digest(1),
            digest(2),
        );
        text.push_str(&artifact(
            "loader_cache",
            "guest",
            "-",
            Some("/guest/etc/ld.so.cache"),
            "/store/artifacts/cache",
            10,
        ));
        text.push_str(&artifact(
            "executable",
            "guest",
            "-",
            Some("/guest/bin/app"),
            "/store/artifacts/app",
            11,
        ));
        text.push_str(&artifact(
            "interpreter",
            "guest",
            "ld-review.so",
            Some("/guest/lib/ld-review.so"),
            "/store/artifacts/loader",
            12,
        ));
        text.push_str(&artifact(
            "provider",
            "guest",
            "libc.so.6",
            Some("/guest/lib/libc.so.6"),
            "/store/artifacts/libc",
            13,
        ));
        text.push_str(&artifact(
            "initial_dependency",
            "guest",
            "libalpha.so",
            Some("/guest/lib/libalpha.so"),
            "/store/artifacts/libalpha",
            14,
        ));
        text.push_str(&artifact(
            "deferred_dependency",
            "guest",
            "libomega.so",
            Some("/guest/lib/libomega.so"),
            "/store/artifacts/libomega",
            15,
        ));
        text.push_str(&artifact(
            "runtime",
            "guest",
            "-",
            Some("/guest/lib/libreverie_liteinst.so"),
            "/store/artifacts/runtime",
            16,
        ));
        text.push_str(&artifact(
            "runtime_marker",
            "controller_only",
            "-",
            None,
            "/store/artifacts/runtime.marker",
            17,
        ));
        text.push_str("environment=41\t\n");
        text
    }

    fn swap_lines(text: &str, left: &str, right: &str) -> String {
        let mut lines = text.lines().map(str::to_owned).collect::<Vec<_>>();
        let left = lines
            .iter()
            .position(|line| line.starts_with(left))
            .unwrap();
        let right = lines
            .iter()
            .position(|line| line.starts_with(right))
            .unwrap();
        lines.swap(left, right);
        format!("{}\n", lines.join("\n"))
    }

    #[test]
    fn canonical_schema4_parses_without_filesystem_access() {
        let manifest = ReviewedManifestV4::parse(valid_manifest().as_bytes()).unwrap();
        assert_eq!(manifest.covers().len(), 1);
        assert_eq!(manifest.artifacts().len(), 8);
        assert_eq!(manifest.environment().len(), 1);
        assert_eq!(
            manifest.system_preload().as_os_str().as_bytes(),
            b"/guest/etc/ld.so.preload"
        );
        assert!(matches!(
            manifest.artifacts().last().unwrap().scope(),
            ArtifactScope::ControllerOnly
        ));

        let cover = &manifest.covers()[0];
        assert_eq!(cover.logical_root(), Path::new("/guest"));
        assert_eq!(cover.source_path(), Path::new("/store/covers/guest"));
        assert_eq!(
            cover.identity(),
            FileIdentity {
                device: 1,
                inode: 1
            }
        );
        assert_eq!(cover.metadata_sha256(), [1; 32]);
        assert_eq!(cover.tree_sha256(), [2; 32]);

        let stores = [
            "/store/artifacts/cache",
            "/store/artifacts/app",
            "/store/artifacts/loader",
            "/store/artifacts/libc",
            "/store/artifacts/libalpha",
            "/store/artifacts/libomega",
            "/store/artifacts/runtime",
            "/store/artifacts/runtime.marker",
        ];
        let logical = [
            Some("/guest/etc/ld.so.cache"),
            Some("/guest/bin/app"),
            Some("/guest/lib/ld-review.so"),
            Some("/guest/lib/libc.so.6"),
            Some("/guest/lib/libalpha.so"),
            Some("/guest/lib/libomega.so"),
            Some("/guest/lib/libreverie_liteinst.so"),
            None,
        ];
        for (offset, artifact) in manifest.artifacts().iter().enumerate() {
            let identity = 10 + offset as u64;
            assert_eq!(artifact.store_path(), Path::new(stores[offset]));
            assert_eq!(
                artifact.scope().guest_path(),
                logical[offset].map(Path::new)
            );
            assert_eq!(
                artifact.expectation().identity(),
                FileIdentity {
                    device: 2,
                    inode: identity,
                }
            );
            assert_eq!(
                artifact.expectation().content_sha256(),
                [identity as u8; 32]
            );
            assert_eq!(
                artifact.expectation().verity_sha256(),
                [identity.wrapping_add(1) as u8; 32]
            );
            assert_eq!(
                artifact.expectation().metadata_sha256(),
                [identity.wrapping_add(2) as u8; 32]
            );
            assert_eq!(
                artifact.role().requires_executable_source(),
                !matches!(
                    artifact.role(),
                    ManifestRole::LoaderCache | ManifestRole::RuntimeMarker
                )
            );
        }
    }

    #[test]
    fn schema_profile_and_policy_tokens_are_exact() {
        let valid = valid_manifest();
        for changed in [
            valid.replacen("schema=4", "schema=3", 1),
            valid.replacen(PROFILE, "DynamicX86_64EtExecV2", 1),
            valid.replacen("single-link-v1", "single-link-v0", 1),
            valid.replacen("broker-epoch-v2", "broker-epoch-v1", 1),
            valid.replacen("xattr_policy=absent-v1", "xattr_policy=ignored", 1),
            valid.replacen("mounted-bundle-only", "sealed-bundle-only", 1),
        ] {
            assert!(ReviewedManifestV4::parse(changed.as_bytes()).is_err());
        }
    }

    #[test]
    fn fixed_roles_and_dependency_phases_cannot_be_reordered() {
        let valid = valid_manifest();
        for changed in [
            swap_lines(&valid, "artifact=loader_cache", "artifact=executable"),
            swap_lines(&valid, "artifact=interpreter", "artifact=provider"),
            swap_lines(
                &valid,
                "artifact=initial_dependency",
                "artifact=deferred_dependency",
            ),
            swap_lines(&valid, "artifact=runtime", "artifact=runtime_marker"),
        ] {
            assert!(ReviewedManifestV4::parse(changed.as_bytes()).is_err());
        }
    }

    #[test]
    fn runtime_marker_is_controller_only_pathless_and_nameless() {
        let valid = valid_manifest();
        for changed in [
            valid.replacen(
                "artifact=runtime_marker\tcontroller_only\t-\t-",
                &format!(
                    "artifact=runtime_marker\tguest\t-\t{}",
                    hex(b"/guest/runtime.marker")
                ),
                1,
            ),
            valid.replacen(
                "artifact=runtime_marker\tcontroller_only\t-\t-",
                "artifact=runtime_marker\tcontroller_only\tmarker.so\t-",
                1,
            ),
        ] {
            assert!(ReviewedManifestV4::parse(changed.as_bytes()).is_err());
        }
    }

    #[test]
    fn lexical_paths_hex_and_fixed_width_numbers_are_exact() {
        let valid = valid_manifest();
        let guest = hex(b"/guest");
        for changed in [
            valid.replacen(&guest, &hex(b"/guest/../guest"), 1),
            valid.replacen(&digest(10), &digest(10).to_uppercase(), 1),
            valid.replacen("0000000000000001", "1", 1),
            valid.trim_end().to_owned(),
            format!("{valid}\n"),
        ] {
            assert!(ReviewedManifestV4::parse(changed.as_bytes()).is_err());
        }
    }

    #[test]
    fn covers_must_be_sorted_unique_and_component_disjoint() {
        let valid = valid_manifest();
        let nested = format!(
            "cover={}\t{}\t{:016x}\t{:016x}\t{}\t{}\n",
            hex(b"/guest/lib"),
            hex(b"/store/covers/lib"),
            3_u64,
            3_u64,
            digest(3),
            digest(4),
        );
        let changed = valid.replacen("artifact=", &format!("{nested}artifact="), 1);
        assert!(ReviewedManifestV4::parse(changed.as_bytes()).is_err());

        let source_overlap = valid.replacen(
            &hex(b"/store/covers/guest"),
            &hex(b"/guest/cover-source"),
            1,
        );
        assert!(ReviewedManifestV4::parse(source_overlap.as_bytes()).is_err());

        let nested_marker_source = valid.replacen(
            &hex(b"/store/artifacts/runtime.marker"),
            &hex(b"/store/covers/guest/private/runtime.marker"),
            1,
        );
        assert!(ReviewedManifestV4::parse(nested_marker_source.as_bytes()).is_err());
    }

    #[test]
    fn every_guest_artifact_and_absent_preload_belongs_to_one_cover() {
        let valid = valid_manifest();
        let outside = valid.replacen(
            &hex(b"/guest/lib/libreverie_liteinst.so"),
            &hex(b"/outside/runtime.so"),
            1,
        );
        assert!(ReviewedManifestV4::parse(outside.as_bytes()).is_err());

        for overlapping_preload in [
            "/guest/etc/ld.so.cache",
            "/guest/etc",
            "/guest/etc/ld.so.cache/child",
        ] {
            let occupied = valid.replacen(
                &hex(b"/guest/etc/ld.so.preload"),
                &hex(overlapping_preload.as_bytes()),
                1,
            );
            assert!(ReviewedManifestV4::parse(occupied.as_bytes()).is_err());
        }
    }

    #[test]
    fn role_soname_path_and_identity_aliases_are_rejected() {
        let valid = valid_manifest();
        let duplicate_soname = valid.replacen("libomega.so", "libalpha.so", 1);
        assert!(ReviewedManifestV4::parse(duplicate_soname.as_bytes()).is_err());

        let duplicate_store = valid.replacen(
            &hex(b"/store/artifacts/runtime"),
            &hex(b"/store/artifacts/cache"),
            1,
        );
        assert!(ReviewedManifestV4::parse(duplicate_store.as_bytes()).is_err());

        let duplicate_identity = valid.replacen("0000000000000010\t", "000000000000000f\t", 1);
        assert!(ReviewedManifestV4::parse(duplicate_identity.as_bytes()).is_err());
    }

    #[test]
    fn environment_is_sorted_bounded_and_loader_neutral() {
        let valid = valid_manifest();
        let unsorted = valid.replacen(
            "environment=41\t\n",
            "environment=42\t\nenvironment=41\t\n",
            1,
        );
        assert!(ReviewedManifestV4::parse(unsorted.as_bytes()).is_err());

        let unsafe_key = valid.replacen("environment=41\t", "environment=4c445f58\t", 1);
        assert!(ReviewedManifestV4::parse(unsafe_key.as_bytes()).is_err());

        let nul_value = valid.replacen("environment=41\t\n", "environment=41\t00\n", 1);
        assert!(ReviewedManifestV4::parse(nul_value.as_bytes()).is_err());
    }

    #[test]
    fn unknown_missing_and_extra_fields_are_rejected() {
        let valid = valid_manifest();
        for changed in [
            valid.replacen("cover=", "unknown=", 1),
            valid.replacen("\tabsent\n", "\tabsent\textra\n", 1),
            valid.replacen("artifact=runtime\t", "artifact=runtime\textra\t", 1),
            valid.replacen("artifact=runtime_marker", "artifact=unknown", 1),
        ] {
            assert!(ReviewedManifestV4::parse(changed.as_bytes()).is_err());
        }
    }
}
