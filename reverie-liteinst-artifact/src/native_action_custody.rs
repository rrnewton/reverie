// Copyright (c) Meta Platforms, Inc. and affiliates.
// SPDX-License-Identifier: BSD-2-Clause

//! Private custody of one explicitly supplied directory Artifact. This is a
//! complete bounded inventory, not a source search or an authority constructor.
//! Independent expected member sets and hashes belong to the action factory.

use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::collections::VecDeque;
use std::ffi::CString;
use std::ffi::OsString;
use std::fs;
use std::io::Read;
use std::io::Write;
use std::os::fd::AsRawFd;
use std::os::fd::FromRawFd;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::FileExt;
use std::os::unix::fs::MetadataExt;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Component;
use std::path::Path;
use std::path::PathBuf;

use sha2::Digest;
use sha2::Sha256;

use super::FileIdentity;
use super::Result;
use super::native_build::DirectoryGuard;
use super::native_build::OpenedFile;
use super::require;

const MAX_TREE_ENTRIES: usize = 65_536;
const MAX_TREE_DEPTH: usize = 128;
const MAX_SYMLINK_HOPS: usize = 40;
const MAX_PATH_COMPONENTS: usize = 4096;
const MAX_NAMESPACE_ENTRIES: usize = 262_144;
const MAX_MEMBER_BYTES: u64 = 8 * 1024 * 1024 * 1024;
const MAX_TREE_BYTES: u64 = 64 * 1024 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
struct Stamp {
    device: u64,
    inode: u64,
    size: u64,
    mode: u32,
    modified: (i64, i64),
    changed: (i64, i64),
}

impl Stamp {
    fn from_metadata(metadata: &fs::Metadata) -> Self {
        Self {
            device: metadata.dev(),
            inode: metadata.ino(),
            size: metadata.len(),
            mode: metadata.mode(),
            modified: (metadata.mtime(), metadata.mtime_nsec()),
            changed: (metadata.ctime(), metadata.ctime_nsec()),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum NamespaceEntry {
    // Parent directory timestamps change when unrelated siblings are created.
    // Artifact contents have their own complete child inventories below.
    Directory { device: u64, inode: u64 },
    Link { stamp: Stamp, target: PathBuf },
}

impl NamespaceEntry {
    fn read(path: &Path) -> Result<Self> {
        let metadata = fs::symlink_metadata(path)
            .map_err(|error| format!("native tree namespace {}: {error}", path.display()))?;
        if metadata.is_dir() {
            Ok(Self::Directory {
                device: metadata.dev(),
                inode: metadata.ino(),
            })
        } else if metadata.file_type().is_symlink() {
            let target = fs::read_link(path)
                .map_err(|error| format!("native tree link {}: {error}", path.display()))?;
            Ok(Self::Link {
                stamp: Stamp::from_metadata(&metadata),
                target,
            })
        } else {
            Err(format!(
                "native tree namespace is not a directory/link: {}",
                path.display()
            ))
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EntryKind {
    Directory,
    File,
    Link,
}

fn kind(file_type: fs::FileType, path: &Path) -> Result<EntryKind> {
    if file_type.is_dir() {
        Ok(EntryKind::Directory)
    } else if file_type.is_file() {
        Ok(EntryKind::File)
    } else if file_type.is_symlink() {
        Ok(EntryKind::Link)
    } else {
        Err(format!(
            "native tree entry is not a regular file/directory/link: {}",
            path.display()
        ))
    }
}

fn listing(path: &Path, bound: usize) -> Result<BTreeMap<OsString, EntryKind>> {
    let mut entries = BTreeMap::new();
    for entry in fs::read_dir(path)
        .map_err(|error| format!("native tree directory {}: {error}", path.display()))?
    {
        let entry = entry.map_err(|error| format!("native tree entry: {error}"))?;
        require(
            entries.len() < bound,
            "native tree directory entry bound exceeded",
        )?;
        let entry_kind = kind(
            entry.file_type().map_err(|error| error.to_string())?,
            &entry.path(),
        )?;
        require(
            entries.insert(entry.file_name(), entry_kind).is_none(),
            "duplicate native tree directory name",
        )?;
    }
    Ok(entries)
}

struct Directory {
    guard: DirectoryGuard,
    entries: BTreeMap<OsString, EntryKind>,
}

struct RegularFile {
    opened: OpenedFile,
    mode: u32,
}

/// One explicit member association from the retained directory capture.
/// `root.join(relative)` keeps the declared spelling even when `target.path`
/// resolves outside that root. This is observed custody data; the caller must
/// independently require the member, target, and any executable permission.
#[derive(Clone, Copy, Debug)]
pub(super) struct DirectoryMember<'a> {
    pub(super) root: &'a Path,
    pub(super) relative: &'a Path,
    pub(super) target: &'a FileIdentity,
    /// Full POSIX mode captured with the regular target, including file type.
    pub(super) mode: u32,
}

#[derive(Default)]
struct TreeContents {
    members: BTreeMap<PathBuf, FileIdentity>,
    total_bytes: u64,
}

/// Retains the explicit spelling, each directory inventory (including empty
/// directories), every traversed link/parent identity, and canonical file
/// guards. Multiple explicit relative keys may resolve to the same file.
pub(super) struct DirectoryTree {
    requested_root: PathBuf,
    resolved: BTreeMap<PathBuf, PathBuf>,
    namespace: BTreeMap<PathBuf, NamespaceEntry>,
    directories: BTreeMap<PathBuf, Directory>,
    files: BTreeMap<PathBuf, RegularFile>,
    maximum_entries: usize,
}

impl DirectoryTree {
    pub(super) fn capture(root: &Path) -> Result<(Self, BTreeMap<PathBuf, FileIdentity>)> {
        Self::capture_bounded(root, MAX_TREE_ENTRIES, MAX_TREE_DEPTH)
    }

    /// Enumerate every regular leaf from the existing capture, preserving
    /// distinct aliases. This performs no filesystem access or fresh search.
    /// The caller retains this tree and calls `verify` at transaction close.
    pub(super) fn regular_members(&self) -> impl Iterator<Item = DirectoryMember<'_>> {
        self.resolved.iter().filter_map(|(relative, target)| {
            self.files.get(target).map(|file| DirectoryMember {
                root: &self.requested_root,
                relative,
                target: &file.opened.file,
                mode: file.mode,
            })
        })
    }

    fn capture_bounded(
        root: &Path,
        maximum_entries: usize,
        maximum_depth: usize,
    ) -> Result<(Self, BTreeMap<PathBuf, FileIdentity>)> {
        require(
            maximum_entries > 0,
            "native tree entry bound must be positive",
        )?;
        let requested_root = if root.is_absolute() {
            root.to_owned()
        } else {
            std::env::current_dir()
                .map_err(|error| error.to_string())?
                .join(root)
        };
        let mut tree = Self {
            requested_root,
            resolved: BTreeMap::new(),
            namespace: BTreeMap::new(),
            directories: BTreeMap::new(),
            files: BTreeMap::new(),
            maximum_entries,
        };
        let mut contents = TreeContents::default();
        let mut ancestors = BTreeSet::new();
        tree.visit(
            Path::new(""),
            &tree.requested_root.clone(),
            0,
            maximum_depth,
            &mut ancestors,
            &mut contents,
        )?;
        // Close the first inventory too, so changes while enumerating cannot be
        // silently accepted as the starting snapshot.
        tree.verify()?;
        Ok((tree, contents.members))
    }

    fn remember(&mut self, path: &Path, entry: NamespaceEntry) -> Result<()> {
        if let Some(prior) = self.namespace.get(path) {
            require(
                prior == &entry,
                format!(
                    "native tree namespace changed during capture: {}",
                    path.display()
                ),
            )?;
        } else {
            require(
                self.namespace.len() < MAX_NAMESPACE_ENTRIES,
                "native tree namespace bound exceeded",
            )?;
            self.namespace.insert(path.to_owned(), entry);
        }
        Ok(())
    }

    // Resolve explicit link targets component by component. canonicalize alone
    // would discard intermediate symlinks, whose unchanged target bytes do not
    // make a replacement link the same namespace entry.
    fn resolve(&mut self, path: &Path) -> Result<PathBuf> {
        let mut pending = components(path)?;
        let mut current = PathBuf::from("/");
        self.remember(&current, NamespaceEntry::read(&current)?)?;
        let mut hops = 0;
        let mut steps = 0;
        while let Some(component) = pending.pop_front() {
            steps += 1;
            require(
                steps <= MAX_PATH_COMPONENTS,
                "native tree path component bound exceeded",
            )?;
            match component {
                Part::Root => {
                    current = PathBuf::from("/");
                }
                Part::Parent => {
                    current.pop();
                    if current.as_os_str().is_empty() {
                        current.push("/");
                    }
                }
                Part::Current => {}
                Part::Name(name) => {
                    let candidate = current.join(name);
                    let metadata = fs::symlink_metadata(&candidate).map_err(|error| {
                        format!("native tree resolve {}: {error}", candidate.display())
                    })?;
                    match kind(metadata.file_type(), &candidate)? {
                        EntryKind::Link => {
                            hops += 1;
                            require(
                                hops <= MAX_SYMLINK_HOPS,
                                "native tree symlink cycle/hop bound exceeded",
                            )?;
                            let entry = NamespaceEntry::read(&candidate)?;
                            let target = match &entry {
                                NamespaceEntry::Link { target, .. } => target.clone(),
                                _ => {
                                    return Err(
                                        "native tree link changed kind during resolution".into()
                                    );
                                }
                            };
                            self.remember(&candidate, entry)?;
                            require(
                                !target.as_os_str().is_empty(),
                                "native tree symlink target is empty",
                            )?;
                            let mut target = components(&target)?;
                            require(
                                target.len() + pending.len() <= MAX_PATH_COMPONENTS,
                                "native tree pending path bound exceeded",
                            )?;
                            target.append(&mut pending);
                            pending = target;
                        }
                        EntryKind::Directory => {
                            self.remember(
                                &candidate,
                                NamespaceEntry::Directory {
                                    device: metadata.dev(),
                                    inode: metadata.ino(),
                                },
                            )?;
                            current = candidate;
                        }
                        EntryKind::File => {
                            require(
                                pending.is_empty(),
                                "native tree path traverses a regular file",
                            )?;
                            current = candidate;
                        }
                    }
                }
            }
        }
        require(
            fs::canonicalize(path).map_err(|error| error.to_string())? == current,
            format!("native tree resolution changed: {}", path.display()),
        )?;
        Ok(current)
    }

    fn visit(
        &mut self,
        relative: &Path,
        spelling: &Path,
        depth: usize,
        maximum_depth: usize,
        ancestors: &mut BTreeSet<(u64, u64)>,
        contents: &mut TreeContents,
    ) -> Result<()> {
        require(depth <= maximum_depth, "native tree depth bound exceeded")?;
        require(
            self.resolved.len() < self.maximum_entries,
            "native tree entry bound exceeded",
        )?;
        let canonical = self.resolve(spelling)?;
        require(
            self.resolved
                .insert(relative.to_owned(), canonical.clone())
                .is_none(),
            "duplicate native tree relative entry",
        )?;
        let metadata = fs::symlink_metadata(&canonical).map_err(|error| error.to_string())?;
        match kind(metadata.file_type(), &canonical)? {
            EntryKind::Directory => {
                let identity = (metadata.dev(), metadata.ino());
                require(
                    ancestors.insert(identity),
                    "native tree directory symlink cycle",
                )?;
                let entries = listing(&canonical, self.maximum_entries)?;
                if let Some(prior) = self.directories.get(&canonical) {
                    require(
                        prior.entries == entries,
                        "native tree alias directory changed during capture",
                    )?;
                } else {
                    self.directories.insert(
                        canonical.clone(),
                        Directory {
                            guard: DirectoryGuard::capture(&canonical)?,
                            entries: entries.clone(),
                        },
                    );
                }
                for (name, expected_kind) in entries {
                    let child = canonical.join(&name);
                    let child_metadata =
                        fs::symlink_metadata(&child).map_err(|error| error.to_string())?;
                    require(
                        kind(child_metadata.file_type(), &child)? == expected_kind,
                        "native tree entry kind changed during capture",
                    )?;
                    self.visit(
                        &relative.join(name),
                        &child,
                        depth + 1,
                        maximum_depth,
                        ancestors,
                        contents,
                    )?;
                }
                ancestors.remove(&identity);
            }
            EntryKind::File => {
                require(
                    !relative.as_os_str().is_empty(),
                    "native tree root is not a directory",
                )?;
                if !self.files.contains_key(&canonical) {
                    require(
                        metadata.len() <= MAX_MEMBER_BYTES,
                        "native tree member byte bound exceeded",
                    )?;
                    contents.total_bytes = contents
                        .total_bytes
                        .checked_add(metadata.len())
                        .ok_or("native tree byte count overflow")?;
                    require(
                        contents.total_bytes <= MAX_TREE_BYTES,
                        "native tree total byte bound exceeded",
                    )?;
                    let stamp = Stamp::from_metadata(&metadata);
                    let identity = bounded_identity(&canonical, &stamp)?;
                    let opened = OpenedFile::capture(&canonical, &identity)?;
                    require(
                        Stamp::from_metadata(
                            &fs::symlink_metadata(&canonical).map_err(|error| error.to_string())?,
                        ) == stamp,
                        "native tree member changed while taking custody",
                    )?;
                    self.files.insert(
                        canonical.clone(),
                        RegularFile {
                            opened,
                            mode: stamp.mode,
                        },
                    );
                }
                contents.members.insert(
                    relative.to_owned(),
                    self.files[&canonical].opened.file.clone(),
                );
            }
            EntryKind::Link => {
                return Err("native tree resolution left an unresolved symlink".into());
            }
        }
        Ok(())
    }

    pub(super) fn verify(&self) -> Result<()> {
        let mut errors = Vec::new();
        for (path, expected) in &self.namespace {
            let result = NamespaceEntry::read(path).and_then(|current| {
                require(
                    current == *expected,
                    format!(
                        "native tree link/parent identity changed: {}",
                        path.display()
                    ),
                )
            });
            if let Err(error) = result {
                errors.push(error);
            }
        }
        for (path, directory) in &self.directories {
            if let Err(error) = directory.guard.verify() {
                errors.push(error);
            }
            let result = listing(path, self.maximum_entries).and_then(|current| {
                require(
                    current == directory.entries,
                    format!("native tree directory entries changed: {}", path.display()),
                )
            });
            if let Err(error) = result {
                errors.push(error);
            }
        }
        for (relative, expected) in &self.resolved {
            let path = self.requested_root.join(relative);
            let result = fs::canonicalize(&path)
                .map_err(|error| format!("native tree canonical path {}: {error}", path.display()))
                .and_then(|current| {
                    require(
                        current == *expected,
                        format!("native tree canonical target changed: {}", path.display()),
                    )
                });
            if let Err(error) = result {
                errors.push(error);
            }
        }
        for (path, file) in &self.files {
            let mode = fs::symlink_metadata(path)
                .map_err(|error| format!("native tree member mode {}: {error}", path.display()))
                .and_then(|metadata| {
                    require(
                        metadata.mode() == file.mode,
                        format!("native tree member mode changed: {}", path.display()),
                    )
                });
            if let Err(error) = mode {
                errors.push(error);
            }
            if let Err(error) = file.opened.verify() {
                errors.push(error);
            }
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors.join("; "))
        }
    }
}

enum Part {
    Root,
    Parent,
    Current,
    Name(OsString),
}

fn components(path: &Path) -> Result<VecDeque<Part>> {
    let mut parts = VecDeque::new();
    for component in path.components() {
        require(
            parts.len() < MAX_PATH_COMPONENTS,
            "native tree path component bound exceeded",
        )?;
        parts.push_back(match component {
            Component::RootDir => Part::Root,
            Component::ParentDir => Part::Parent,
            Component::CurDir => Part::Current,
            Component::Normal(name) => Part::Name(name.to_owned()),
            Component::Prefix(_) => return Err("native tree requires POSIX paths".into()),
        });
    }
    Ok(parts)
}

fn bounded_identity(path: &Path, expected: &Stamp) -> Result<FileIdentity> {
    let mut file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .map_err(|error| format!("native tree open {}: {error}", path.display()))?;
    let metadata = file.metadata().map_err(|error| error.to_string())?;
    require(
        metadata.is_file() && Stamp::from_metadata(&metadata) == *expected,
        "native tree member identity changed while opening",
    )?;
    let mut limited = (&mut file).take(expected.size + 1);
    let mut hash = Sha256::new();
    let mut buffer = [0u8; 65536];
    let mut total = 0;
    loop {
        let count = limited
            .read(&mut buffer)
            .map_err(|error| error.to_string())?;
        if count == 0 {
            break;
        }
        total += count as u64;
        hash.update(&buffer[..count]);
    }
    require(
        total == expected.size
            && Stamp::from_metadata(&file.metadata().map_err(|error| error.to_string())?)
                == *expected,
        "native tree member bytes/identity changed while reading",
    )?;
    Ok(FileIdentity {
        path: path.to_owned(),
        sha256: format!("{:x}", hash.finalize()),
    })
}

const MAX_MARKER_BYTES: usize = 64 * 1024;
const MAX_QUALIFIED_COPY_BYTES: u64 = 8 * 1024 * 1024 * 1024;

struct OutputPayload {
    stamp: Stamp,
    sha256: String,
}

/// Shared custody for the two factory-owned output files. The factory registers
/// ownership before payload work, and explicitly combines cleanup failures with
/// its primary error. Drop closes descriptors; it never hides cleanup errors.
///
/// The parent must be owner-private under cooperative ownership. POSIX has no
/// inode-conditional unlink; this does not claim to exclude a hostile same-UID
/// writer racing the final identity check and unlinkat.
struct OwnedOutput {
    directory: fs::File,
    file: fs::File,
    guard: DirectoryGuard,
    name: CString,
    path: PathBuf,
    write_started: bool,
    payload: Option<OutputPayload>,
}

impl OwnedOutput {
    fn create(destination: &Path) -> Result<Self> {
        let directory = destination.parent().ok_or("owned output has no parent")?;
        let basename = destination
            .file_name()
            .ok_or("owned output has no filename")?;
        let name = CString::new(basename.as_bytes()).map_err(|_| "NUL in owned output filename")?;
        let guard = DirectoryGuard::capture(directory)?;
        let path = fs::canonicalize(directory)
            .map_err(|error| error.to_string())?
            .join(basename);
        let opened_directory = fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(directory)
            .map_err(|error| format!("open owned output directory: {error}"))?;
        let metadata = opened_directory
            .metadata()
            .map_err(|error| error.to_string())?;
        // SAFETY: geteuid takes no pointers and only returns this process's uid.
        let own_uid = unsafe { libc::geteuid() };
        require(
            metadata.is_dir() && metadata.uid() == own_uid && metadata.mode() & 0o077 == 0,
            "owned output directory must be owner-private",
        )?;
        guard.verify()?;
        let current = fs::metadata(directory).map_err(|error| error.to_string())?;
        require(
            current.dev() == metadata.dev() && current.ino() == metadata.ino(),
            "owned output directory changed while opening",
        )?;
        // SAFETY: live directory fd and a checked NUL-terminated basename.
        // Successful openat transfers one owned descriptor.
        let descriptor = unsafe {
            libc::openat(
                opened_directory.as_raw_fd(),
                name.as_ptr(),
                libc::O_RDWR | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                0o600,
            )
        };
        if descriptor < 0 {
            return Err(format!(
                "create owned output: {}",
                std::io::Error::last_os_error()
            ));
        }
        // No fallible operation follows creation before ownership is returned.
        // SAFETY: openat just returned this descriptor; it has one owner.
        let file = unsafe { fs::File::from_raw_fd(descriptor) };
        Ok(Self {
            directory: opened_directory,
            file,
            guard,
            name,
            path,
            write_started: false,
            payload: None,
        })
    }

    fn begin_write(&mut self) -> Result<()> {
        require(!self.write_started, "owned output write cannot be retried")?;
        self.write_started = true;
        Ok(())
    }

    fn complete(&mut self, size: u64, sha256: String) -> Result<FileIdentity> {
        self.file
            .sync_all()
            .map_err(|error| format!("sync owned output: {error}"))?;
        let metadata = self.file.metadata().map_err(|error| error.to_string())?;
        require(
            metadata.is_file() && metadata.len() == size,
            "owned output differs after write",
        )?;
        self.payload = Some(OutputPayload {
            stamp: Stamp::from_metadata(&metadata),
            sha256,
        });
        self.verify()?;
        Ok(FileIdentity {
            path: self.path.clone(),
            sha256: self.payload.as_ref().unwrap().sha256.clone(),
        })
    }

    fn entry_identity(&self) -> Result<Option<(u64, u64, u32)>> {
        let mut metadata = std::mem::MaybeUninit::<libc::stat>::uninit();
        // SAFETY: live directory fd, terminated basename, valid output storage.
        // AT_SYMLINK_NOFOLLOW inspects the entry itself.
        let status = unsafe {
            libc::fstatat(
                self.directory.as_raw_fd(),
                self.name.as_ptr(),
                metadata.as_mut_ptr(),
                libc::AT_SYMLINK_NOFOLLOW,
            )
        };
        if status < 0 {
            let error = std::io::Error::last_os_error();
            if error.kind() == std::io::ErrorKind::NotFound {
                return Ok(None);
            }
            return Err(format!("inspect owned output: {error}"));
        }
        // SAFETY: successful fstatat initialized this structure.
        let metadata = unsafe { metadata.assume_init() };
        Ok(Some((metadata.st_dev, metadata.st_ino, metadata.st_mode)))
    }

    fn require_owned_entry(&self) -> Result<()> {
        let metadata = self.file.metadata().map_err(|error| error.to_string())?;
        let entry = self
            .entry_identity()?
            .ok_or("owned output entry is absent")?;
        require(
            entry.0 == metadata.dev()
                && entry.1 == metadata.ino()
                && entry.2 & libc::S_IFMT == libc::S_IFREG,
            "owned output entry was replaced; refusing another writer's entry",
        )
    }

    fn verify(&self) -> Result<()> {
        self.guard.verify()?;
        let directory = self
            .directory
            .metadata()
            .map_err(|error| error.to_string())?;
        // SAFETY: geteuid only returns the calling process's uid.
        require(
            directory.uid() == unsafe { libc::geteuid() } && directory.mode() & 0o077 == 0,
            "owned output directory is no longer owner-private",
        )?;
        self.require_owned_entry()?;
        let payload = self
            .payload
            .as_ref()
            .ok_or("owned output payload has not completed")?;
        require(
            Stamp::from_metadata(&self.file.metadata().map_err(|error| error.to_string())?)
                == payload.stamp,
            "owned output identity changed",
        )?;
        let mut hash = Sha256::new();
        let mut buffer = [0u8; 65536];
        let mut total = 0u64;
        loop {
            let bound =
                (payload.stamp.size.saturating_add(1) - total).min(buffer.len() as u64) as usize;
            if bound == 0 {
                break;
            }
            let count = self
                .file
                .read_at(&mut buffer[..bound], total)
                .map_err(|error| error.to_string())?;
            if count == 0 {
                break;
            }
            total += count as u64;
            hash.update(&buffer[..count]);
        }
        require(
            total == payload.stamp.size && format!("{:x}", hash.finalize()) == payload.sha256,
            "owned output bytes changed",
        )?;
        require(
            Stamp::from_metadata(&self.file.metadata().map_err(|error| error.to_string())?)
                == payload.stamp,
            "owned output changed while reading",
        )?;
        self.require_owned_entry()?;
        self.guard.verify()
    }

    /// Cleanup uses the original parent fd, including after its pathname moves.
    /// A substituted inode is retained and reported, never removed by pathname.
    fn remove_owned(&self) -> Result<()> {
        if self.entry_identity()?.is_none() {
            return Ok(());
        }
        self.require_owned_entry()?;
        // SAFETY: live directory fd and basename identity-checked above in the
        // factory-owned directory. No unrelated parent pathname is followed.
        if unsafe { libc::unlinkat(self.directory.as_raw_fd(), self.name.as_ptr(), 0) } < 0 {
            return Err(format!(
                "remove owned output: {}",
                std::io::Error::last_os_error()
            ));
        }
        require(
            self.entry_identity()?.is_none(),
            "owned output entry reappeared after cleanup",
        )
    }
}

/// Finalization is owned before any fallible payload validation or write.
pub(super) struct FinalMarker {
    output: OwnedOutput,
}
impl FinalMarker {
    pub(super) fn create(directory: &Path) -> Result<Self> {
        Ok(Self {
            output: OwnedOutput::create(&directory.join("finalization.json"))?,
        })
    }
    pub(super) fn write_payload(&mut self, bytes: &[u8]) -> Result<FileIdentity> {
        self.output.begin_write()?;
        require(
            bytes.len() <= MAX_MARKER_BYTES,
            "finalization payload exceeds byte bound",
        )?;
        self.output
            .file
            .write_all(bytes)
            .map_err(|error| format!("write finalization marker: {error}"))?;
        self.output
            .complete(bytes.len() as u64, super::sha256(bytes))
    }
    pub(super) fn verify(&self) -> Result<()> {
        self.output.verify()
    }
    pub(super) fn remove_owned(&self) -> Result<()> {
        self.output.remove_owned()
    }
}

/// A copy is not qualified by this handle. The factory owns qualification and
/// must register the newly created handle before calling `write_from`, then
/// remove its exact entry on any subsequent primary or close failure.
pub(super) struct QualifiedCopy {
    output: OwnedOutput,
}
impl QualifiedCopy {
    pub(super) fn create(destination: &Path) -> Result<Self> {
        Ok(Self {
            output: OwnedOutput::create(destination)?,
        })
    }
    pub(super) fn write_from(&mut self, source: &FileIdentity) -> Result<FileIdentity> {
        self.output.begin_write()?;
        let metadata = fs::symlink_metadata(&source.path)
            .map_err(|error| format!("qualified copy source: {error}"))?;
        require(
            metadata.is_file() && metadata.len() <= MAX_QUALIFIED_COPY_BYTES,
            "qualified copy source is not a bounded regular file",
        )?;
        let stamp = Stamp::from_metadata(&metadata);
        let guard = OpenedFile::capture(&source.path, source)?;
        let primary = (|| {
            let mut input = fs::OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
                .open(&source.path)
                .map_err(|error| format!("open qualified copy source: {error}"))?;
            require(
                Stamp::from_metadata(&input.metadata().map_err(|error| error.to_string())?)
                    == stamp,
                "qualified copy source changed while opening",
            )?;
            let mut limited = (&mut input).take(stamp.size + 1);
            let mut buffer = [0u8; 65536];
            let mut total = 0u64;
            let mut hash = Sha256::new();
            loop {
                let count = limited
                    .read(&mut buffer)
                    .map_err(|error| format!("read qualified copy source: {error}"))?;
                if count == 0 {
                    break;
                }
                total += count as u64;
                require(
                    total <= stamp.size,
                    "qualified copy source grew while copying",
                )?;
                self.output
                    .file
                    .write_all(&buffer[..count])
                    .map_err(|error| format!("write qualified copy: {error}"))?;
                hash.update(&buffer[..count]);
            }
            require(
                total == stamp.size
                    && format!("{:x}", hash.finalize()) == source.sha256
                    && Stamp::from_metadata(&input.metadata().map_err(|error| error.to_string())?)
                        == stamp,
                "qualified copy source bytes/identity changed while copying",
            )?;
            self.output.complete(total, source.sha256.clone())
        })();
        match (primary, guard.verify()) {
            (Ok(file), Ok(())) => Ok(file),
            (Err(error), Ok(())) => Err(error),
            (Ok(_), Err(close)) => Err(format!("qualified copy source close: {close}")),
            (Err(error), Err(close)) => {
                Err(format!("{error}; qualified copy source close: {close}"))
            }
        }
    }
    pub(super) fn verify(&self) -> Result<()> {
        self.output.verify()
    }
    pub(super) fn remove_owned(&self) -> Result<()> {
        self.output.remove_owned()
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::DirBuilderExt;
    use std::os::unix::fs::PermissionsExt;
    use std::os::unix::fs::symlink;
    use std::sync::atomic::AtomicU64;
    use std::sync::atomic::Ordering;

    use super::*;

    static NEXT: AtomicU64 = AtomicU64::new(0);

    struct Fixture {
        root: PathBuf,
    }
    impl Fixture {
        fn new() -> Self {
            let root = std::env::temp_dir().join(format!(
                "native-tree-custody-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&root).unwrap();
            Self {
                root: fs::canonicalize(root).unwrap(),
            }
        }
        fn directory(&self, relative: &str) -> PathBuf {
            let path = self.root.join(relative);
            fs::create_dir_all(&path).unwrap();
            path
        }
        fn file(&self, relative: &str, contents: &[u8]) -> PathBuf {
            let path = self.root.join(relative);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, contents).unwrap();
            path
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    #[test]
    fn complete_members_preserve_explicit_file_and_directory_aliases() {
        let fixture = Fixture::new();
        let root = fixture
            .directory("artifact/empty")
            .parent()
            .unwrap()
            .to_owned();
        let backing = fixture.file("backing/source.rs", b"independent source bytes");
        fixture.file("artifact/direct.rs", b"direct bytes");
        symlink(&backing, root.join("first.rs")).unwrap();
        symlink("../backing/source.rs", root.join("second.rs")).unwrap();
        symlink("../backing", root.join("source_tree")).unwrap();
        let (tree, members) = DirectoryTree::capture(&root).unwrap();
        assert_eq!(
            members.keys().cloned().collect::<Vec<_>>(),
            [
                "direct.rs",
                "first.rs",
                "second.rs",
                "source_tree/source.rs"
            ]
            .into_iter()
            .map(PathBuf::from)
            .collect::<Vec<_>>()
        );
        assert_eq!(
            members[Path::new("first.rs")],
            members[Path::new("second.rs")]
        );
        assert_eq!(
            members[Path::new("first.rs")],
            members[Path::new("source_tree/source.rs")]
        );
        assert_eq!(members[Path::new("first.rs")].path, backing);
        assert_eq!(
            members[Path::new("first.rs")].sha256,
            super::super::sha256(b"independent source bytes")
        );
        tree.verify().unwrap();
    }

    #[test]
    fn symlinked_root_and_intermediate_link_chain_keep_custody() {
        let fixture = Fixture::new();
        fixture.file("backing/sub/file", b"bytes");
        symlink("backing", fixture.root.join("middle")).unwrap();
        symlink("middle/sub", fixture.root.join("artifact")).unwrap();
        let (tree, members) = DirectoryTree::capture(&fixture.root.join("artifact")).unwrap();
        assert_eq!(members.len(), 1);
        tree.verify().unwrap();
        fs::rename(
            fixture.root.join("middle"),
            fixture.root.join("retained-link"),
        )
        .unwrap();
        symlink("backing", fixture.root.join("middle")).unwrap();
        assert!(tree.verify().unwrap_err().contains("identity changed"));
    }

    #[test]
    fn same_byte_file_replacement_and_in_place_change_refuse() {
        for replace in [false, true] {
            let fixture = Fixture::new();
            let file = fixture.file("artifact/source", b"original");
            let (tree, _) = DirectoryTree::capture(file.parent().unwrap()).unwrap();
            if replace {
                let replacement = fixture.file("replacement", b"original");
                fs::rename(replacement, &file).unwrap();
            } else {
                fs::write(&file, b"modified").unwrap();
            }
            assert!(tree.verify().is_err());
        }
    }

    #[test]
    fn inventory_refuses_added_removed_renamed_and_empty_directory_entries() {
        for change in 0..5 {
            let fixture = Fixture::new();
            let root = fixture
                .directory("artifact/empty")
                .parent()
                .unwrap()
                .to_owned();
            fixture.file("artifact/source", b"bytes");
            let (tree, _) = DirectoryTree::capture(&root).unwrap();
            match change {
                0 => {
                    fixture.file("artifact/new-file", b"new");
                }
                1 => fs::remove_file(root.join("source")).unwrap(),
                2 => fs::rename(root.join("source"), root.join("renamed")).unwrap(),
                3 => {
                    fixture.directory("artifact/new-empty");
                }
                4 => fs::remove_dir(root.join("empty")).unwrap(),
                _ => unreachable!(),
            }
            assert!(tree.verify().unwrap_err().contains("entries changed"));
        }
    }

    #[test]
    fn root_retarget_and_backing_parent_replacement_refuse_with_old_bytes_intact() {
        for change_parent in [false, true] {
            let fixture = Fixture::new();
            let original = fixture.file("backing/source", b"same bytes");
            if change_parent {
                fixture.directory("replacement-backing");
                fs::hard_link(&original, fixture.root.join("replacement-backing/source")).unwrap();
            }
            symlink("backing", fixture.root.join("artifact")).unwrap();
            let (tree, _) = DirectoryTree::capture(&fixture.root.join("artifact")).unwrap();
            let prior = Stamp::from_metadata(&fs::metadata(&original).unwrap());
            if change_parent {
                fs::rename(
                    fixture.root.join("backing"),
                    fixture.root.join("old-backing"),
                )
                .unwrap();
                fs::rename(
                    fixture.root.join("replacement-backing"),
                    fixture.root.join("backing"),
                )
                .unwrap();
                assert_eq!(
                    Stamp::from_metadata(&fs::metadata(&original).unwrap()),
                    prior
                );
            } else {
                fixture.file("other/source", b"same bytes");
                fs::remove_file(fixture.root.join("artifact")).unwrap();
                symlink("other", fixture.root.join("artifact")).unwrap();
            }
            assert!(tree.verify().is_err());
            assert_eq!(fs::read(&original).unwrap(), b"same bytes");
        }
    }

    #[test]
    fn unchanged_tree_allows_unrelated_siblings_of_artifact_and_backing_parents() {
        let fixture = Fixture::new();
        let root = fixture.directory("artifact");
        let backing = fixture.file("backing/source", b"bytes");
        symlink(&backing, root.join("source")).unwrap();
        let (tree, _) = DirectoryTree::capture(&root).unwrap();
        fixture.file("unrelated", b"outside tree");
        fixture.file("backing/sibling", b"not a declared directory member");
        tree.verify().unwrap();
    }

    #[test]
    fn directory_cycles_link_cycles_dangling_links_and_file_roots_refuse() {
        let fixture = Fixture::new();
        let root = fixture.directory("artifact");
        symlink(".", root.join("recursive")).unwrap();
        assert!(DirectoryTree::capture(&root).is_err());
        fs::remove_file(root.join("recursive")).unwrap();
        symlink("second", root.join("first")).unwrap();
        symlink("first", root.join("second")).unwrap();
        assert!(DirectoryTree::capture(&root).is_err());
        fs::remove_file(root.join("first")).unwrap();
        fs::remove_file(root.join("second")).unwrap();
        symlink("missing", root.join("dangling")).unwrap();
        assert!(DirectoryTree::capture(&root).is_err());
        let file = fixture.file("file-root", b"not a directory");
        assert!(DirectoryTree::capture(&file).is_err());
    }

    #[test]
    fn traversal_limits_refuse_whole_tree_without_silently_truncating_members() {
        let fixture = Fixture::new();
        let file = fixture.file("artifact/nested/source", b"bytes");
        let root = fixture.root.join("artifact");
        let (_, members) = DirectoryTree::capture_bounded(&root, 3, 2).unwrap();
        assert_eq!(members.len(), 1);
        assert_eq!(members[Path::new("nested/source")].path, file);
        assert!(DirectoryTree::capture_bounded(&root, 2, 2).is_err());
        assert!(DirectoryTree::capture_bounded(&root, 3, 1).is_err());
    }

    #[test]
    fn non_utf8_relative_names_are_preserved_exactly() {
        use std::os::unix::ffi::OsStringExt;
        let fixture = Fixture::new();
        let root = fixture.directory("artifact");
        let name = OsString::from_vec(b"source-\xff".to_vec());
        fs::write(root.join(&name), b"bytes").unwrap();
        let (tree, members) = DirectoryTree::capture(&root).unwrap();
        assert!(members.contains_key(Path::new(&name)));
        tree.verify().unwrap();
    }

    #[test]
    fn linked_directory_contents_are_inventory_not_only_link_metadata() {
        let fixture = Fixture::new();
        let root = fixture.directory("artifact");
        fixture.file("tools/compiler", b"tool bytes");
        symlink("../tools", root.join("tool_directory")).unwrap();
        let (tree, _) = DirectoryTree::capture(&root).unwrap();
        fixture.file("tools/new-tool", b"unlisted");
        assert!(tree.verify().unwrap_err().contains("entries changed"));
    }

    #[test]
    fn regular_member_observations_keep_bound_root_alias_and_target_mode() {
        let fixture = Fixture::new();
        let source_root = fixture.directory("backing");
        let target = fixture.file("tools/compiler", b"synthetic tool bytes");
        fs::set_permissions(&target, fs::Permissions::from_mode(0o755)).unwrap();
        symlink("../tools/compiler", source_root.join("tool")).unwrap();
        symlink("../tools", source_root.join("tool_directory")).unwrap();
        let root = fixture.root.join("artifact");
        symlink("backing", &root).unwrap();
        let (tree, original_members) = DirectoryTree::capture(&root).unwrap();
        let observed: Vec<_> = tree.regular_members().collect();
        assert_eq!(observed.len(), 2);
        assert_eq!(observed[0].relative, Path::new("tool"));
        assert_eq!(observed[1].relative, Path::new("tool_directory/compiler"));
        for member in observed {
            assert_eq!(member.root, root);
            assert_ne!(member.root.join(member.relative), target);
            assert_eq!(member.target.path, target);
            assert_eq!(member.target, &original_members[member.relative]);
            assert_eq!(
                member.target.sha256,
                super::super::sha256(b"synthetic tool bytes")
            );
            assert_eq!(member.mode & 0o777, 0o755);
            assert_eq!(member.mode & libc::S_IFMT, libc::S_IFREG);
        }
        tree.verify().unwrap();
    }

    #[test]
    fn alias_retarget_or_removal_refuses_with_regular_content_rows_unchanged() {
        for retarget in [false, true] {
            let fixture = Fixture::new();
            let root = fixture.directory("artifact");
            let first = fixture.file("backing/first", b"same regular bytes");
            let second = fixture.file("backing/second", b"same regular bytes");
            let rows = [
                FileIdentity::read(&first).unwrap(),
                FileIdentity::read(&second).unwrap(),
            ];
            let stamps = [
                Stamp::from_metadata(&fs::metadata(&first).unwrap()),
                Stamp::from_metadata(&fs::metadata(&second).unwrap()),
            ];
            let alias = root.join("source");
            symlink(&first, &alias).unwrap();
            let (tree, _) = DirectoryTree::capture(&root).unwrap();
            assert_eq!(tree.regular_members().next().unwrap().target.path, first);
            fs::remove_file(&alias).unwrap();
            if retarget {
                symlink(&second, &alias).unwrap();
            }
            for (index, path) in [&first, &second].into_iter().enumerate() {
                assert_eq!(FileIdentity::read(path).unwrap(), rows[index]);
                assert_eq!(
                    Stamp::from_metadata(&fs::metadata(path).unwrap()),
                    stamps[index]
                );
            }
            assert!(tree.verify().is_err());
        }
    }

    #[test]
    fn executable_permission_changes_refuse_with_unchanged_bytes_and_target() {
        for original_mode in [0o644, 0o755] {
            let fixture = Fixture::new();
            let target = fixture.file("tools/compiler", b"same tool bytes");
            fs::set_permissions(&target, fs::Permissions::from_mode(original_mode)).unwrap();
            let root = fixture.directory("artifact");
            symlink(&target, root.join("compiler")).unwrap();
            let (tree, _) = DirectoryTree::capture(&root).unwrap();
            let member = tree.regular_members().next().unwrap();
            assert_eq!(member.mode & 0o777, original_mode);
            assert_eq!(member.mode & 0o111 != 0, original_mode == 0o755);
            let prior = member.target.clone();
            let changed_mode = if original_mode == 0o755 { 0o644 } else { 0o755 };
            fs::set_permissions(&target, fs::Permissions::from_mode(changed_mode)).unwrap();
            assert_eq!(FileIdentity::read(&target).unwrap(), prior);
            assert_eq!(fs::canonicalize(root.join("compiler")).unwrap(), target);
            assert!(tree.verify().unwrap_err().contains("member mode changed"));
        }
    }

    fn private_directory(fixture: &Fixture, name: &str) -> PathBuf {
        let path = fixture.root.join(name);
        fs::DirBuilder::new().mode(0o700).create(&path).unwrap();
        path
    }

    #[test]
    fn marker_ownership_precedes_fallible_payload_validation() {
        let fixture = Fixture::new();
        let directory = private_directory(&fixture, "owner");
        let mut marker = FinalMarker::create(&directory).unwrap();
        assert!(directory.join("finalization.json").exists());
        assert!(
            marker
                .write_payload(&vec![b'x'; MAX_MARKER_BYTES + 1])
                .is_err()
        );
        marker.remove_owned().unwrap();
        assert!(!directory.join("finalization.json").exists());
    }

    #[test]
    fn completed_marker_retains_bytes_and_refuses_same_byte_replacement() {
        let fixture = Fixture::new();
        let directory = private_directory(&fixture, "owner");
        let mut marker = FinalMarker::create(&directory).unwrap();
        let identity = marker.write_payload(b"closed synthetic record\n").unwrap();
        assert_eq!(
            identity.sha256,
            super::super::sha256(b"closed synthetic record\n")
        );
        marker.verify().unwrap();
        let replacement = fixture.file("replacement", b"closed synthetic record\n");
        fs::rename(replacement, directory.join("finalization.json")).unwrap();
        assert!(marker.verify().is_err());
        assert!(marker.remove_owned().unwrap_err().contains("replaced"));
        assert_eq!(
            fs::read(directory.join("finalization.json")).unwrap(),
            b"closed synthetic record\n"
        );
    }

    #[test]
    fn marker_cleanup_uses_original_directory_fd_after_path_replacement() {
        let fixture = Fixture::new();
        let directory = private_directory(&fixture, "owner");
        let mut marker = FinalMarker::create(&directory).unwrap();
        marker.write_payload(b"original").unwrap();
        let moved = fixture.root.join("moved-owner");
        fs::rename(&directory, &moved).unwrap();
        private_directory(&fixture, "owner");
        fs::write(directory.join("finalization.json"), b"another writer").unwrap();
        assert!(marker.verify().is_err());
        marker.remove_owned().unwrap();
        assert!(!moved.join("finalization.json").exists());
        assert_eq!(
            fs::read(directory.join("finalization.json")).unwrap(),
            b"another writer"
        );
    }

    #[test]
    fn marker_refuses_existing_files_links_and_in_place_mutation() {
        for existing_link in [false, true] {
            let fixture = Fixture::new();
            let directory = private_directory(&fixture, "owner");
            if existing_link {
                let target = fixture.file("other", b"retained");
                symlink(target, directory.join("finalization.json")).unwrap();
            } else {
                fs::write(directory.join("finalization.json"), b"retained").unwrap();
            }
            assert!(FinalMarker::create(&directory).is_err());
            assert_eq!(
                fs::read(directory.join("finalization.json")).unwrap(),
                b"retained"
            );
        }
        let fixture = Fixture::new();
        let directory = private_directory(&fixture, "owner");
        let mut marker = FinalMarker::create(&directory).unwrap();
        marker.write_payload(b"original").unwrap();
        fs::write(directory.join("finalization.json"), b"modified").unwrap();
        assert!(marker.verify().is_err());
        marker.remove_owned().unwrap();
        assert!(!directory.join("finalization.json").exists());
    }

    #[test]
    fn copy_streams_exact_source_bytes_and_refuses_same_byte_replacement() {
        let fixture = Fixture::new();
        let directory = private_directory(&fixture, "qualified");
        let destination = directory.join("product");
        // Multiple buffers exercise descriptor streaming; these synthetic bytes
        // are not an ELF qualification or a successful compiler observation.
        let bytes: Vec<_> = (0..200_003).map(|index| (index % 251) as u8).collect();
        let source = FileIdentity::read(&fixture.file("source", &bytes)).unwrap();
        let mut copy = QualifiedCopy::create(&destination).unwrap();
        assert!(destination.exists());
        assert!(copy.verify().is_err());
        let actual = copy.write_from(&source).unwrap();
        assert_eq!(actual.path, destination);
        assert_eq!(actual.sha256, source.sha256);
        assert_eq!(fs::read(&destination).unwrap(), bytes);
        copy.verify().unwrap();
        assert!(copy.write_from(&source).unwrap_err().contains("retried"));
        let replacement = fixture.file("replacement", &bytes);
        fs::rename(replacement, &destination).unwrap();
        assert!(copy.verify().is_err());
        assert!(copy.remove_owned().unwrap_err().contains("replaced"));
        assert_eq!(fs::read(&destination).unwrap(), bytes);
    }

    #[test]
    fn copy_ownership_precedes_source_and_destination_io_failures() {
        for fail_write in [false, true] {
            let fixture = Fixture::new();
            let directory = private_directory(&fixture, "qualified");
            let destination = directory.join("product");
            let source = FileIdentity::read(&fixture.file("source", b"unchanged source")).unwrap();
            let mut copy = QualifiedCopy::create(&destination).unwrap();
            assert!(destination.exists());
            if fail_write {
                // The same owned inode, reopened read-only, makes the write
                // fail deterministically without permission/global-state tricks.
                copy.output.file = fs::File::open(&destination).unwrap();
            } else {
                fs::remove_file(&source.path).unwrap();
            }
            let error = copy.write_from(&source).unwrap_err();
            assert!(
                error.contains(if fail_write {
                    "write qualified copy"
                } else {
                    "qualified copy source"
                }),
                "{error}"
            );
            assert!(copy.verify().is_err());
            copy.remove_owned().unwrap();
            assert!(!destination.exists());
            if fail_write {
                source.verify().unwrap();
            }
        }
    }

    #[test]
    fn copy_cleanup_uses_original_directory_fd_after_path_replacement() {
        let fixture = Fixture::new();
        let directory = private_directory(&fixture, "qualified");
        let destination = directory.join("product");
        let source = FileIdentity::read(&fixture.file("source", b"original product")).unwrap();
        let mut copy = QualifiedCopy::create(&destination).unwrap();
        copy.write_from(&source).unwrap();
        let moved = fixture.root.join("moved-qualified");
        fs::rename(&directory, &moved).unwrap();
        private_directory(&fixture, "qualified");
        fs::write(&destination, b"another writer").unwrap();
        assert!(copy.verify().is_err());
        copy.remove_owned().unwrap();
        assert!(!moved.join("product").exists());
        assert_eq!(fs::read(&destination).unwrap(), b"another writer");
        source.verify().unwrap();
    }

    #[test]
    fn copy_refuses_existing_destinations_and_cleans_mutated_owned_inode() {
        for existing_link in [false, true] {
            let fixture = Fixture::new();
            let directory = private_directory(&fixture, "qualified");
            let destination = directory.join("product");
            if existing_link {
                symlink(fixture.file("other", b"retained"), &destination).unwrap();
            } else {
                fs::write(&destination, b"retained").unwrap();
            }
            assert!(QualifiedCopy::create(&destination).is_err());
            assert_eq!(fs::read(&destination).unwrap(), b"retained");
        }
        let fixture = Fixture::new();
        let directory = private_directory(&fixture, "qualified");
        let destination = directory.join("product");
        let source = FileIdentity::read(&fixture.file("source", b"original")).unwrap();
        let mut copy = QualifiedCopy::create(&destination).unwrap();
        copy.write_from(&source).unwrap();
        fs::write(&destination, b"modified").unwrap();
        assert!(copy.verify().is_err());
        copy.remove_owned().unwrap();
        assert!(!destination.exists());
        source.verify().unwrap();
    }
}
