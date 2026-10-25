// Copyright (c) Meta Platforms, Inc. and affiliates.
// SPDX-License-Identifier: BSD-2-Clause

//! Provisional, untrusted native input descriptions and real-file transactions.
//!
//! This module checks associations supplied by a caller against its independent
//! expectations. It neither runs a build nor authenticates build-system data.
//! Parsing a description, or successfully closing a transaction, does not
//! establish compilation, artifact qualification, or permission to execute it.
//! The build-observation path uses these transactions for input checking;
//! successful input checks do not grant compilation or qualification authority.

use std::collections::BTreeMap;
use std::collections::BTreeSet;
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

type Result<T> = std::result::Result<T, String>;

/// An opaque configured producer identity and ordered settings, not execution evidence.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Configuration {
    pub producer: String,
    pub identity: String,
    pub arguments: Vec<String>,
    pub environment: BTreeMap<String, String>,
}

/// A role mapped to a file beneath the transaction root.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Input {
    pub role: String,
    pub path: PathBuf,
    pub sha256: String,
}

/// The declared producer/output association of one input role.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Dependency {
    pub input_role: String,
    pub producer: String,
    pub configuration: String,
    pub output_role: String,
}

/// Both selected and optional staged bytes must match the independent digest.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Output {
    pub role: String,
    pub selected: PathBuf,
    pub staged: Option<PathBuf>,
    pub sha256: String,
}

/// Raw descriptions only; there is no verified-source or qualification token.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Inputs {
    pub configuration: Configuration,
    pub inputs: Vec<Input>,
    pub dependencies: Vec<Dependency>,
    pub outputs: Vec<Output>,
}

fn named(value: &str) -> Result<()> {
    if value.trim().is_empty() {
        return Err("empty role or configuration identity".into());
    }
    Ok(())
}

fn index<'a, T>(
    items: &'a [T],
    key: impl Fn(&'a T) -> &'a str,
) -> Result<BTreeMap<&'a str, &'a T>> {
    let mut result = BTreeMap::new();
    for item in items {
        let role = key(item);
        named(role)?;
        if result.insert(role, item).is_some() {
            return Err(format!("duplicate role: {role}"));
        }
    }
    Ok(result)
}

impl Inputs {
    fn files(&self) -> Vec<(&Path, &str)> {
        let mut files: Vec<_> = self
            .inputs
            .iter()
            .map(|input| (input.path.as_path(), input.sha256.as_str()))
            .collect();
        for output in &self.outputs {
            files.push((&output.selected, &output.sha256));
            if let Some(staged) = &output.staged {
                files.push((staged, &output.sha256));
            }
        }
        files
    }

    fn validate(&self) -> Result<()> {
        named(&self.configuration.producer)?;
        named(&self.configuration.identity)?;
        let inputs = index(&self.inputs, |input| &input.role)?;
        index(&self.outputs, |output| &output.role)?;
        index(&self.dependencies, |dep| &dep.input_role)?;
        for dependency in &self.dependencies {
            named(&dependency.producer)?;
            named(&dependency.configuration)?;
            named(&dependency.output_role)?;
            if !inputs.contains_key(dependency.input_role.as_str()) {
                return Err(format!(
                    "missing dependency input role: {}",
                    dependency.input_role
                ));
            }
        }
        let mut paths = BTreeSet::new();
        for (path, digest) in self.files() {
            if path.as_os_str().is_empty()
                || !path
                    .components()
                    .all(|part| matches!(part, Component::Normal(_)))
            {
                return Err(format!(
                    "file mapping must be a relative path without traversal: {}",
                    path.display()
                ));
            }
            if !paths.insert(path) {
                return Err(format!("duplicate file mapping: {}", path.display()));
            }
            if digest.len() != 64
                || !digest
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            {
                return Err(format!("invalid SHA256 for {}", path.display()));
            }
        }
        Ok(())
    }
}

fn join(expected: &Inputs, observed: &Inputs) -> Result<()> {
    expected.validate()?;
    observed.validate()?;
    if expected.configuration != observed.configuration {
        return Err("configured producer/settings differ from independent expectation".into());
    }
    if index(&expected.inputs, |input| &input.role)?
        != index(&observed.inputs, |input| &input.role)?
    {
        return Err("input roles/mappings differ from independent expectation".into());
    }
    if index(&expected.dependencies, |dep| &dep.input_role)?
        != index(&observed.dependencies, |dep| &dep.input_role)?
    {
        return Err("dependency join differs from independent expectation".into());
    }
    if index(&expected.outputs, |output| &output.role)?
        != index(&observed.outputs, |output| &output.role)?
    {
        return Err("output roles/mappings differ from independent expectation".into());
    }
    Ok(())
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Identity {
    device: u64,
    inode: u64,
    size: u64,
    modified: (i64, i64),
    changed: (i64, i64),
}

impl Identity {
    fn read(metadata: &fs::Metadata) -> Self {
        Self {
            device: metadata.dev(),
            inode: metadata.ino(),
            size: metadata.len(),
            modified: (metadata.mtime(), metadata.mtime_nsec()),
            changed: (metadata.ctime(), metadata.ctime_nsec()),
        }
    }
}

type LinkStates = Vec<(PathBuf, PathBuf, Identity)>;

#[derive(Debug, PartialEq, Eq)]
struct FileState {
    canonical: PathBuf,
    identity: Identity,
    links: LinkStates,
}

fn resolve(root: &Path, relative: &Path) -> Result<(PathBuf, LinkStates)> {
    let path = root.join(relative);
    let canonical =
        fs::canonicalize(&path).map_err(|e| format!("resolve {}: {e}", path.display()))?;
    if !canonical.starts_with(root) {
        return Err(format!("file mapping escapes root: {}", path.display()));
    }
    let mut links = Vec::new();
    let mut component = root.to_owned();
    for part in relative.components() {
        component.push(part);
        let metadata = fs::symlink_metadata(&component)
            .map_err(|e| format!("metadata {}: {e}", component.display()))?;
        if metadata.file_type().is_symlink() {
            let target = fs::read_link(&component)
                .map_err(|e| format!("read link {}: {e}", component.display()))?;
            // Only direct targets are supported: otherwise a link reached
            // through this target could change without appearing in `links`.
            let target_path = component.parent().unwrap().join(&target);
            let mut prefix = PathBuf::new();
            for part in target_path.components() {
                prefix.push(part);
                let target_metadata = fs::symlink_metadata(&prefix)
                    .map_err(|e| format!("target metadata {}: {e}", prefix.display()))?;
                if target_metadata.file_type().is_symlink() {
                    return Err(format!("indirect symlink target: {}", prefix.display()));
                }
            }
            links.push((component.clone(), target, Identity::read(&metadata)));
        }
    }
    Ok((canonical, links))
}

fn read_file(root: &Path, relative: &Path, expected_digest: &str) -> Result<FileState> {
    let (canonical, links) = resolve(root, relative)?;
    let path_metadata = fs::metadata(&canonical).map_err(|e| format!("file metadata: {e}"))?;
    if !path_metadata.is_file() {
        return Err(format!("not a regular file: {}", relative.display()));
    }
    let mut file =
        fs::File::open(&canonical).map_err(|e| format!("open {}: {e}", canonical.display()))?;
    let metadata = file.metadata().map_err(|e| format!("file metadata: {e}"))?;
    if !metadata.is_file() {
        return Err(format!("not a regular file: {}", relative.display()));
    }
    let identity = Identity::read(&metadata);
    let mut hash = Sha256::new();
    let mut buffer = [0u8; 65536];
    loop {
        let count = file
            .read(&mut buffer)
            .map_err(|e| format!("read {}: {e}", relative.display()))?;
        if count == 0 {
            break;
        }
        hash.update(&buffer[..count]);
    }
    if format!("{:x}", hash.finalize()) != expected_digest {
        return Err(format!("file digest differs: {}", relative.display()));
    }
    let after = file
        .metadata()
        .map_err(|e| format!("file metadata after read: {e}"))?;
    let path_metadata =
        fs::metadata(root.join(relative)).map_err(|e| format!("path metadata after read: {e}"))?;
    if identity != Identity::read(&after)
        || identity != Identity::read(&path_metadata)
        || resolve(root, relative)? != (canonical.clone(), links.clone())
    {
        return Err(format!(
            "file mapping changed while reading: {}",
            relative.display()
        ));
    }
    Ok(FileState {
        canonical,
        identity,
        links,
    })
}

/// Join raw data to independent expectations and recheck all files after `action`.
///
/// The caller supplies the complete expected role set and owns exclusion of
/// concurrent writers. These before/after checks are not a filesystem lock or
/// protection against an adversary restoring a namespace between observations.
/// Direct contained symlinks are allowed; indirect symlink targets are refused.
/// The closure receives no compilation/qualification authority. Both its error
/// and all close failures are preserved. Panics are not caught.
pub fn with_inputs<T>(
    root: &Path,
    expected: &Inputs,
    observed: &Inputs,
    action: impl FnOnce() -> Result<T>,
) -> Result<T> {
    join(expected, observed)?;
    let canonical_root = fs::canonicalize(root).map_err(|e| format!("resolve input root: {e}"))?;
    if !canonical_root.is_dir() {
        return Err("input root is not a directory".into());
    }
    let files = expected.files();
    let mut canonical_paths = BTreeSet::new();
    let mut before = Vec::new();
    for (path, digest) in &files {
        let state = read_file(&canonical_root, path, digest)?;
        if !canonical_paths.insert(state.canonical.clone()) {
            return Err(format!(
                "duplicate resolved file mapping: {}",
                path.display()
            ));
        }
        before.push(state);
    }
    let primary = action();
    let close = {
        let mut errors = Vec::new();
        match fs::canonicalize(root) {
            Ok(current) if current == canonical_root => {}
            Ok(_) => errors.push("input root changed at close".into()),
            Err(error) => errors.push(format!("resolve input root at close: {error}")),
        }
        for ((path, digest), original) in files.iter().zip(before) {
            match read_file(&canonical_root, path, digest) {
                Ok(current) if current == original => {}
                Ok(_) => errors.push(format!(
                    "file identity changed at close: {}",
                    path.display()
                )),
                Err(error) => errors.push(error),
            }
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors.join("; "))
        }
    };
    match (primary, close) {
        (Ok(value), Ok(())) => Ok(value),
        (Err(primary), Ok(())) => Err(primary),
        (Ok(_), Err(close)) => Err(format!("input close failed: {close}")),
        (Err(primary), Err(close)) => Err(format!("{primary}; input close failed: {close}")),
    }
}
