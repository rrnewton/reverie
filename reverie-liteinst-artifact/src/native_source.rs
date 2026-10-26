// Copyright (c) Meta Platforms, Inc. and affiliates.
// SPDX-License-Identifier: BSD-2-Clause

//! Private source binding for the checked native action factory.
//!
//! These internal inputs are supplied only after the factory has checked the
//! independently expected declared producer edges, invocation and consumed
//! products. They are not a wire format or a public record-admission API. The
//! factory retains its complete file/parent identity transaction through final
//! close, including generated, response, tool and dependency inputs.

use super::*;

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub(super) struct NativeProfile {
    pub opt_level: String,
    pub debuginfo: u32,
    pub debug_assertions: bool,
    pub overflow_checks: bool,
    pub test: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub(super) struct NativePackage {
    pub crate_name: String,
    pub features: Vec<String>,
    pub profile: NativeProfile,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub(super) struct NativeSourceBinding {
    // Exact rv:<relative> catalog roles, with explicit current materializations.
    pub files: BTreeMap<String, FileIdentity>,
    // Original compiler spellings, independently bound to those logical roles
    // by the action factory. Never reconstructed from a relocated current path.
    pub compiler_filenames: BTreeMap<String, Vec<u8>>,
    pub packages: BTreeMap<String, NativePackage>,
    pub artifact: FileIdentity,
}

// No Deserialize, public constructor, Clone or escaping authority handle.
pub(super) struct VerifiedNativeSources {
    binding: NativeSourceBinding,
    source_sha256: String,
    cohort: NoScopeCohort,
}

fn expected_file_roles(cohort: NoScopeCohort) -> BTreeSet<String> {
    CONTENT_REVERIE_CATALOG
        .iter()
        .map(|entry| entry.0)
        .chain(
            (cohort.image_kind() == FixedImageKind::Detcore)
                .then_some(CONTENT_DETCORE_FIXED_GATE.0),
        )
        .chain(["reverie-liteinst-artifact/src/lib.rs"])
        .map(|relative| format!("rv:{relative}"))
        .collect()
}

fn select_action_cohort(packages: &BTreeMap<String, NativePackage>) -> Result<NoScopeCohort> {
    let profiles = packages
        .iter()
        .map(|(name, package)| {
            require(
                package.crate_name == name.replace('-', "_"),
                "native source package has a different compiled crate identity",
            )?;
            Ok((
                name.as_str(),
                serde_json::to_value(&package.profile).map_err(|e| e.to_string())?,
            ))
        })
        .collect::<Result<BTreeMap<_, _>>>()?;
    let selected = packages
        .iter()
        .map(|(name, package)| {
            (
                name.as_str(),
                (package.features.as_slice(), &profiles[name.as_str()]),
            )
        })
        .collect();
    select_no_scope_cohort(&selected)
}

fn validate_packages(packages: &BTreeMap<String, NativePackage>) -> Result<NoScopeCohort> {
    let cohort = select_action_cohort(packages)?;
    require(
        cohort == NoScopeCohort::DetcoreOrdinary101,
        "ordinary native constructor requires the ordinary OFF cohort",
    )?;
    Ok(cohort)
}

fn validate_filename_roles(filenames: &BTreeMap<String, Vec<u8>>) -> Result<()> {
    validate_cohort_filename_roles(filenames, NoScopeCohort::DetcoreOrdinary101)
}

fn validate_cohort_filename_roles(
    filenames: &BTreeMap<String, Vec<u8>>,
    cohort: NoScopeCohort,
) -> Result<()> {
    // The standalone physical families can request both publication and preload
    // filenames. Bind their closed union before examining the selected ELF.
    // Every spelling still comes from that producer's actual compiler remaps.
    let required = match cohort {
        NoScopeCohort::StandaloneRelease62 => STANDALONE_COMPILED_SOURCE_FILES.as_slice(),
        NoScopeCohort::DetcoreOrdinary101 | NoScopeCohort::DetcoreNative101 => {
            ORDINARY_COMPILED_SOURCE_FILES.as_slice()
        }
    };
    let expected: BTreeSet<_> = required
        .iter()
        .map(|relative| format!("rv:{relative}"))
        .collect();
    require(
        filenames.keys().cloned().collect::<BTreeSet<_>>() == expected,
        "native compiler filename mapping lacks the exact source role set",
    )?;
    let mut spellings = BTreeSet::new();
    for spelling in filenames.values() {
        require(
            !spelling.is_empty()
                && !spelling.contains(&0)
                && std::str::from_utf8(spelling).is_ok()
                && spellings.insert(spelling),
            "native compiler filename spelling is empty, invalid or aliased",
        )?;
    }
    Ok(())
}

// Only these dynamic Location filenames enter the ordinary OFF physical proof.
// The complete source inventory remains CONTENT_REVERIE_CATALOG; this does not
// require a compilation of the standalone preload or keyed/explicit entry path.
const ORDINARY_COMPILED_SOURCE_FILES: [&str; 4] = [
    "reverie-liteinst/src/patch_alloc.rs",
    "reverie-inguest/src/guest/alloc.rs",
    "reverie-inguest/src/guest/tool_region_constructor.rs",
    "reverie-inguest/src/guest/tool_region_arena.rs",
];

const STANDALONE_COMPILED_SOURCE_FILES: [&str; 6] = [
    ORDINARY_COMPILED_SOURCE_FILES[0],
    ORDINARY_COMPILED_SOURCE_FILES[1],
    ORDINARY_COMPILED_SOURCE_FILES[2],
    ORDINARY_COMPILED_SOURCE_FILES[3],
    "reverie-inguest/src/guest/tool_region.rs",
    "reverie-liteinst-preload/src/lib.rs",
];

fn normalized_source_path(bytes: &[u8], relative: bool, trailing_slash: bool) -> Result<&str> {
    let value = std::str::from_utf8(bytes).map_err(|e| e.to_string())?;
    require(
        !value.is_empty()
            && !value.contains('\0')
            && !value.contains('\\')
            && (!relative || !value.starts_with('/')),
        "compiler source path is empty or has an unsupported spelling",
    )?;
    let value = if trailing_slash {
        value.strip_suffix('/').unwrap_or(value)
    } else {
        value
    };
    let components = value.strip_prefix('/').unwrap_or(value);
    require(
        !components.is_empty()
            && components
                .split('/')
                .all(|part| !part.is_empty() && part != "." && part != ".."),
        "compiler source path has non-normal components",
    )?;
    Ok(value)
}

fn prefix_suffix<'a>(path: &'a str, prefix: &str) -> Option<&'a str> {
    path.strip_prefix(prefix)
        .filter(|suffix| suffix.is_empty() || suffix.starts_with('/'))
}

/// Check a source member's filename against the actual normalized compiler
/// operands, using its original recipe spelling even after cache relocation.
/// The caller proves that `relative_member` is a checked member of that exact
/// source-tree Artifact and joins the original tree through its logical ID.
///
/// This intentionally supports only unambiguous explicit remapping: at most one
/// prefix may match the original file, and another remap may not match its
/// result. It does not guess overlapping-prefix precedence. The normal producer
/// emits `--remap-path-prefix=<source_tree>/=<owner-path>/`; normalized argv is
/// retained in its original order. No historical path is opened or canonicalized.
pub(super) fn checked_compiler_filename(
    original_source_tree: &[u8],
    relative_member: &Path,
    expanded_argv: &[Vec<u8>],
    expected_filename: &[u8],
) -> Result<()> {
    let tree = normalized_source_path(original_source_tree, false, false)?;
    let member = relative_member
        .to_str()
        .ok_or("compiler source member is not UTF-8")?;
    normalized_source_path(member.as_bytes(), true, false)?;
    let original = format!("{tree}/{member}");
    let args = expanded_argv
        .iter()
        .map(|arg| {
            let value = std::str::from_utf8(arg).map_err(|e| e.to_string())?;
            require(!value.contains('\0'), "NUL in normalized compiler operand")?;
            Ok(value)
        })
        .collect::<Result<Vec<_>>>()?;
    require(
        !args.is_empty() && !args[0].is_empty(),
        "compiler argv is empty",
    )?;
    let mut remaps = Vec::new();
    let mut roots = Vec::new();
    let mut index = 1;
    while index < args.len() {
        let arg = args[index];
        let remap = if let Some(value) = arg.strip_prefix("--remap-path-prefix=") {
            Some(value)
        } else if arg == "--remap-path-prefix" {
            index += 1;
            Some(*args.get(index).ok_or("missing compiler remap operand")?)
        } else {
            None
        };
        if let Some(remap) = remap {
            let mut parts = remap.split('=');
            let from = parts.next().unwrap();
            let to = parts.next().ok_or("compiler remap lacks destination")?;
            require(
                parts.next().is_none(),
                "compiler remap contains ambiguous equals signs",
            )?;
            let from = normalized_source_path(from.as_bytes(), false, true)?;
            // The normal wrapper appends absolute cwd-to-dot remaps. They do
            // not match its relative source-tree operand; retain their exact
            // spelling without inventing dot normalization for a matching map.
            let to = if to.is_empty() || to == "." {
                to
            } else {
                normalized_source_path(to.as_bytes(), false, true)?
            };
            remaps.push((from, to));
        } else if matches!(
            arg,
            "--crate-name"
                | "--crate-type"
                | "--emit"
                | "--print"
                | "--cfg"
                | "--check-cfg"
                | "--edition"
                | "--target"
                | "--out-dir"
                | "--extern"
                | "--sysroot"
                | "--error-format"
                | "--json"
                | "--color"
                | "--cap-lints"
                | "--diagnostic-width"
                | "--remap-path-scope"
                | "--allow"
                | "--warn"
                | "--deny"
                | "--forbid"
                | "--force-warn"
                | "--codegen"
                | "--library-path"
                | "-o"
                | "-L"
                | "-C"
                | "-Z"
                | "-A"
                | "-W"
                | "-D"
                | "-F"
        ) {
            index += 1;
            require(index < args.len(), "missing compiler option operand")?;
        } else if arg.starts_with('-') {
            require(
                arg.contains('=')
                    || ["-C", "-Z", "-L", "-A", "-W", "-D", "-F"]
                        .iter()
                        .any(|prefix| arg.starts_with(prefix) && arg.len() > prefix.len())
                    || matches!(arg, "--test" | "-g" | "-O" | "-v" | "--verbose"),
                "unsupported compiler option while locating the source operand",
            )?;
        } else {
            require(!arg.starts_with('@'), "unexpanded compiler response input")?;
            normalized_source_path(arg.as_bytes(), false, false)?;
            require(
                prefix_suffix(arg, tree)
                    .is_some_and(|suffix| suffix.starts_with('/') && suffix.ends_with(".rs")),
                "compiler crate root is not inside the declared original source tree",
            )?;
            roots.push(arg);
        }
        index += 1;
    }
    require(
        roots.len() == 1,
        "compiler invocation lacks one explicit source-tree crate root",
    )?;
    let matching = remaps
        .iter()
        .enumerate()
        .filter_map(|(index, (from, to))| {
            prefix_suffix(&original, from).map(|suffix| (index, *to, suffix))
        })
        .collect::<Vec<_>>();
    require(
        matching.len() <= 1,
        "overlapping compiler source remaps are unsupported",
    )?;
    let mapped = if let Some((matched, to, suffix)) = matching.first() {
        require(
            *to != ".",
            "matching cwd-to-dot compiler remap is outside the supported relative-tree contract",
        )?;
        let mapped = if to.is_empty() {
            suffix.trim_start_matches('/').to_owned()
        } else {
            format!("{to}{suffix}")
        };
        require(
            remaps.iter().enumerate().all(|(index, (from, _))| {
                index == *matched || prefix_suffix(&mapped, from).is_none()
            }),
            "compiler source remaps could chain ambiguously",
        )?;
        mapped
    } else {
        original
    };
    require(
        mapped.as_bytes() == expected_filename,
        "expected compiler filename differs from the actual source-tree member and remap",
    )
}

impl VerifiedNativeSources {
    /// Only the checked action factory calls this after its normal producer
    /// bridge and independent expectations have been joined. A successful
    /// catalog check alone is not compilation authority.
    pub(super) fn from_checked_action(binding: &NativeSourceBinding) -> Result<Self> {
        let cohort = select_action_cohort(&binding.packages)?;
        require(
            binding.files.keys().cloned().collect::<BTreeSet<_>>() == expected_file_roles(cohort),
            "native source mapping differs from the complete exact source catalog",
        )?;
        if cohort == NoScopeCohort::DetcoreOrdinary101 {
            validate_filename_roles(&binding.compiler_filenames)?;
        } else {
            validate_cohort_filename_roles(&binding.compiler_filenames, cohort)?;
        }
        let mut inventory = Vec::new();
        let mut paths = BTreeSet::new();
        for (role, file) in &binding.files {
            file.verify()?;
            require(
                paths.insert(file.path.clone()),
                "native source catalog roles alias one current file",
            )?;
            inventory.push(DeclaredSourceFile {
                relative: role
                    .strip_prefix("rv:")
                    .ok_or("native source role prefix differs")?
                    .into(),
                size: fs::metadata(&file.path).map_err(|e| e.to_string())?.len(),
                sha256: file.sha256.clone(),
            });
        }
        // Same literal hashes, sizes, qualifier self hash and separately
        // compiled module hashes as the existing content source validator.
        validate_reverie_inventory_entries(&inventory, cohort.image_kind())?;
        binding.artifact.verify()?;
        let source_sha256 = sha256(&serde_json::to_vec(binding).map_err(|e| e.to_string())?);
        let sources = Self {
            binding: binding.clone(),
            source_sha256,
            cohort,
        };
        sources.verify_unchanged()?;
        Ok(sources)
    }

    pub(super) fn verify_unchanged(&self) -> Result<()> {
        // Check every file even on failure; the factory additionally closes
        // the descriptor/path/parent identities and its other declared inputs.
        let mut errors = Vec::new();
        for (role, file) in &self.binding.files {
            if let Err(error) = file.verify() {
                errors.push(format!("{role}: {error}"));
            }
        }
        if let Err(error) = self.binding.artifact.verify() {
            errors.push(format!("native selected product: {error}"));
        }
        require(errors.is_empty(), errors.join("; "))
    }

    pub(super) fn require_image(&self, initializer: &str) -> Result<()> {
        require(
            initializer == self.cohort.image_kind().initializer(),
            "native source context belongs to a different final image",
        )?;
        self.verify_unchanged()
    }

    pub(super) fn filename(&self, relative: &[u8]) -> Result<Vec<u8>> {
        let relative = std::str::from_utf8(relative).map_err(|e| e.to_string())?;
        if !COMPILED_RV_LOCATION_FILES.contains(&relative) {
            return Ok(relative.as_bytes().to_vec());
        }
        self.verify_unchanged()?;
        self.binding
            .compiler_filenames
            .get(&format!("rv:{relative}"))
            .cloned()
            .ok_or_else(|| "native compiler source role absent".into())
    }

    pub(super) fn require_artifact_bytes(&self, bytes: &[u8]) -> Result<()> {
        self.verify_unchanged()?;
        require(
            self.binding.artifact.sha256 == sha256(bytes),
            "native physical proof uses a different selected product",
        )
    }

    pub(super) fn no_scope_source(&self) -> Result<&FileIdentity> {
        self.binding
            .files
            .get(&format!("rv:{}", NO_INSTALLATION_SCOPE_SOURCE.0))
            .ok_or_else(|| "native no-scope source role absent".into())
    }

    pub(super) fn cohort(&self) -> NoScopeCohort {
        self.cohort
    }
}

/// Completed native constructor evidence. It is distinct from Cargo envelopes
/// and deliberately cannot be deserialized into a fresh source context.
#[derive(Debug, Serialize)]
pub struct NativeConstructorQualification {
    kind: &'static str,
    version: u32,
    native_sources_sha256: String,
    artifact: FileIdentity,
    allocator_backing: NativeAllocatorBacking,
    initializer: SymbolEvidence,
    init_array_slot: u64,
    init_array_resolution: String,
    constructor: DetcoreConstructorEvidence,
    generic_bootstrap: GenericBootstrapEvidence,
    needed: Vec<String>,
    required_versions: Vec<String>,
    readelf_versions: FileIdentity,
    readelf_symbols_relocations: FileIdentity,
    objdump_disassembly: FileIdentity,
}

#[derive(Debug, Serialize)]
struct NativeAllocatorBacking {
    physical: AllocatorBackingPhysical,
    no_installation_scope: NoScopePhysical,
}

impl NativeConstructorQualification {
    pub fn source_sha256(&self) -> &str {
        &self.native_sources_sha256
    }
    pub fn constructor(&self) -> &DetcoreConstructorEvidence {
        &self.constructor
    }
    pub fn artifact(&self) -> &FileIdentity {
        &self.artifact
    }
    pub fn generic_bootstrap(&self) -> &GenericBootstrapEvidence {
        &self.generic_bootstrap
    }
}

pub(super) fn qualify_native_constructor(
    path: &Path,
    runner: &mut Runner,
    sources: &VerifiedNativeSources,
) -> Result<NativeConstructorQualification> {
    let primary = (|| {
        sources.verify_unchanged()?;
        validate_packages(&sources.binding.packages)?;
        require(
            FileIdentity::read(path)? == sources.binding.artifact,
            "native constructor path is not the checked selected product",
        )?;
        let physical = collect_constructor_qualification(
            path,
            runner,
            Some(SourceContextRef::Native(sources)),
        )?;
        let QualifiedAllocator::Native {
            physical: allocator,
            no_scope,
        } = physical.allocator_backing
        else {
            return Err("native constructor received foreign allocator provenance".into());
        };
        Ok(NativeConstructorQualification {
            kind: "native_constructor_qualification",
            version: 1,
            native_sources_sha256: sources.source_sha256.clone(),
            artifact: physical.artifact,
            allocator_backing: NativeAllocatorBacking {
                physical: *allocator,
                no_installation_scope: *no_scope,
            },
            initializer: physical.initializer,
            init_array_slot: physical.init_array_slot,
            init_array_resolution: physical.init_array_resolution,
            constructor: physical.detcore_constructor,
            generic_bootstrap: physical.generic_bootstrap,
            needed: physical.needed,
            required_versions: physical.required_versions,
            readelf_versions: physical.readelf_versions,
            readelf_symbols_relocations: physical.readelf_symbols_relocations,
            objdump_disassembly: physical.objdump_disassembly,
        })
    })();
    match (primary, sources.verify_unchanged()) {
        (Ok(value), Ok(())) => Ok(value),
        (Err(primary), Ok(())) => Err(primary),
        (Ok(_), Err(close)) => Err(format!("native source close failed: {close}")),
        (Err(primary), Err(close)) => {
            Err(format!("{primary}; native source close failed: {close}"))
        }
    }
}

/// Completed native physical evidence, distinct from Cargo serialization.
/// This observation cannot construct or deserialize a live source authority.
#[derive(Debug, Serialize)]
pub struct NativeStandaloneConstructorQualification {
    kind: &'static str,
    version: u32,
    native_sources_sha256: String,
    artifact: FileIdentity,
    allocator_backing: NativeAllocatorBacking,
    initializer: SymbolEvidence,
    init_array_slot: u64,
    init_array_resolution: String,
    standalone_constructor: StandaloneConstructorEvidence,
    generic_bootstrap: GenericBootstrapEvidence,
    needed: Vec<String>,
    required_versions: Vec<String>,
    readelf_versions: FileIdentity,
    readelf_symbols_relocations: FileIdentity,
    objdump_disassembly: FileIdentity,
}
impl NativeStandaloneConstructorQualification {
    pub fn source_sha256(&self) -> &str {
        &self.native_sources_sha256
    }
    pub fn artifact(&self) -> &FileIdentity {
        &self.artifact
    }
    pub fn generic_bootstrap(&self) -> &GenericBootstrapEvidence {
        &self.generic_bootstrap
    }
    pub fn standalone_constructor(&self) -> &StandaloneConstructorEvidence {
        &self.standalone_constructor
    }
}
pub(super) fn qualify_native_standalone_constructor(
    path: &Path,
    runner: &mut Runner,
    sources: &VerifiedNativeSources,
) -> Result<NativeStandaloneConstructorQualification> {
    let primary = (|| {
        sources.verify_unchanged()?;
        SourceContextRef::Native(sources)
            .require_native_cohort(NoScopeCohort::StandaloneRelease62)?;
        require(
            FileIdentity::read(path)? == sources.binding.artifact,
            "native qualification path is not the checked selected product",
        )?;
        let physical = collect_standalone_qualification(
            path,
            runner,
            Some(SourceContextRef::Native(sources)),
        )?;
        let QualifiedAllocator::Native {
            physical: allocator,
            no_scope,
        } = physical.allocator_backing
        else {
            return Err("native qualification received foreign allocator provenance".into());
        };
        Ok(NativeStandaloneConstructorQualification {
            kind: "native_standalone_constructor_qualification",
            version: 1,
            native_sources_sha256: sources.source_sha256.clone(),
            artifact: physical.artifact,
            allocator_backing: NativeAllocatorBacking {
                physical: *allocator,
                no_installation_scope: *no_scope,
            },
            initializer: physical.initializer,
            init_array_slot: physical.init_array_slot,
            init_array_resolution: physical.init_array_resolution,
            standalone_constructor: physical.standalone_constructor,
            generic_bootstrap: physical.generic_bootstrap,
            needed: physical.needed,
            required_versions: physical.required_versions,
            readelf_versions: physical.readelf_versions,
            readelf_symbols_relocations: physical.readelf_symbols_relocations,
            objdump_disassembly: physical.objdump_disassembly,
        })
    })();
    close_sources(primary, sources)
}

/// Completed native physical evidence, distinct from Cargo serialization.
/// This observation cannot construct or deserialize a live source authority.
#[derive(Debug, Serialize)]
pub struct NativeEntryQualification {
    kind: &'static str,
    version: u32,
    native_sources_sha256: String,
    artifact: FileIdentity,
    allocator_backing: NativeAllocatorBacking,
    native_entry: SymbolEvidence,
    native_entry_sha256: String,
    generic_bootstrap: GenericBootstrapEvidence,
    needed: Vec<String>,
    required_versions: Vec<String>,
    readelf_versions: FileIdentity,
    readelf_symbols_relocations: FileIdentity,
    objdump_disassembly: FileIdentity,
    observation: String,
}
impl NativeEntryQualification {
    pub fn source_sha256(&self) -> &str {
        &self.native_sources_sha256
    }
    pub fn artifact(&self) -> &FileIdentity {
        &self.artifact
    }
    pub fn generic_bootstrap(&self) -> &GenericBootstrapEvidence {
        &self.generic_bootstrap
    }
    pub fn native_entry(&self) -> &SymbolEvidence {
        &self.native_entry
    }
    pub fn native_entry_sha256(&self) -> &str {
        &self.native_entry_sha256
    }
}
pub(super) fn qualify_native_entry(
    path: &Path,
    runner: &mut Runner,
    sources: &VerifiedNativeSources,
) -> Result<NativeEntryQualification> {
    let primary = (|| {
        sources.verify_unchanged()?;
        SourceContextRef::Native(sources).require_native_cohort(NoScopeCohort::DetcoreNative101)?;
        require(
            FileIdentity::read(path)? == sources.binding.artifact,
            "native qualification path is not the checked selected product",
        )?;
        let physical =
            collect_native_entry_qualification(path, runner, SourceContextRef::Native(sources))?;
        let QualifiedAllocator::Native {
            physical: allocator,
            no_scope,
        } = physical.allocator_backing
        else {
            return Err("native qualification received foreign allocator provenance".into());
        };
        Ok(NativeEntryQualification {
            kind: "native_entry_qualification",
            version: 1,
            native_sources_sha256: sources.source_sha256.clone(),
            artifact: physical.artifact,
            allocator_backing: NativeAllocatorBacking {
                physical: *allocator,
                no_installation_scope: *no_scope,
            },
            native_entry: physical.native_entry,
            native_entry_sha256: physical.native_entry_sha256,
            generic_bootstrap: physical.generic_bootstrap,
            needed: physical.needed,
            required_versions: physical.required_versions,
            readelf_versions: physical.readelf_versions,
            readelf_symbols_relocations: physical.readelf_symbols_relocations,
            objdump_disassembly: physical.objdump_disassembly,
            observation: physical.observation,
        })
    })();
    close_sources(primary, sources)
}

fn close_sources<T>(primary: Result<T>, sources: &VerifiedNativeSources) -> Result<T> {
    match (primary, sources.verify_unchanged()) {
        (Ok(value), Ok(())) => Ok(value),
        (Err(primary), Ok(())) => Err(primary),
        (Ok(_), Err(close)) => Err(format!("native source close failed: {close}")),
        (Err(primary), Err(close)) => {
            Err(format!("{primary}; native source close failed: {close}"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ordinary_packages() -> BTreeMap<String, NativePackage> {
        NoScopeCohort::DetcoreOrdinary101
            .features()
            .iter()
            .map(|(name, features)| {
                (
                    (*name).into(),
                    NativePackage {
                        crate_name: name.replace('-', "_"),
                        features: features.iter().map(|value| (*value).into()).collect(),
                        profile: NativeProfile {
                            opt_level: "3".into(),
                            debuginfo: 0,
                            debug_assertions: true,
                            overflow_checks: true,
                            test: false,
                        },
                    },
                )
            })
            .collect()
    }

    fn compiler_filenames() -> BTreeMap<String, Vec<u8>> {
        ORDINARY_COMPILED_SOURCE_FILES
            .iter()
            .map(|relative| {
                (
                    format!("rv:{relative}"),
                    format!("/original/compiler/tree/{relative}").into_bytes(),
                )
            })
            .collect()
    }

    #[test]
    fn compiler_filename_joins_original_tree_member_and_normal_remap() {
        let args = [
            "rustc",
            "cache/original/src/lib.rs",
            "--crate-name",
            "guest",
            "--remap-path-prefix=cache/original/=reverie-inguest/",
            "--remap-path-prefix=/work/=.",
            "--remap-path-prefix=/real-work/=.",
        ]
        .map(|value| value.as_bytes().to_vec());
        checked_compiler_filename(
            b"cache/original",
            Path::new("src/guest/alloc.rs"),
            &args,
            b"reverie-inguest/src/guest/alloc.rs",
        )
        .unwrap();
        assert!(
            checked_compiler_filename(
                b"cache/current",
                Path::new("src/guest/alloc.rs"),
                &args,
                b"reverie-inguest/src/guest/alloc.rs"
            )
            .is_err()
        );
        assert!(
            checked_compiler_filename(
                b"cache/original",
                Path::new("src/guest/alloc.rs"),
                &args,
                b"unrelated/src/guest/alloc.rs"
            )
            .is_err()
        );
        assert!(
            checked_compiler_filename(
                b"cache/original",
                Path::new("../src/guest/alloc.rs"),
                &args,
                b"reverie-inguest/src/guest/alloc.rs"
            )
            .is_err()
        );
        let mut overlap = args.to_vec();
        overlap.push(b"--remap-path-prefix=cache/=other/".to_vec());
        assert!(
            checked_compiler_filename(
                b"cache/original",
                Path::new("src/guest/alloc.rs"),
                &overlap,
                b"reverie-inguest/src/guest/alloc.rs"
            )
            .is_err()
        );
        let mut chained = args.to_vec();
        chained.push(b"--remap-path-prefix=reverie-inguest/=other/".to_vec());
        assert!(
            checked_compiler_filename(
                b"cache/original",
                Path::new("src/guest/alloc.rs"),
                &chained,
                b"reverie-inguest/src/guest/alloc.rs"
            )
            .is_err()
        );
        let wrong_prefix = [
            "rustc",
            "cache/original/src/lib.rs",
            "--remap-path-prefix=cache/orig/=wrong/",
        ]
        .map(|value| value.as_bytes().to_vec());
        checked_compiler_filename(
            b"cache/original",
            Path::new("src/guest/alloc.rs"),
            &wrong_prefix,
            b"cache/original/src/guest/alloc.rs",
        )
        .unwrap();
        let option_only = ["rustc", "--extern", "cache/original/src/lib.rs"]
            .map(|value| value.as_bytes().to_vec());
        assert!(
            checked_compiler_filename(
                b"cache/original",
                Path::new("src/guest/alloc.rs"),
                &option_only,
                b"cache/original/src/guest/alloc.rs"
            )
            .is_err()
        );
        let matching_dot = [
            "rustc",
            "cache/original/src/lib.rs",
            "--remap-path-prefix=cache/original/=.",
        ]
        .map(|value| value.as_bytes().to_vec());
        assert!(
            checked_compiler_filename(
                b"cache/original",
                Path::new("src/guest/alloc.rs"),
                &matching_dot,
                b"src/guest/alloc.rs"
            )
            .is_err()
        );
    }

    #[test]
    fn ordinary_native_cohort_refuses_changed_dependency_and_entry_contracts() {
        let expected = ordinary_packages();
        assert_eq!(
            validate_packages(&expected).unwrap(),
            NoScopeCohort::DetcoreOrdinary101
        );
        for role in expected.keys() {
            let mut wrong = expected.clone();
            wrong.get_mut(role).unwrap().profile.debug_assertions = false;
            assert!(
                validate_packages(&wrong).is_err(),
                "changed profile accepted for {role}"
            );
            let mut wrong = expected.clone();
            wrong.get_mut(role).unwrap().profile.test = true;
            assert!(
                validate_packages(&wrong).is_err(),
                "test artifact accepted for {role}"
            );
            let mut wrong = expected.clone();
            wrong
                .get_mut(role)
                .unwrap()
                .crate_name
                .push_str("_lookalike");
            assert!(
                validate_packages(&wrong).is_err(),
                "wrong crate accepted for {role}"
            );
            let mut wrong = expected.clone();
            wrong
                .get_mut(role)
                .unwrap()
                .features
                .push("liteinst2".into());
            assert!(
                validate_packages(&wrong).is_err(),
                "ON feature accepted for {role}"
            );
        }
        let mut native_entry = expected.clone();
        for (name, features) in NoScopeCohort::DetcoreNative101.features() {
            native_entry.get_mut(*name).unwrap().features =
                features.iter().map(|value| (*value).into()).collect();
        }
        assert!(
            validate_packages(&native_entry).is_err(),
            "explicit-entry cohort entered ordinary factory"
        );
        let mut missing = expected.clone();
        missing.remove("reverie-inguest");
        assert!(validate_packages(&missing).is_err());
        let mut extra = expected.clone();
        extra.insert("foreign".into(), expected["reverie-liteinst"].clone());
        assert!(validate_packages(&extra).is_err());
    }

    #[test]
    fn compiler_filename_mapping_refuses_missing_extra_and_aliased_roles() {
        let expected = compiler_filenames();
        validate_filename_roles(&expected).unwrap();
        for role in expected.keys() {
            let mut missing = expected.clone();
            missing.remove(role);
            assert!(validate_filename_roles(&missing).is_err());
        }
        let mut extra = expected.clone();
        extra.insert(
            "rv:foreign/src/lib.rs".into(),
            b"/foreign/src/lib.rs".to_vec(),
        );
        assert!(validate_filename_roles(&extra).is_err());
        let mut aliased = expected.clone();
        let first = aliased.values().next().unwrap().clone();
        *aliased.values_mut().nth(1).unwrap() = first;
        assert!(validate_filename_roles(&aliased).is_err());
        for invalid in [Vec::new(), vec![0], vec![0xff]] {
            let mut wrong = expected.clone();
            *wrong.values_mut().next().unwrap() = invalid;
            assert!(validate_filename_roles(&wrong).is_err());
        }
    }

    #[test]
    fn native_source_refuses_record_only_catalog_and_cargo_projection() {
        let binding = NativeSourceBinding {
            files: BTreeMap::new(),
            compiler_filenames: compiler_filenames(),
            packages: ordinary_packages(),
            artifact: FileIdentity {
                path: PathBuf::from("/unused-product"),
                sha256: "0".repeat(64),
            },
        };
        assert!(VerifiedNativeSources::from_checked_action(&binding).is_err());
        // Internal dispatch fixture only: this deliberately bypasses acquisition
        // to assert that even a native reference has no Cargo evidence projection.
        let fixture = VerifiedNativeSources {
            binding,
            source_sha256: "0".repeat(64),
            cohort: NoScopeCohort::DetcoreOrdinary101,
        };
        assert!(SourceContextRef::Native(&fixture).cargo().is_err());
        assert!(fixture.require_image(STANDALONE_ROOT).is_err());
    }

    #[test]
    fn relocated_filenames_remain_original_and_source_failures_all_close() {
        static SERIAL: AtomicUsize = AtomicUsize::new(0);
        let root = std::env::temp_dir().join(format!(
            "native-source-boundary-{}-{}",
            std::process::id(),
            SERIAL.fetch_add(1, Ordering::Relaxed),
        ));
        fs::create_dir(&root).unwrap();
        struct RemoveOwned(PathBuf);
        impl Drop for RemoveOwned {
            fn drop(&mut self) {
                let _ = fs::remove_dir_all(&self.0);
            }
        }
        let _cleanup = RemoveOwned(root.clone());
        let first = root.join("current-first.rs");
        let second = root.join("current-second.rs");
        let product = root.join("current-product.so");
        for path in [&first, &second, &product] {
            fs::write(path, b"before").unwrap();
        }
        let first_role = format!("rv:{}", COMPILED_RV_LOCATION_FILES[0]);
        let second_role = format!("rv:{}", COMPILED_RV_LOCATION_FILES[1]);
        let filenames = compiler_filenames();
        let binding = NativeSourceBinding {
            files: BTreeMap::from([
                (first_role.clone(), FileIdentity::read(&first).unwrap()),
                (second_role.clone(), FileIdentity::read(&second).unwrap()),
            ]),
            compiler_filenames: filenames.clone(),
            packages: ordinary_packages(),
            artifact: FileIdentity::read(&product).unwrap(),
        };
        // This fixture exercises only private mapping and close behavior. Its
        // incomplete catalog cannot pass the factory or qualify any artifact.
        assert!(VerifiedNativeSources::from_checked_action(&binding).is_err());
        let fixture = VerifiedNativeSources {
            binding,
            source_sha256: "0".repeat(64),
            cohort: NoScopeCohort::DetcoreOrdinary101,
        };
        assert_eq!(
            fixture
                .filename(COMPILED_RV_LOCATION_FILES[0].as_bytes())
                .unwrap(),
            filenames[&first_role]
        );
        assert_ne!(filenames[&first_role], first.to_str().unwrap().as_bytes());
        fixture.verify_unchanged().unwrap();
        for path in [&first, &second, &product] {
            fs::write(path, b"changed").unwrap();
        }
        let error = fixture.verify_unchanged().unwrap_err();
        assert!(error.contains(&first_role) && error.contains(&second_role));
        assert!(error.contains("native selected product"));
        assert!(
            fixture
                .filename(COMPILED_RV_LOCATION_FILES[0].as_bytes())
                .is_err()
        );
        let mut runner = Runner::new(root.join("logs"), Duration::from_secs(5), 1024).unwrap();
        let error = qualify_native_constructor(&product, &mut runner, &fixture).unwrap_err();
        assert!(error.contains("native source close failed:"));
        assert_eq!(
            error.matches(&first_role).count(),
            2,
            "primary and close source failures must both survive"
        );
        assert!(
            runner.commands.is_empty(),
            "invalid sources reached a physical tool command"
        );
    }

    // Complete catalog controls use owned copies of the actual slot sources.
    // Product bytes are deliberately not ELF: admission here is source-only,
    // never a compiler, physical qualification, or native execution positive.
    struct AdmissionFixture {
        root: PathBuf,
        files: BTreeMap<String, FileIdentity>,
    }
    impl AdmissionFixture {
        fn new() -> Self {
            static SERIAL: AtomicUsize = AtomicUsize::new(0);
            let root = PathBuf::from(std::env::var_os("TMPDIR").expect("owned source-test TMPDIR"))
                .join(format!(
                    "native-cohort-sources-{}-{}",
                    std::process::id(),
                    SERIAL.fetch_add(1, Ordering::Relaxed)
                ));
            fs::create_dir(&root).unwrap();
            let slot = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
            let mut files = BTreeMap::new();
            for relative in CONTENT_REVERIE_CATALOG.iter().map(|entry| entry.0).chain([
                CONTENT_DETCORE_FIXED_GATE.0,
                "reverie-liteinst-artifact/src/lib.rs",
            ]) {
                let copy = root.join("sources").join(relative);
                fs::create_dir_all(copy.parent().unwrap()).unwrap();
                fs::copy(slot.join(relative), &copy).unwrap();
                files.insert(format!("rv:{relative}"), FileIdentity::read(&copy).unwrap());
            }
            assert_eq!(
                files["rv:reverie-liteinst-artifact/src/lib.rs"].sha256,
                qualifier_source_sha256()
            );
            fs::write(
                root.join("selected-product"),
                b"source admission only; not an ELF\n",
            )
            .unwrap();
            Self { root, files }
        }
        fn binding(&self, cohort: NoScopeCohort) -> NativeSourceBinding {
            let mut files = self.files.clone();
            if cohort == NoScopeCohort::StandaloneRelease62 {
                files.remove(&format!("rv:{}", CONTENT_DETCORE_FIXED_GATE.0));
            }
            // Independently name the four Detcore and six standalone filename
            // consumers, then prove each logical member/remap association.
            let mut names = vec![
                "reverie-liteinst/src/patch_alloc.rs",
                "reverie-inguest/src/guest/alloc.rs",
                "reverie-inguest/src/guest/tool_region_constructor.rs",
                "reverie-inguest/src/guest/tool_region_arena.rs",
            ];
            if cohort == NoScopeCohort::StandaloneRelease62 {
                names.extend([
                    "reverie-inguest/src/guest/tool_region.rs",
                    "reverie-liteinst-preload/src/lib.rs",
                ]);
            }
            let compiler_filenames = names
                .into_iter()
                .map(|relative| {
                    let (package, member) = relative.split_once('/').unwrap();
                    let tree = format!("original/normal/{package}");
                    let argv = vec![
                        b"./pinned-rustc".to_vec(),
                        format!("{tree}/src/lib.rs").into_bytes(),
                        format!("--remap-path-prefix={tree}/={package}/").into_bytes(),
                    ];
                    checked_compiler_filename(
                        tree.as_bytes(),
                        Path::new(member),
                        &argv,
                        relative.as_bytes(),
                    )
                    .unwrap();
                    (format!("rv:{relative}"), relative.as_bytes().to_vec())
                })
                .collect();
            NativeSourceBinding {
                files,
                compiler_filenames,
                packages: admission_packages(cohort),
                artifact: FileIdentity::read(&self.root.join("selected-product")).unwrap(),
            }
        }
    }
    impl Drop for AdmissionFixture {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.root).unwrap();
        }
    }

    fn admission_packages(cohort: NoScopeCohort) -> BTreeMap<String, NativePackage> {
        if cohort == NoScopeCohort::DetcoreOrdinary101 {
            return ordinary_packages();
        }
        let (roles, debug): (&[(&str, &[&str])], bool) = match cohort {
            NoScopeCohort::StandaloneRelease62 => (
                &[
                    ("hermit-liteinst-runtime-artifact", &[]),
                    ("reverie-liteinst-preload", &[]),
                    ("reverie-liteinst", &[]),
                    (
                        "reverie-inguest",
                        &["coordinator-rpc", "launcher", "rcb-clock"],
                    ),
                ],
                false,
            ),
            NoScopeCohort::DetcoreNative101 => (
                &[
                    (
                        "detcore-liteinst",
                        &["exec-native-injection", "exec-private-init"],
                    ),
                    (
                        "reverie-liteinst",
                        &[
                            "exec-native-injection",
                            "exec-private-init",
                            "exec-private-successor",
                            "fixed-syscall-gates",
                            "native-exec-state",
                        ],
                    ),
                    (
                        "reverie-inguest",
                        &[
                            "coordinator-rpc",
                            "exec-native-injection",
                            "exec-private-init",
                            "exec-private-successor",
                            "fixed-syscall-gates",
                            "launcher",
                            "native-exec-state",
                            "rcb-clock",
                        ],
                    ),
                ],
                true,
            ),
            NoScopeCohort::DetcoreOrdinary101 => unreachable!(),
        };
        roles
            .iter()
            .map(|(name, features)| {
                (
                    (*name).into(),
                    NativePackage {
                        crate_name: name.replace('-', "_"),
                        features: features.iter().map(|value| (*value).into()).collect(),
                        profile: NativeProfile {
                            opt_level: "3".into(),
                            debuginfo: 0,
                            debug_assertions: debug,
                            overflow_checks: debug,
                            test: false,
                        },
                    },
                )
            })
            .collect()
    }

    fn admission_error(binding: &NativeSourceBinding) -> String {
        match VerifiedNativeSources::from_checked_action(binding) {
            Ok(_) => panic!("invalid source binding was admitted"),
            Err(error) => error,
        }
    }
    fn qualification_error(
        route: NoScopeCohort,
        path: &Path,
        runner: &mut Runner,
        sources: &VerifiedNativeSources,
    ) -> String {
        let result = match route {
            NoScopeCohort::DetcoreOrdinary101 => {
                qualify_native_constructor(path, runner, sources).map(|_| ())
            }
            NoScopeCohort::StandaloneRelease62 => {
                qualify_native_standalone_constructor(path, runner, sources).map(|_| ())
            }
            NoScopeCohort::DetcoreNative101 => {
                qualify_native_entry(path, runner, sources).map(|_| ())
            }
        };
        result.expect_err("source-only/non-ELF fixture earned physical qualification")
    }

    #[test]
    fn checked_standalone_and_entry_admission_uses_actual_catalog_and_exact_roles() {
        let fixture = AdmissionFixture::new();
        for cohort in [
            NoScopeCohort::StandaloneRelease62,
            NoScopeCohort::DetcoreNative101,
        ] {
            let binding = fixture.binding(cohort);
            let sources = VerifiedNativeSources::from_checked_action(&binding).unwrap();
            assert_eq!(sources.cohort(), cohort);
            assert_eq!(sources.binding.artifact, binding.artifact);
            sources.verify_unchanged().unwrap();
            sources
                .require_image(cohort.image_kind().initializer())
                .unwrap();
            assert!(SourceContextRef::Native(&sources).cargo().is_err());
            for (role, expected) in &binding.compiler_filenames {
                let relative = role.strip_prefix("rv:").unwrap();
                assert_eq!(&sources.filename(relative.as_bytes()).unwrap(), expected);
                assert_ne!(
                    expected.as_slice(),
                    binding.files[role].path.as_os_str().as_encoded_bytes()
                );
            }
            assert_eq!(
                binding.compiler_filenames.len(),
                if cohort == NoScopeCohort::StandaloneRelease62 {
                    6
                } else {
                    4
                }
            );
            sources
                .require_artifact_bytes(&fs::read(&binding.artifact.path).unwrap())
                .unwrap();
            assert!(
                sources
                    .require_artifact_bytes(b"foreign selected product")
                    .is_err()
            );
            for role in binding.packages.keys() {
                for change in 0..7 {
                    let mut wrong = binding.clone();
                    let package = wrong.packages.get_mut(role).unwrap();
                    match change {
                        0 => package.profile.opt_level = "2".into(),
                        1 => package.profile.debug_assertions = !package.profile.debug_assertions,
                        2 => package.profile.overflow_checks = !package.profile.overflow_checks,
                        3 => package.profile.test = true,
                        4 => package.profile.debuginfo = 1,
                        5 => package.features.push("liteinst2".into()),
                        _ => package.crate_name.push_str("_foreign"),
                    }
                    let reason = if change == 6 {
                        "native source package has a different compiled crate identity"
                    } else {
                        "no-scope build has an unreviewed package/feature/test/profile cohort"
                    };
                    assert_eq!(
                        admission_error(&wrong),
                        reason,
                        "{cohort:?} {role} mutation {change}"
                    );
                }
            }
            let mut missing = binding.clone();
            missing.packages.pop_first();
            assert_eq!(
                admission_error(&missing),
                "no-scope build has an unreviewed package/feature/test/profile cohort"
            );
            let mut extra = binding.clone();
            extra.packages.insert(
                "foreign".into(),
                binding.packages.values().next().unwrap().clone(),
            );
            // This existing extra-key mutation retains the copied crate name,
            // so it first violates the independent key-to-crate identity join.
            assert_eq!(
                admission_error(&extra),
                "native source package has a different compiled crate identity"
            );
            for role in binding.compiler_filenames.keys() {
                let mut missing = binding.clone();
                missing.compiler_filenames.remove(role);
                assert_eq!(
                    admission_error(&missing),
                    "native compiler filename mapping lacks the exact source role set",
                    "{cohort:?} missing filename role {role}"
                );
            }
            let mut keyed = binding.clone();
            keyed.compiler_filenames.insert(
                "rv:reverie-inguest/src/guest/tool_region_keyed.rs".into(),
                b"reverie-inguest/src/guest/tool_region_keyed.rs".to_vec(),
            );
            assert_eq!(
                admission_error(&keyed),
                "native compiler filename mapping lacks the exact source role set"
            );
            let mut missing = binding.clone();
            missing.files.pop_first();
            assert!(admission_error(&missing).contains("complete exact source catalog"));
            let mut extra = binding.clone();
            extra
                .files
                .insert("rv:foreign.rs".into(), binding.artifact.clone());
            assert!(admission_error(&extra).contains("complete exact source catalog"));
            for (role, diagnostic) in [
                ("rv:reverie-liteinst/src/patch_alloc.rs", "reviewed catalog"),
                (
                    "rv:reverie-liteinst-artifact/src/lib.rs",
                    "compiled host qualifier",
                ),
            ] {
                let mut changed = fs::read(&binding.files[role].path).unwrap();
                changed.extend_from_slice(b"\n// changed source\n");
                let replacement = fixture.root.join("changed-source.rs");
                fs::write(&replacement, changed).unwrap();
                let mut wrong = binding.clone();
                wrong
                    .files
                    .insert(role.into(), FileIdentity::read(&replacement).unwrap());
                assert!(admission_error(&wrong).contains(diagnostic));
            }
        }
    }

    #[test]
    fn new_native_routes_refuse_foreign_cohorts_products_and_archives_before_tools() {
        let fixture = AdmissionFixture::new();
        let mut runner =
            Runner::new(fixture.root.join("logs"), Duration::from_secs(5), 1024).unwrap();
        for cohort in [
            NoScopeCohort::DetcoreOrdinary101,
            NoScopeCohort::StandaloneRelease62,
            NoScopeCohort::DetcoreNative101,
        ] {
            let binding = fixture.binding(cohort);
            let sources = VerifiedNativeSources::from_checked_action(&binding).unwrap();
            for route in [
                NoScopeCohort::StandaloneRelease62,
                NoScopeCohort::DetcoreNative101,
            ] {
                let error =
                    qualification_error(route, &binding.artifact.path, &mut runner, &sources);
                if route == cohort {
                    assert!(error.contains("parse ") && error.contains("ELF"), "{error}");
                } else {
                    assert!(error.contains("cohort"), "{error}");
                }
                assert!(
                    runner.commands.is_empty(),
                    "wrong route or non-ELF product started a tool"
                );
            }
            if cohort == NoScopeCohort::DetcoreOrdinary101 {
                continue;
            }
            let error = qualification_error(
                NoScopeCohort::DetcoreOrdinary101,
                &binding.artifact.path,
                &mut runner,
                &sources,
            );
            assert!(error.contains("ordinary OFF cohort"), "{error}");
            let foreign = fixture.root.join("foreign-product");
            fs::copy(&binding.artifact.path, &foreign).unwrap();
            let error = qualification_error(cohort, &foreign, &mut runner, &sources);
            assert!(
                error.contains("not the checked selected product"),
                "{error}"
            );
            assert!(runner.commands.is_empty());
            // Valid archive magic is not an ELF or private-static authority.
            // The source-only factory may bind these bytes; both DSO qualifiers
            // must still refuse before invoking a physical tool.
            let archive = fixture.root.join("private-archive.a");
            fs::write(&archive, b"!<arch>\n").unwrap();
            let mut archived = binding.clone();
            archived.artifact = FileIdentity::read(&archive).unwrap();
            let archive_sources = VerifiedNativeSources::from_checked_action(&archived).unwrap();
            let error = qualification_error(cohort, &archive, &mut runner, &archive_sources);
            assert!(error.contains("parse ") && error.contains("ELF"), "{error}");
            assert!(runner.commands.is_empty());
        }
    }

    #[test]
    fn new_native_source_close_preserves_primary_and_every_changed_source() {
        for cohort in [
            NoScopeCohort::StandaloneRelease62,
            NoScopeCohort::DetcoreNative101,
        ] {
            let fixture = AdmissionFixture::new();
            let binding = fixture.binding(cohort);
            let sources = VerifiedNativeSources::from_checked_action(&binding).unwrap();
            let roles = [
                "rv:reverie-liteinst/src/patch_alloc.rs",
                "rv:reverie-inguest/src/guest/alloc.rs",
            ];
            for role in roles {
                fs::write(&binding.files[role].path, b"changed owned source copy").unwrap();
            }
            fs::write(&binding.artifact.path, b"changed selected product").unwrap();
            let mut runner =
                Runner::new(fixture.root.join("logs"), Duration::from_secs(5), 1024).unwrap();
            let error = qualification_error(cohort, &binding.artifact.path, &mut runner, &sources);
            assert!(error.contains("native source close failed:"), "{error}");
            for label in roles.into_iter().chain(["native selected product"]) {
                assert_eq!(
                    error.matches(label).count(),
                    2,
                    "primary and close lost {label}: {error}"
                );
            }
            assert!(runner.commands.is_empty());
            let error =
                close_sources::<()>(Err("independent physical primary failure".into()), &sources)
                    .unwrap_err();
            assert!(
                error.starts_with(
                    "independent physical primary failure; native source close failed:"
                )
            );
            for label in roles.into_iter().chain(["native selected product"]) {
                assert!(error.contains(label));
            }
            assert!(
                close_sources(Ok(()), &sources)
                    .unwrap_err()
                    .starts_with("native source close failed:")
            );
        }
    }

    #[test]
    fn checked_native_entry_population_route_refuses_cargo130_and_selects_native198() {
        let fixture = AdmissionFixture::new();
        for case in 0..2 {
            let bytes = crate::tests::complete_tls_population_bytes_for_source_controls(case);
            let product = fixture
                .root
                .join(format!("population-source-control-{case}"));
            fs::write(&product, &bytes).unwrap();
            let mut binding = fixture.binding(NoScopeCohort::DetcoreNative101);
            binding.artifact = FileIdentity::read(&product).unwrap();
            let sources = VerifiedNativeSources::from_checked_action(&binding).unwrap();
            assert_eq!(sources.cohort(), NoScopeCohort::DetcoreNative101);
            sources.require_artifact_bytes(&bytes).unwrap();
            let elf = Elf::parse(&bytes).unwrap();
            let selected = selected_no_scope_census_in_source_context(
                &elf,
                &bytes,
                NoScopeCohort::DetcoreNative101,
                SourceContextRef::Native(&sources),
            );
            if case == 0 {
                assert_eq!(
                    selected
                        .err()
                        .expect("native source accepted Cargo130 population"),
                    "complete native TLS population belongs to another source authority route",
                );
            } else {
                let selected = selected.expect("native source refused its complete198 population");
                assert_eq!(selected.version, COMPLETE_NATIVE_POPULATION_VERSION);
                assert_eq!(selected.policy, NATIVE198_CELLS28_POLICY);
                assert!(selected.native_tls_layout100.is_none());
                let Some(CompleteNativeTlsPopulation::Dispatch198Cells28(payload)) =
                    selected.complete_tls_population
                else {
                    panic!("native selection lost its distinct complete198 payload");
                };
                assert_eq!(payload.cells.len(), 28);
                assert_eq!(payload.tls_file_size, 0x100);
                assert_eq!(payload.tls_memory_size, 0x1b1);
                assert_eq!(selected.census.other_tls_getters.len(), 4);
            }
            sources.require_artifact_bytes(&bytes).unwrap();
            sources.verify_unchanged().unwrap();
        }
    }

    #[test]
    fn checked_native_entry_population_route_selects_native1a8_and_closes_sources() {
        // Synthetic complete ELF/provider fixture, with independently retained
        // population bytes. This exercises checked source selection and close,
        // not an owned compiler action or a whole-image qualification positive.
        let fixture = AdmissionFixture::new();
        let bytes = crate::tests::complete_tls_population_bytes_for_source_controls(2);
        let product = fixture.root.join("population-source-control-1a8");
        fs::write(&product, &bytes).unwrap();
        let mut binding = fixture.binding(NoScopeCohort::DetcoreNative101);
        binding.artifact = FileIdentity::read(&product).unwrap();
        let sources = VerifiedNativeSources::from_checked_action(&binding).unwrap();
        assert_eq!(sources.cohort(), NoScopeCohort::DetcoreNative101);
        sources.require_artifact_bytes(&bytes).unwrap();
        let elf = Elf::parse(&bytes).unwrap();
        let selected = selected_no_scope_census_in_source_context(
            &elf,
            &bytes,
            NoScopeCohort::DetcoreNative101,
            SourceContextRef::Native(&sources),
        );
        let selected = close_sources(selected, &sources)
            .expect("checked native source refused its complete1a8 population or close");
        assert_eq!(selected.version, COMPLETE_NATIVE_POPULATION_VERSION);
        assert_eq!(selected.policy, NATIVE1A8_CELLS28_POLICY);
        assert_ne!(selected.policy, NATIVE198_CELLS28_POLICY);
        assert!(selected.native_tls_layout100.is_none());
        let Some(CompleteNativeTlsPopulation::Dispatch1a8Cells28(payload)) =
            selected.complete_tls_population
        else {
            panic!("native selection lost its distinct complete1a8 payload");
        };
        assert_eq!(payload.cells.len(), 28);
        assert_eq!(payload.tls_file_size, 0x100);
        assert_eq!(payload.tls_memory_size, 0x1b0);
        assert_eq!(selected.census.other_tls_getters.len(), 4);

        // Establish the complete positive first; each association refusal then
        // uses the same valid physical bytes with just that association wrong.
        let mut ordinary = fixture.binding(NoScopeCohort::DetcoreOrdinary101);
        ordinary.artifact = binding.artifact.clone();
        let ordinary_sources = VerifiedNativeSources::from_checked_action(&ordinary).unwrap();
        assert_eq!(
            selected_no_scope_census_in_source_context(
                &elf,
                &bytes,
                NoScopeCohort::DetcoreNative101,
                SourceContextRef::Native(&ordinary_sources),
            )
            .err()
            .expect("ordinary source cohort selected explicit-entry1a8 population"),
            "native source cohort does not match the physical qualification route",
        );
        close_sources(Ok(()), &ordinary_sources).unwrap();

        let source_role = "rv:reverie-inguest/src/guest/alloc.rs";
        let mut missing = binding.clone();
        missing.files.remove(source_role).unwrap();
        assert_eq!(
            admission_error(&missing),
            "native source mapping differs from the complete exact source catalog",
        );
        let other_product = crate::tests::complete_tls_population_bytes_for_source_controls(1);
        assert_eq!(
            sources.require_artifact_bytes(&other_product).unwrap_err(),
            "native physical proof uses a different selected product",
        );
        sources.require_artifact_bytes(&bytes).unwrap();
        close_sources(Ok(()), &sources).unwrap();

        // A completed physical selection cannot suppress later source/product
        // changes or turn a primary failure into a successful scope close.
        fs::write(
            &binding.files[source_role].path,
            b"changed owned source copy",
        )
        .unwrap();
        fs::write(&product, b"changed selected product").unwrap();
        let error = close_sources::<()>(
            Err("independent complete1a8 primary failure".into()),
            &sources,
        )
        .unwrap_err();
        assert!(
            error.starts_with(
                "independent complete1a8 primary failure; native source close failed:"
            ),
            "{error}"
        );
        for label in [source_role, "native selected product"] {
            assert_eq!(error.matches(label).count(), 1, "lost {label}: {error}");
        }
    }
}
