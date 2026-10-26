// Copyright (c) Meta Platforms, Inc. and affiliates.
// SPDX-License-Identifier: BSD-2-Clause

//! Owned ordinary build-report capture under real-file transactions.
//!
//! This boundary owns the command and selected output files. Its observations
//! remain data: a successful build report lacks the terminal action, resolved
//! producer path, exact recipe and configured dependency joins needed to create
//! compilation authority. No observation can enter the qualification dispatch.

use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::collections::VecDeque;
use std::fs;
use std::io::Read;
use std::os::unix::fs::MetadataExt;
use std::path::Component;
use std::path::Path;
use std::path::PathBuf;

use serde::Deserialize;
use serde::Serialize;
use sha2::Digest;
use sha2::Sha256;

use super::CommandEvidence;
use super::FileIdentity;
use super::Result;
use super::Runner;
use super::native_inputs;
use super::native_results;
use super::require;

/// Independently selected tool, exact ordinary argv and requested codegen roles.
/// The role/path declaration is not itself proof that an output is codegen.
pub struct BuildRequest {
    pub executable: FileIdentity,
    pub arguments: Vec<String>,
    pub working_directory: PathBuf,
    pub report_path: PathBuf,
    pub outputs: Vec<native_results::ExpectedOutput>,
}

/// Materialized bytes selected by the report's full configured provider path.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SelectedFile {
    pub role: String,
    pub reported_path: PathBuf,
    pub file: FileIdentity,
}

/// Ordinary owned invocation observations, never an opaque source authority.
/// Keeping this form separate also prevents it being a historical Cargo receipt.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BuildObservation {
    pub kind: ObservationKind,
    pub executable: FileIdentity,
    pub command: CommandEvidence,
    pub report_file: FileIdentity,
    /// Trace reported by the owned build, not joined to an independent request token.
    pub trace_id: String,
    pub project_root: PathBuf,
    pub selected_files: Vec<SelectedFile>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ObservationKind {
    #[serde(rename = "reverie-native-owned-build-observation/v1")]
    OwnedBuildReport,
}

fn finish<T>(primary: Result<T>, close: Result<()>) -> Result<T> {
    finish_check(primary, close, "close")
}

fn finish_check<T>(primary: Result<T>, check: Result<()>, phase: &str) -> Result<T> {
    match (primary, check) {
        (Ok(value), Ok(())) => Ok(value),
        (Err(error), Ok(())) => Err(error),
        (Ok(_), Err(error)) => Err(format!("owned build {phase} failed: {error}")),
        (Err(error), Err(check)) => Err(format!("{error}; owned build {phase} failed: {check}")),
    }
}

// FileIdentity is the historical path/digest record. Keep it unchanged, and
// retain the non-serialized open/close identity separately for this transaction.
// The original spelling matters for reports reached through parent symlinks.
#[derive(PartialEq, Eq)]
struct FileStamp {
    device: u64,
    inode: u64,
    size: u64,
    modified: (i64, i64),
    changed: (i64, i64),
}

impl FileStamp {
    fn read(path: &Path) -> Result<Self> {
        let metadata = fs::symlink_metadata(path)
            .map_err(|e| format!("owned file metadata {}: {e}", path.display()))?;
        require(
            metadata.file_type().is_file(),
            format!("owned file is not a regular file: {}", path.display()),
        )?;
        Ok(Self::from_metadata(&metadata))
    }

    fn from_metadata(metadata: &fs::Metadata) -> Self {
        Self {
            device: metadata.dev(),
            inode: metadata.ino(),
            size: metadata.len(),
            modified: (metadata.mtime(), metadata.mtime_nsec()),
            changed: (metadata.ctime(), metadata.ctime_nsec()),
        }
    }
}

#[derive(PartialEq, Eq)]
enum DirectoryEntry {
    Directory { device: u64, inode: u64 },
    Link { target: PathBuf, stamp: FileStamp },
    File { stamp: FileStamp },
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum PathEnd {
    Directory,
    RegularFile,
}

#[derive(PartialEq, Eq)]
pub(super) struct DirectoryGuard {
    requested_path: PathBuf,
    canonical: PathBuf,
    entries: BTreeMap<PathBuf, DirectoryEntry>,
    end: PathEnd,
}

// Match the bounded POSIX walk used by directory-tree custody. A repeated
// link can be a finite lookup (link/../../link), so bound total traversals
// instead of rejecting every repeated identity as a cycle.
const MAX_DIRECTORY_LINKS: usize = 40;
const MAX_DIRECTORY_COMPONENTS: usize = 4096;

impl DirectoryEntry {
    fn read(path: &Path) -> Result<Self> {
        let metadata = fs::symlink_metadata(path)
            .map_err(|e| format!("owned directory {}: {e}", path.display()))?;
        if metadata.file_type().is_symlink() {
            let stamp = FileStamp::from_metadata(&metadata);
            let target = fs::read_link(path).map_err(|e| e.to_string())?;
            let after = fs::symlink_metadata(path).map_err(|e| e.to_string())?;
            require(
                after.file_type().is_symlink() && FileStamp::from_metadata(&after) == stamp,
                format!(
                    "owned directory link changed while reading: {}",
                    path.display()
                ),
            )?;
            require(
                !target.as_os_str().is_empty(),
                "owned directory link is empty",
            )?;
            Ok(Self::Link { target, stamp })
        } else if metadata.is_file() {
            Ok(Self::File {
                stamp: FileStamp::from_metadata(&metadata),
            })
        } else {
            require(
                metadata.is_dir(),
                format!("owned path is not a directory: {}", path.display()),
            )?;
            // Child creation is legitimate; directory timestamps do not
            // participate in the existing namespace-identity contract.
            Ok(Self::Directory {
                device: metadata.dev(),
                inode: metadata.ino(),
            })
        }
    }
}

fn directory_components(path: &Path) -> Result<VecDeque<PathBuf>> {
    let mut parts = VecDeque::new();
    for part in path.components() {
        require(
            !matches!(part, Component::Prefix(_)),
            "unsupported directory prefix",
        )?;
        require(
            parts.len() < MAX_DIRECTORY_COMPONENTS,
            "owned directory component bound exceeded",
        )?;
        parts.push_back(PathBuf::from(part.as_os_str()));
    }
    Ok(parts)
}

impl DirectoryGuard {
    pub(super) fn capture(path: &Path) -> Result<Self> {
        Self::capture_end(path, PathEnd::Directory)
    }

    fn capture_end(path: &Path, end: PathEnd) -> Result<Self> {
        // Anchor the unresolved spelling once. Relative targets below are
        // resolved from the *physical* parent, never from this original path.
        let requested_path = if path.is_absolute() {
            path.to_owned()
        } else {
            std::env::current_dir()
                .map_err(|e| format!("owned directory current path: {e}"))?
                .join(path)
        };
        let mut guard = Self {
            requested_path,
            canonical: PathBuf::from("/"),
            entries: BTreeMap::new(),
            end,
        };
        guard.remember(PathBuf::from("/"), DirectoryEntry::read(Path::new("/"))?)?;
        let mut pending = directory_components(&guard.requested_path)?;
        let mut steps = 0;
        let mut links = 0;
        while let Some(part) = pending.pop_front() {
            steps += 1;
            require(
                steps <= MAX_DIRECTORY_COMPONENTS,
                "owned directory component bound exceeded",
            )?;
            match part
                .components()
                .next()
                .ok_or("empty directory component")?
            {
                Component::RootDir => guard.canonical = PathBuf::from("/"),
                Component::CurDir => {}
                Component::ParentDir => {
                    // Every popped directory remains in the custody ledger.
                    guard.canonical.pop();
                }
                Component::Normal(name) => {
                    let candidate = guard.canonical.join(name);
                    let entry = DirectoryEntry::read(&candidate)?;
                    if let DirectoryEntry::Link { target, .. } = &entry {
                        links += 1;
                        require(
                            links <= MAX_DIRECTORY_LINKS,
                            "owned directory symlink bound exceeded",
                        )?;
                        let mut expanded = directory_components(target)?;
                        require(
                            expanded.len() + pending.len() <= MAX_DIRECTORY_COMPONENTS,
                            "owned directory component bound exceeded",
                        )?;
                        expanded.append(&mut pending);
                        pending = expanded;
                    } else {
                        require(
                            !matches!(entry, DirectoryEntry::File { .. })
                                || (end == PathEnd::RegularFile && pending.is_empty()),
                            format!("owned path is not a directory: {}", candidate.display()),
                        )?;
                        guard.canonical = candidate.clone();
                    }
                    guard.remember(candidate, entry)?;
                }
                Component::Prefix(_) => return Err("unsupported directory prefix".into()),
            }
        }
        require(
            matches!(
                (end, guard.entries.get(&guard.canonical)),
                (PathEnd::Directory, Some(DirectoryEntry::Directory { .. }))
                    | (PathEnd::RegularFile, Some(DirectoryEntry::File { .. }))
            ),
            "owned path has the wrong endpoint kind",
        )?;
        require(
            fs::canonicalize(&guard.requested_path).map_err(|e| e.to_string())? == guard.canonical,
            format!(
                "owned directory resolution changed: {}",
                guard.requested_path.display()
            ),
        )?;
        // Catch changes during the walk as well as later transaction close.
        for (path, entry) in &guard.entries {
            require(
                DirectoryEntry::read(path)? == *entry,
                format!("owned directory entry changed: {}", path.display()),
            )?;
        }
        Ok(guard)
    }

    fn remember(&mut self, path: PathBuf, entry: DirectoryEntry) -> Result<()> {
        if let Some(previous) = self.entries.get(&path) {
            require(
                *previous == entry,
                format!(
                    "owned directory entry changed during traversal: {}",
                    path.display()
                ),
            )
        } else {
            self.entries.insert(path, entry);
            Ok(())
        }
    }

    pub(super) fn verify(&self) -> Result<()> {
        require(
            Self::capture_end(&self.requested_path, self.end)? == *self,
            format!(
                "owned directory identity changed: {}",
                self.requested_path.display()
            ),
        )
    }
}

pub(super) struct OpenedFile {
    requested_path: PathBuf,
    pub(super) file: FileIdentity,
    stamp: FileStamp,
    parent: DirectoryGuard,
}

impl OpenedFile {
    pub(super) fn capture(path: &Path, expected: &FileIdentity) -> Result<Self> {
        let (opened, ()) = Self::read(path, Self::hash)?;
        require(
            opened.file == *expected,
            format!("owned file content/path changed: {}", path.display()),
        )?;
        Ok(opened)
    }

    fn hash(file: &mut fs::File) -> Result<(String, ())> {
        let size = file.metadata().map_err(|e| e.to_string())?.len();
        let mut limited = file.take(size.saturating_add(1));
        let mut hash = Sha256::new();
        let mut buffer = [0u8; 65536];
        let mut total = 0;
        loop {
            let count = limited.read(&mut buffer).map_err(|e| e.to_string())?;
            if count == 0 {
                break;
            }
            total += count as u64;
            hash.update(&buffer[..count]);
        }
        require(total == size, "owned file size changed while hashing")?;
        Ok((format!("{:x}", hash.finalize()), ()))
    }

    fn read<T>(
        path: &Path,
        read: impl FnOnce(&mut fs::File) -> Result<(String, T)>,
    ) -> Result<(Self, T)> {
        let parent = DirectoryGuard::capture(path.parent().ok_or("owned file has no parent")?)?;
        let stamp = FileStamp::read(path)?;
        let canonical = fs::canonicalize(path).map_err(|e| e.to_string())?;
        let mut file = fs::File::open(path).map_err(|e| e.to_string())?;
        let metadata = file.metadata().map_err(|e| e.to_string())?;
        require(
            metadata.is_file() && FileStamp::from_metadata(&metadata) == stamp,
            format!("owned file changed while opening: {}", path.display()),
        )?;
        let (sha256, value) = read(&mut file)?;
        let metadata = file.metadata().map_err(|e| e.to_string())?;
        require(
            FileStamp::from_metadata(&metadata) == stamp
                && stamp == FileStamp::read(path)?
                && fs::canonicalize(path).map_err(|e| e.to_string())? == canonical,
            format!("owned file changed while reading: {}", path.display()),
        )?;
        parent.verify()?;
        Ok((
            Self {
                requested_path: path.to_owned(),
                file: FileIdentity {
                    path: canonical,
                    sha256,
                },
                stamp,
                parent,
            },
            value,
        ))
    }

    pub(super) fn report(path: &Path) -> Result<(Self, Vec<u8>)> {
        Self::read(path, |file| {
            require(
                file.metadata().map_err(|e| e.to_string())?.len()
                    <= native_results::MAX_REPORT_BYTES as u64,
                "native report is not a bounded regular file",
            )?;
            let mut bytes = Vec::new();
            file.take(native_results::MAX_REPORT_BYTES as u64 + 1)
                .read_to_end(&mut bytes)
                .map_err(|e| e.to_string())?;
            require(
                bytes.len() <= native_results::MAX_REPORT_BYTES,
                "native report grew beyond its byte bound",
            )?;
            Ok((super::sha256(&bytes), bytes))
        })
    }

    pub(super) fn verify(&self) -> Result<()> {
        self.parent.verify()?;
        require(
            FileStamp::read(&self.requested_path)? == self.stamp,
            format!(
                "owned file identity changed: {}",
                self.requested_path.display()
            ),
        )?;
        let current = Self::capture(&self.requested_path, &self.file)?;
        require(
            current.stamp == self.stamp && current.parent == self.parent,
            format!(
                "owned file identity changed: {}",
                self.requested_path.display()
            ),
        )
    }
}

// Only the native action's explicitly associated, independently pinned linker
// inputs use this route. Strict reports, sources, tools and products continue to
// use OpenedFile directly. No serialized identity contains this custody.
pub(super) struct LinkedInputFile {
    namespace: DirectoryGuard,
    opened: OpenedFile,
    inventory_path: PathBuf,
}

// Observations only: the action adapter must separately bind the expected
// input category, logical role, endpoint pins and original producer record.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ExternalLinkWitness {
    pub(super) requested: PathBuf,
    pub(super) link: PathBuf,
    pub(super) target: PathBuf,
    pub(super) remaining: PathBuf,
}

impl LinkedInputFile {
    pub(super) fn capture(path: &Path) -> Result<Self> {
        Self::read(path, OpenedFile::hash).map(|(file, ())| file)
    }

    fn read<T>(
        path: &Path,
        read: impl FnOnce(&mut fs::File) -> Result<(String, T)>,
    ) -> Result<(Self, T)> {
        let namespace = DirectoryGuard::capture_end(path, PathEnd::RegularFile)?;
        Self::read_captured(namespace, read)
    }

    fn read_captured<T>(
        namespace: DirectoryGuard,
        read: impl FnOnce(&mut fs::File) -> Result<(String, T)>,
    ) -> Result<(Self, T)> {
        let primary = (|| {
            // Bind a regular leaf's requested inventory spelling separately
            // from the canonical endpoint used by physical and source checks.
            // This does not establish that the executor recorded that row: the
            // complete inventory join must still refuse missing observations.
            // A leaf symlink is not itself a regular row.
            let inventory_path = if fs::symlink_metadata(&namespace.requested_path)
                .map_err(|e| format!("linked input classification: {e}"))?
                .file_type()
                .is_file()
            {
                namespace.requested_path.clone()
            } else {
                namespace.canonical.clone()
            };
            let (opened, value) = OpenedFile::read(&namespace.canonical, read)?;
            Ok((opened, inventory_path, value))
        })();
        let (opened, inventory_path, value) =
            finish_check(primary, namespace.verify(), "linked input namespace")?;
        Ok((
            Self {
                namespace,
                opened,
                inventory_path,
            },
            value,
        ))
    }

    pub(super) fn file(&self) -> &FileIdentity {
        &self.opened.file
    }

    pub(super) fn inventory_path(&self) -> &Path {
        &self.inventory_path
    }

    pub(super) fn external_witness(&self, project_root: &Path) -> Result<ExternalLinkWitness> {
        let primary = (|| {
            require(
                project_root.is_absolute(),
                "external input project root must be absolute",
            )?;
            let root = DirectoryGuard::capture(project_root)?;
            let witness = self.external_witness_in_root(&root);
            finish_check(witness, root.verify(), "external input project root")
        })();
        finish_check(primary, self.verify(), "external input custody")
    }

    fn external_witness_in_root(&self, root: &DirectoryGuard) -> Result<ExternalLinkWitness> {
        // Reject normalization rather than changing the declared input spelling.
        // In particular, lexical '..' removal is not valid across symlinks.
        let normal_absolute = |path: &Path| {
            path.is_absolute()
                && path
                    .components()
                    .all(|part| matches!(part, Component::RootDir | Component::Normal(_)))
                && path.components().collect::<PathBuf>().as_os_str() == path.as_os_str()
        };
        require(
            normal_absolute(&root.requested_path)
                && normal_absolute(&self.namespace.requested_path),
            "external input requires normal project and requested path spellings",
        )?;
        let requested = self
            .namespace
            .requested_path
            .strip_prefix(&root.requested_path)
            .map_err(|_| "external input is outside the declared project root".to_owned())?;
        require(
            !requested.as_os_str().is_empty(),
            "external input cannot be the project root",
        )?;
        // The root is part of the originally captured namespace, not a freshly
        // chosen canonical prefix. Its entries remain held for the full input
        // lifetime even though this additional root view is verified here.
        require(
            root.entries
                .iter()
                .all(|(path, entry)| self.namespace.entries.get(path) == Some(entry)),
            "external input project root differs from captured namespace",
        )?;
        let mut physical = root.canonical.clone();
        let mut logical = PathBuf::new();
        let mut remaining = requested.components();
        while let Some(part) = remaining.next() {
            let Component::Normal(name) = part else {
                return Err("external input has a non-normal relative component".into());
            };
            physical.push(name);
            logical.push(name);
            let entry = self
                .namespace
                .entries
                .get(&physical)
                .ok_or("external input component is absent from captured namespace")?;
            if let DirectoryEntry::Link { target, .. } = entry {
                // Stop at the first input link. Links used to reach the project
                // root are excluded, and a relative first link cannot borrow a
                // later absolute link's classification.
                require(
                    target.is_absolute(),
                    "external input first project-relative link is not absolute",
                )?;
                return Ok(ExternalLinkWitness {
                    requested: requested.to_owned(),
                    link: logical,
                    target: target.clone(),
                    remaining: remaining.collect(),
                });
            }
        }
        Err("external input has no project-relative symlink".into())
    }

    pub(super) fn verify(&self) -> Result<()> {
        finish_check(
            self.opened.verify(),
            self.namespace.verify(),
            "linked input namespace",
        )
    }
}

fn request_report(request: &BuildRequest, root: &Path) -> Result<()> {
    require(
        request.arguments.first().map(String::as_str) == Some("build"),
        "owned native report requires an ordinary build invocation",
    )?;
    let report = request
        .report_path
        .to_str()
        .ok_or("native report path is not UTF-8")?;
    require(
        request.report_path.is_absolute()
            && request.report_path.components().all(|part| {
                matches!(
                    part,
                    std::path::Component::RootDir | std::path::Component::Normal(_)
                )
            }),
        "native report path must be an absolute path without traversal",
    )?;
    let exact_report = format!("--build-report={report}");
    let report_options: Vec<_> = request
        .arguments
        .iter()
        .filter(|arg| arg.as_str() == "--build-report" || arg.starts_with("--build-report="))
        .collect();
    require(
        report_options == [&exact_report],
        "native build must write the one exact declared report path",
    )?;
    let materializations: Vec<_> = request
        .arguments
        .iter()
        .filter(|arg| {
            arg.as_str() == "--materializations" || arg.starts_with("--materializations=")
        })
        .collect();
    require(
        materializations.len() == 1 && materializations[0] == "--materializations=all",
        "native build requires explicit complete output materialization",
    )?;
    require(
        fs::canonicalize(&request.working_directory).map_err(|e| e.to_string())? == root,
        "native build working directory differs from the guarded project root",
    )?;
    match fs::symlink_metadata(&request.report_path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!("native report metadata: {error}")),
        Ok(_) => Err("native report destination already exists".into()),
    }
}

/// Run the ordinary producer and inspect its selected bytes within source and
/// output before/close checks. Every result path closes the input transaction;
/// a closure failure and a concurrent close failure are both retained.
///
/// `expected_inputs` must contain only pre-build inputs; generated outputs are
/// opened after the owned command succeeds. The caller owns writer exclusion.
/// The callback receives data, not a verified compilation or execution token.
/// File identity and original report-path resolution are rechecked at close;
/// this is not a filesystem lock or protection against namespace restoration
/// between observations. Panics are not caught.
/// In particular, this function does not validate terminal action events,
/// recipe/source/dependency attribution, or logical-artifact path resolution.
pub fn with_owned_build_report<T>(
    root: &Path,
    expected_inputs: &native_inputs::Inputs,
    observed_inputs: &native_inputs::Inputs,
    request: &BuildRequest,
    runner: &mut Runner,
    action: impl FnOnce(&BuildObservation, &mut Runner) -> Result<T>,
) -> Result<T> {
    require(
        expected_inputs.outputs.is_empty() && observed_inputs.outputs.is_empty(),
        "owned native build input transaction cannot contain preselected outputs",
    )?;
    let canonical_root = fs::canonicalize(root).map_err(|e| e.to_string())?;
    request_report(request, &canonical_root)?;
    request.executable.verify()?;
    native_inputs::with_inputs(root, expected_inputs, observed_inputs, || {
        // These destinations must already exist. Hold their identity before
        // the command can create evidence; child creation changes timestamps,
        // which deliberately are not part of directory identity.
        let directories = [
            DirectoryGuard::capture(
                request
                    .report_path
                    .parent()
                    .ok_or("native report has no parent")?,
            )?,
            DirectoryGuard::capture(&runner.logs)?,
        ];
        let mut opened = vec![OpenedFile::capture(
            &request.executable.path,
            &request.executable,
        )?];
        let primary = (|| {
            // Check once more after opening the source transaction, before the
            // producer can create anything. No existing record is an input here.
            request_report(request, &canonical_root)?;
            opened[0].verify()?;
            let program = request
                .executable
                .path
                .to_str()
                .ok_or("native build executable path is not UTF-8")?;
            let before = runner.commands.len();
            let result = runner.run(program, &request.arguments, &request.working_directory);
            let capture = (|| {
                if runner.commands.len() == before + 1 {
                    for file in [
                        &runner.commands[before].stdout,
                        &runner.commands[before].stderr,
                    ] {
                        opened.push(OpenedFile::capture(&file.path, file)?);
                    }
                }
                Ok(())
            })();
            finish_check(result, capture, "command evidence capture")?;
            require(
                runner.commands.len() == before + 1,
                "owned native command evidence is absent or ambiguous",
            )?;
            let command = runner.commands[before].clone();
            require(
                command.status == 0
                    && command.program == program
                    && command.arguments == request.arguments,
                "native command result differs from the owned invocation",
            )?;
            let (report_guard, report_bytes) = OpenedFile::report(&request.report_path)?;
            let report_file = report_guard.file.clone();
            opened.push(report_guard);
            let report = native_results::parse_build_report(&report_bytes)?;
            // Ordinary build invocation supplies no independently expected trace.
            // Keep the reported trace as observation data; join only the independently
            // requested root and complete configured provider/output groups here.
            native_results::join_build_report_outputs(&report, &canonical_root, &request.outputs)?;
            let mut selected_files = Vec::new();
            let mut outputs = Vec::new();
            let mut seen = BTreeSet::new();
            for output in &request.outputs {
                for (index, path) in output.paths.iter().enumerate() {
                    let relative = PathBuf::from(path);
                    require(
                        relative
                            .components()
                            .all(|part| matches!(part, std::path::Component::Normal(_)))
                            && !relative.as_os_str().is_empty()
                            && seen.insert(relative.clone()),
                        "native selected output path is empty, aliased or duplicated",
                    )?;
                    let file = FileIdentity::read(&canonical_root.join(&relative))?;
                    opened.push(OpenedFile::capture(&canonical_root.join(&relative), &file)?);
                    require(
                        file.path.starts_with(&canonical_root),
                        "native selected output escapes the guarded project root",
                    )?;
                    selected_files.push(SelectedFile {
                        role: output.role.clone(),
                        reported_path: relative.clone(),
                        file: file.clone(),
                    });
                    // The transaction key is deliberately independent of any
                    // filename suffix, target spelling or asserted crate kind.
                    outputs.push(native_inputs::Output {
                        role: format!("{}:{index}", output.role),
                        selected: relative,
                        staged: None,
                        sha256: file.sha256,
                    });
                }
            }
            let observation = BuildObservation {
                kind: ObservationKind::OwnedBuildReport,
                executable: request.executable.clone(),
                command,
                report_file,
                trace_id: report.trace_id,
                project_root: canonical_root.clone(),
                selected_files,
            };
            let output_inputs = native_inputs::Inputs {
                configuration: expected_inputs.configuration.clone(),
                inputs: Vec::new(),
                dependencies: Vec::new(),
                outputs,
            };
            native_inputs::with_inputs(&canonical_root, &output_inputs, &output_inputs, || {
                action(&observation, runner)
            })
        })();
        let errors: Vec<_> = opened
            .iter()
            .filter_map(|file| file.verify().err())
            .chain(
                directories
                    .iter()
                    .filter_map(|directory| directory.verify().err()),
            )
            .collect();
        let close = if errors.is_empty() {
            Ok(())
        } else {
            Err(errors.join("; "))
        };
        finish(primary, close)
    })
}

#[cfg(test)]
mod directory_chain_tests {
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::symlink;
    use std::sync::atomic::AtomicUsize;
    use std::sync::atomic::Ordering;

    use super::*;

    struct Fixture(PathBuf);

    impl Fixture {
        fn new() -> Self {
            static NEXT: AtomicUsize = AtomicUsize::new(0);
            let root = std::env::temp_dir().join(format!(
                "reverie-directory-chain-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&root).unwrap();
            fs::create_dir_all(root.join("real/11.2.0")).unwrap();
            fs::write(root.join("real/11.2.0/input"), b"pinned input").unwrap();
            symlink("11.2.0", root.join("real/trunk")).unwrap();
            symlink(root.join("real"), root.join("lib")).unwrap();
            Self(root)
        }

        fn input(&self) -> PathBuf {
            self.0.join("lib/trunk/input")
        }

        fn replace_link(&self, name: &str, target: &Path) {
            let path = self.0.join(name);
            fs::rename(&path, path.with_extension("retained")).unwrap();
            symlink(target, path).unwrap();
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.0).unwrap();
        }
    }

    #[test]
    fn absolute_parent_then_relative_link_keeps_both_links_and_file_custody() {
        let f = Fixture::new();
        let path = f.input();
        let (opened, bytes) = OpenedFile::report(&path).unwrap();
        assert_eq!(bytes, b"pinned input");
        assert_eq!(opened.requested_path, path);
        assert_eq!(opened.file.path, f.0.join("real/11.2.0/input"));
        for name in ["lib", "real/trunk"] {
            assert!(matches!(
                opened.parent.entries.get(&f.0.join(name)),
                Some(DirectoryEntry::Link { .. })
            ));
        }
        fs::write(f.0.join("real/11.2.0/sibling"), b"legitimate output").unwrap();
        opened.verify().unwrap();
    }

    #[test]
    fn nested_relative_and_absolute_targets_preserve_raw_spellings() {
        let f = Fixture::new();
        fs::create_dir(f.0.join("links")).unwrap();
        symlink("../lib/trunk", f.0.join("links/relative")).unwrap();
        symlink(f.0.join("links/relative"), f.0.join("absolute")).unwrap();
        let guard = DirectoryGuard::capture(&f.0.join("absolute")).unwrap();
        assert_eq!(guard.canonical, f.0.join("real/11.2.0"));
        let Some(DirectoryEntry::Link { target, .. }) =
            guard.entries.get(&f.0.join("links/relative"))
        else {
            panic!("missing intermediate link")
        };
        assert_eq!(target, Path::new("../lib/trunk"));
        guard.verify().unwrap();
    }

    #[test]
    fn parent_components_follow_resolved_target_and_keep_popped_directory() {
        let f = Fixture::new();
        fs::create_dir(f.0.join("real/sibling")).unwrap();
        let guard = DirectoryGuard::capture(&f.0.join("lib/trunk/../sibling")).unwrap();
        assert_eq!(guard.canonical, f.0.join("real/sibling"));
        assert!(guard.entries.contains_key(&f.0.join("real/11.2.0")));
        fs::rename(f.0.join("real/11.2.0"), f.0.join("retained")).unwrap();
        fs::create_dir(f.0.join("real/11.2.0")).unwrap();
        assert_eq!(
            fs::canonicalize(&guard.requested_path).unwrap(),
            guard.canonical
        );
        assert!(guard.verify().unwrap_err().contains("identity changed"));
    }

    #[test]
    fn finite_repeated_link_and_aliases_are_bound() {
        let f = Fixture::new();
        let repeated = f.0.join("lib/trunk/../../lib/trunk");
        let a = DirectoryGuard::capture(&repeated).unwrap();
        let b = DirectoryGuard::capture(&f.0.join("real/trunk")).unwrap();
        assert_eq!(a.canonical, b.canonical);
        assert_ne!(a.requested_path, b.requested_path);
        a.verify().unwrap();
        b.verify().unwrap();
        f.replace_link("lib", &f.0.join("real"));
        assert!(a.verify().is_err());
        b.verify().unwrap();
    }

    #[test]
    fn same_target_new_intermediate_link_fails_with_original_file_intact() {
        let f = Fixture::new();
        let (opened, _) = OpenedFile::report(&f.input()).unwrap();
        f.replace_link("real/trunk", Path::new("11.2.0"));
        opened.file.verify().unwrap();
        assert!(opened.verify().unwrap_err().contains("identity changed"));
    }

    #[test]
    fn changed_intermediate_target_fails_even_with_same_endpoint() {
        let f = Fixture::new();
        symlink("11.2.0", f.0.join("real/other")).unwrap();
        let (opened, _) = OpenedFile::report(&f.input()).unwrap();
        f.replace_link("real/trunk", Path::new("other"));
        assert_eq!(fs::canonicalize(f.input()).unwrap(), opened.file.path);
        assert!(opened.verify().is_err());
    }

    #[test]
    fn same_file_cannot_hide_replaced_target_directory() {
        let f = Fixture::new();
        fs::create_dir(f.0.join("replacement")).unwrap();
        fs::hard_link(f.input(), f.0.join("replacement/input")).unwrap();
        let (opened, _) = OpenedFile::report(&f.input()).unwrap();
        fs::rename(f.0.join("real/11.2.0"), f.0.join("retained")).unwrap();
        fs::rename(f.0.join("replacement"), f.0.join("real/11.2.0")).unwrap();
        assert!(FileStamp::read(&f.input()).unwrap() == opened.stamp);
        opened.file.verify().unwrap();
        assert!(opened.verify().is_err());
    }

    #[test]
    fn intermediate_link_replacement_during_file_read_refuses_return() {
        let f = Fixture::new();
        let result = OpenedFile::read(&f.input(), |file| {
            let mut bytes = Vec::new();
            file.read_to_end(&mut bytes).unwrap();
            f.replace_link("real/trunk", Path::new("11.2.0"));
            Ok((super::super::sha256(&bytes), ()))
        });
        assert!(result.err().unwrap().contains("identity changed"));
    }

    #[test]
    fn primary_failure_and_changed_intermediate_link_are_both_preserved() {
        let f = Fixture::new();
        let (opened, _) = OpenedFile::report(&f.input()).unwrap();
        f.replace_link("real/trunk", Path::new("11.2.0"));
        let error = finish(Err::<(), _>("primary refusal".into()), opened.verify()).unwrap_err();
        assert!(error.contains("primary refusal"));
        assert!(error.contains("owned build close failed"));
        assert!(error.contains("identity changed"));
    }

    #[test]
    fn dangling_nondirectory_and_missing_parent_components_refuse() {
        let f = Fixture::new();
        symlink("missing", f.0.join("dangling")).unwrap();
        symlink("real/11.2.0/input", f.0.join("file-link")).unwrap();
        for name in [
            "dangling",
            "file-link",
            "file-link/..",
            "missing/..",
            "real/11.2.0/input/..",
        ] {
            assert!(DirectoryGuard::capture(&f.0.join(name)).is_err(), "{name}");
        }
    }

    #[test]
    fn cycles_and_excess_links_refuse_but_forty_links_work() {
        let f = Fixture::new();
        symlink("self", f.0.join("self")).unwrap();
        symlink("cycle-b", f.0.join("cycle-a")).unwrap();
        symlink("cycle-a", f.0.join("cycle-b")).unwrap();
        for name in ["self", "cycle-a"] {
            assert!(
                DirectoryGuard::capture(&f.0.join(name))
                    .err()
                    .unwrap()
                    .contains("symlink bound")
            );
        }
        for n in (0..41).rev() {
            let target = if n == 40 {
                "real".to_owned()
            } else {
                format!("hop-{}", n + 1)
            };
            symlink(target, f.0.join(format!("hop-{n}"))).unwrap();
        }
        DirectoryGuard::capture(&f.0.join("hop-1"))
            .unwrap()
            .verify()
            .unwrap();
        assert!(
            DirectoryGuard::capture(&f.0.join("hop-0"))
                .err()
                .unwrap()
                .contains("symlink bound")
        );
    }

    #[test]
    fn root_parent_and_component_bounds_preserve_filesystem_semantics() {
        let guard = DirectoryGuard::capture(Path::new("/../../.")).unwrap();
        assert_eq!(guard.canonical, Path::new("/"));
        guard.verify().unwrap();
        let path: PathBuf = std::iter::repeat_n("name", MAX_DIRECTORY_COMPONENTS + 1).collect();
        assert!(
            directory_components(&path)
                .unwrap_err()
                .contains("component bound")
        );
    }

    fn leaf_alias(f: &Fixture) -> PathBuf {
        symlink("input", f.0.join("real/11.2.0/lib.so")).unwrap();
        f.0.join("lib/trunk/lib.so")
    }

    #[test]
    fn linked_leaf_direct_relative_absolute_and_multihop_keep_requested_paths() {
        let f = Fixture::new();
        let alias = leaf_alias(&f);
        symlink(&alias, f.0.join("absolute")).unwrap();
        symlink("absolute", f.0.join("relative")).unwrap();
        let expected = FileIdentity::read(&f.input()).unwrap();
        for path in [f.input(), alias, f.0.join("absolute"), f.0.join("relative")] {
            let guard = LinkedInputFile::capture(&path).unwrap();
            assert_eq!(guard.namespace.requested_path, path);
            assert_eq!(guard.file(), &expected);
            guard.verify().unwrap();
        }
    }

    #[test]
    fn linked_regular_leaf_retains_parent_alias_spelling_separately() {
        let f = Fixture::new();
        let requested = f.0.join("lib/trunk/input");
        let guard = LinkedInputFile::capture(&requested).unwrap();
        assert_eq!(guard.inventory_path(), requested);
        assert_eq!(guard.file(), &FileIdentity::read(&f.input()).unwrap());
        assert_ne!(guard.inventory_path(), guard.file().path);
        guard.file().verify().unwrap();
        guard.verify().unwrap();
    }

    #[test]
    fn linked_classification_failure_preserves_acquired_namespace_close() {
        let f = Fixture::new();
        let requested = f.0.join("lib/trunk/input");
        let namespace = DirectoryGuard::capture_end(&requested, PathEnd::RegularFile).unwrap();
        fs::remove_file(f.0.join("lib")).unwrap();
        let error = LinkedInputFile::read_captured(namespace, OpenedFile::hash)
            .err()
            .unwrap();
        assert!(error.contains("linked input classification"));
        assert!(error.contains("linked input namespace"));
    }

    #[test]
    fn external_witness_binds_parent_alias_regular_and_leaf_alias_inputs() {
        let f = Fixture::new();
        let alias = leaf_alias(&f);
        for path in [f.input(), alias] {
            let guard = LinkedInputFile::capture(&path).unwrap();
            let witness = guard.external_witness(&f.0).unwrap();
            assert_eq!(witness.requested, path.strip_prefix(&f.0).unwrap());
            assert_eq!(witness.link, Path::new("lib"));
            assert_eq!(witness.target, f.0.join("real"));
            assert_eq!(
                witness.remaining,
                witness.requested.strip_prefix("lib").unwrap()
            );
            assert_eq!(guard.file(), &FileIdentity::read(&f.input()).unwrap());
            let bytes = serde_json::to_vec(&witness).unwrap();
            assert_eq!(
                serde_json::from_slice::<ExternalLinkWitness>(&bytes).unwrap(),
                witness
            );
            guard.verify().unwrap();
        }
    }

    #[test]
    fn external_witness_absolute_target_can_be_inside_or_outside_project() {
        for inside in [false, true] {
            let f = Fixture::new();
            let project = f.0.join("project");
            fs::create_dir(&project).unwrap();
            let target = if inside {
                let target = project.join("package");
                fs::create_dir(&target).unwrap();
                fs::write(target.join("input"), b"pinned input").unwrap();
                target
            } else {
                f.0.join("real/11.2.0")
            };
            symlink(&target, project.join("package-link")).unwrap();
            let guard = LinkedInputFile::capture(&project.join("package-link/input")).unwrap();
            let witness = guard.external_witness(&project).unwrap();
            assert_eq!(witness.requested, Path::new("package-link/input"));
            assert_eq!(witness.link, Path::new("package-link"));
            assert_eq!(witness.target, target);
            assert_eq!(witness.remaining, Path::new("input"));
            assert_eq!(guard.file().path.starts_with(&project), inside);
            guard.verify().unwrap();
        }
    }

    #[test]
    fn external_witness_excludes_host_prefix_links_but_allows_an_aliased_root() {
        let f = Fixture::new();
        let project = f.0.join("project");
        let root_alias = f.0.join("project-alias");
        fs::create_dir(&project).unwrap();
        fs::write(project.join("input"), b"pinned input").unwrap();
        symlink(&project, &root_alias).unwrap();
        let regular = LinkedInputFile::capture(&root_alias.join("input")).unwrap();
        assert!(
            regular
                .external_witness(&root_alias)
                .unwrap_err()
                .contains("no project-relative symlink")
        );
        regular.verify().unwrap();
        symlink(f.0.join("real"), project.join("package")).unwrap();
        let linked = LinkedInputFile::capture(&root_alias.join("package/trunk/input")).unwrap();
        let witness = linked.external_witness(&root_alias).unwrap();
        assert_eq!(witness.link, Path::new("package"));
        assert_eq!(witness.requested, Path::new("package/trunk/input"));
        assert_eq!(witness.target, f.0.join("real"));
        linked.verify().unwrap();
    }

    #[test]
    fn external_witness_refuses_relative_first_non_normal_and_outside_paths() {
        let f = Fixture::new();
        symlink("lib", f.0.join("relative-first")).unwrap();
        for path in [
            f.0.join("relative-first/trunk/input"),
            f.0.join("real/trunk/input"),
        ] {
            let guard = LinkedInputFile::capture(&path).unwrap();
            assert!(
                guard
                    .external_witness(&f.0)
                    .unwrap_err()
                    .contains("not absolute")
            );
            guard.verify().unwrap();
        }
        for path in [
            f.0.join("lib/../real/trunk/input"),
            f.0.join("lib/./trunk/input"),
            f.0.join("lib//trunk/input"),
        ] {
            let guard = LinkedInputFile::capture(&path).unwrap();
            assert!(
                guard
                    .external_witness(&f.0)
                    .unwrap_err()
                    .contains("normal project")
            );
            guard.verify().unwrap();
        }
        let guard = LinkedInputFile::capture(&f.input()).unwrap();
        assert!(
            guard
                .external_witness(&f.0.join("real"))
                .unwrap_err()
                .contains("outside")
        );
        assert!(
            guard
                .external_witness(&f.0.join("real/.."))
                .unwrap_err()
                .contains("normal project")
        );
        guard.verify().unwrap();
    }

    #[test]
    fn external_witness_retains_suffix_and_endpoint_identity_custody() {
        for replace_endpoint in [false, true] {
            let f = Fixture::new();
            let guard = LinkedInputFile::capture(&f.input()).unwrap();
            guard.external_witness(&f.0).unwrap();
            if replace_endpoint {
                fs::rename(f.input(), f.0.join("old-input")).unwrap();
                fs::write(f.input(), b"pinned input").unwrap();
            } else {
                f.replace_link("real/trunk", Path::new("11.2.0"));
            }
            guard.file().verify().unwrap();
            let error = guard.external_witness(&f.0).unwrap_err();
            assert!(error.contains("external input custody"));
            assert!(error.contains("identity changed"));
        }
    }

    #[test]
    fn external_witness_preserves_primary_and_root_or_namespace_close_failures() {
        let f = Fixture::new();
        let guard = LinkedInputFile::capture(&f.input()).unwrap();
        f.replace_link("lib", &f.0.join("real"));
        let error = guard
            .external_witness(Path::new("relative-root"))
            .unwrap_err();
        assert!(error.contains("project root must be absolute"));
        assert!(error.contains("external input custody"));
        assert!(error.contains("identity changed"));

        let project = f.0.join("project");
        fs::create_dir(&project).unwrap();
        symlink(f.0.join("real"), project.join("lib")).unwrap();
        let guard = LinkedInputFile::capture(&project.join("lib/trunk/input")).unwrap();
        fs::rename(&project, f.0.join("old-project")).unwrap();
        fs::create_dir(&project).unwrap();
        symlink(f.0.join("real"), project.join("lib")).unwrap();
        guard.file().verify().unwrap();
        let error = guard.external_witness(&project).unwrap_err();
        assert!(error.contains("project root differs from captured namespace"));
        assert!(error.contains("external input custody"));
        assert!(error.contains("identity changed"));
    }

    #[test]
    fn linked_leaf_same_content_retarget_and_same_target_replacement_refuse() {
        for same_target in [false, true] {
            let f = Fixture::new();
            let alias = leaf_alias(&f);
            fs::write(f.0.join("real/11.2.0/other"), b"pinned input").unwrap();
            let guard = LinkedInputFile::capture(&alias).unwrap();
            f.replace_link(
                "real/11.2.0/lib.so",
                Path::new(if same_target { "input" } else { "other" }),
            );
            guard.file().verify().unwrap();
            assert!(guard.verify().is_err());
        }
    }

    #[test]
    fn linked_leaf_same_bytes_target_replacement_and_mutation_refuse() {
        for replace in [false, true] {
            let f = Fixture::new();
            let alias = leaf_alias(&f);
            let guard = LinkedInputFile::capture(&alias).unwrap();
            if replace {
                fs::rename(f.input(), f.0.join("retained-input")).unwrap();
                fs::write(f.input(), b"pinned input").unwrap();
                guard.file().verify().unwrap();
            } else {
                fs::write(f.input(), b"changed input").unwrap();
            }
            assert!(guard.verify().is_err());
        }
    }

    #[test]
    fn linked_leaf_intermediate_link_and_parent_replacements_refuse() {
        for replace_directory in [false, true] {
            let f = Fixture::new();
            let alias = leaf_alias(&f);
            symlink(&alias, f.0.join("outer")).unwrap();
            if replace_directory {
                fs::create_dir_all(f.0.join("replacement/11.2.0")).unwrap();
                fs::hard_link(f.input(), f.0.join("replacement/11.2.0/input")).unwrap();
                symlink("11.2.0", f.0.join("replacement/trunk")).unwrap();
                symlink("input", f.0.join("replacement/11.2.0/lib.so")).unwrap();
            }
            let guard = LinkedInputFile::capture(&f.0.join("outer")).unwrap();
            if replace_directory {
                fs::rename(f.0.join("real"), f.0.join("retained-real")).unwrap();
                fs::rename(f.0.join("replacement"), f.0.join("real")).unwrap();
            } else {
                f.replace_link("real/11.2.0/lib.so", Path::new("input"));
            }
            guard.file().verify().unwrap();
            assert!(guard.verify().is_err());
        }
    }

    #[test]
    fn linked_leaf_read_checks_descriptor_and_alias_before_return() {
        let f = Fixture::new();
        let alias = leaf_alias(&f);
        let result = LinkedInputFile::read(&alias, |file| {
            let (hash, ()) = OpenedFile::hash(file)?;
            f.replace_link("real/11.2.0/lib.so", Path::new("input"));
            Ok((hash, ()))
        });
        assert!(result.err().unwrap().contains("linked input namespace"));
    }

    #[test]
    fn linked_leaf_primary_read_error_keeps_namespace_close_failure() {
        let f = Fixture::new();
        let alias = leaf_alias(&f);
        let result = LinkedInputFile::read(&alias, |_| {
            f.replace_link("real/11.2.0/lib.so", Path::new("input"));
            Err::<(String, ()), _>("primary read refusal".into())
        });
        let error = result.err().unwrap();
        assert!(error.contains("primary read refusal"));
        assert!(error.contains("linked input namespace"));
        assert!(error.contains("identity changed"));
    }

    #[test]
    fn linked_leaf_cycles_dangling_nondirectory_and_nonregular_refuse() {
        let f = Fixture::new();
        let fifo = std::ffi::CString::new(f.0.join("fifo").as_os_str().as_bytes()).unwrap();
        // A FIFO must be refused before opening, so this control cannot block.
        assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
        for (name, target) in [
            ("dangling", "missing"),
            ("cycle-a", "cycle-b"),
            ("cycle-b", "cycle-a"),
            ("directory", "real"),
            ("fifo-link", "fifo"),
            ("bad-parent", "real/11.2.0/input/../input"),
        ] {
            symlink(target, f.0.join(name)).unwrap();
            assert!(LinkedInputFile::capture(&f.0.join(name)).is_err(), "{name}");
        }
        // Recheck the complete two-link cycle, after both nodes exist.
        assert!(
            LinkedInputFile::capture(&f.0.join("cycle-a"))
                .err()
                .unwrap()
                .contains("symlink bound")
        );
        assert!(LinkedInputFile::capture(&f.0.join("fifo")).is_err());
        assert!(LinkedInputFile::capture(&f.0.join("real/11.2.0/input/")).is_err());
    }

    #[test]
    fn linked_leaf_forty_hops_work_and_excess_hops_refuse() {
        let f = Fixture::new();
        for n in (0..41).rev() {
            let target = if n == 40 {
                "real/11.2.0/input".to_owned()
            } else {
                format!("file-hop-{}", n + 1)
            };
            symlink(target, f.0.join(format!("file-hop-{n}"))).unwrap();
        }
        LinkedInputFile::capture(&f.0.join("file-hop-1"))
            .unwrap()
            .verify()
            .unwrap();
        assert!(
            LinkedInputFile::capture(&f.0.join("file-hop-0"))
                .err()
                .unwrap()
                .contains("symlink bound")
        );
    }

    #[test]
    fn linked_leaf_does_not_change_strict_regular_file_or_directory_contracts() {
        let f = Fixture::new();
        let alias = leaf_alias(&f);
        let guard = LinkedInputFile::capture(&alias).unwrap();
        assert!(super::super::real_file(&alias).is_err());
        assert!(FileIdentity::read(&alias).is_err());
        assert!(OpenedFile::capture(&alias, guard.file()).is_err());
        assert!(OpenedFile::report(&alias).is_err());
        assert!(DirectoryGuard::capture(&alias).is_err());
        assert!(DirectoryGuard::capture(&f.input()).is_err());
        guard.verify().unwrap();
    }
}
