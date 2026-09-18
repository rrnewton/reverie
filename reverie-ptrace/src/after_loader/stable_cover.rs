//! Fail-closed verification for persistent loader cover directories.
//!
//! A cover is a pre-published directory tree whose regular files are empty
//! placeholders.  A launcher can bind retained, immutable artifacts over those
//! placeholders without creating a fresh tmpfs and therefore without changing
//! the directory device/inode domain on every run.
//!
//! This verifier proves one bounded source snapshot.  It does **not** make
//! directory membership immutable: Linux fs-verity authenticates regular-file
//! contents, not directory entries.  Keeping the backing filesystem protected
//! against mutation through every other mount for the entire store epoch is an
//! external broker/staging contract.  A read-only, no-atime view is required
//! here but is not by itself that global guarantee.
//!
//! The source snapshot authenticates both forms of mount ID, mount-root state,
//! filesystem type, the complete mount-policy flag word, and every static
//! `statfs` geometry field that Detcore leaves guest-visible.  Volatile free
//! space, free inode, and filesystem-id fields are deliberately absent because
//! the common Detcore `statfs`/`fstatfs` path canonicalizes them.
//! V4 binds the requested `absent-v1` xattr policy and the exact reviewed inode
//! ioctl policy for the root and every record into both digests.  The local
//! `flistxattr` checks below prove only that the current caller sees a zero-byte
//! name list; they do not discharge `absent-v1`.  Runnable authorization also
//! requires a live, authenticated source-epoch broker receipt with the global
//! authority described by that policy.  Earlier digest measurements are
//! intentionally incompatible and must not authorize V4.
//!
//! This module intentionally does not form a procfd pathname or claim that a
//! bind mount occurred.  The lower-level `PreparedMountEpoch` must retain the
//! duplicated source descriptor, authenticate its current task's procfd path,
//! perform the mount, open the exact intended target, and produce a positive
//! receipt proving a distinct unique mount ID plus the exact target policy.
//! The common pre-entry gate must still forbid mount metadata observation until
//! that temporary namespace is restored.

use std::collections::BTreeSet;
use std::ffi::CStr;
use std::ffi::CString;
use std::fs::File;
use std::io;
use std::os::fd::AsRawFd;
use std::os::fd::FromRawFd;
use std::os::fd::RawFd;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;

use sha2::Digest;
use sha2::Sha256;

use super::FileIdentity;
use super::inode_policy::StableInodeKind;
use super::inode_policy::StableInodePolicy;

const MAX_TREE_ENTRIES: usize = 128;
const MAX_RELATIVE_PATH_BYTES: usize = 64 * 1024;
const GETDENTS_BUFFER_BYTES: usize = 16 * 1024;

const RESOLVE_NO_XDEV: u64 = 0x01;
const RESOLVE_NO_MAGICLINKS: u64 = 0x02;
const RESOLVE_NO_SYMLINKS: u64 = 0x04;
const RESOLVE_BENEATH: u64 = 0x08;

const ST_RDONLY_FLAG: u64 = 0x0001;
const ST_VALID_FLAG: u64 = 0x0020;
const ST_NOATIME_FLAG: u64 = 0x0400;
const REQUIRED_MOUNT_FLAGS: u64 = ST_RDONLY_FLAG | ST_NOATIME_FLAG;

const EXT4_SUPER_MAGIC: i64 = 0xef53;
const BTRFS_SUPER_MAGIC: i64 = 0x9123_683e;
const F2FS_SUPER_MAGIC: i64 = 0xf2f5_2010;
const XFS_SUPER_MAGIC: i64 = 0x5846_5342;

const STATX_BASIC_STATS: u32 = 0x0000_07ff;
const STATX_BTIME: u32 = 0x0000_0800;
const STATX_MNT_ID: u32 = 0x0000_1000;
const STATX_DIOALIGN: u32 = 0x0000_2000;
const STATX_MNT_ID_UNIQUE: u32 = 0x0000_4000;
const STATX_SUBVOL: u32 = 0x0000_8000;
const STATX_WRITE_ATOMIC: u32 = 0x0001_0000;
const STATX_DIO_READ_ALIGN: u32 = 0x0002_0000;
const KNOWN_STATX_MASK: u32 = STATX_BASIC_STATS
    | STATX_BTIME
    | STATX_MNT_ID
    | STATX_DIOALIGN
    | STATX_MNT_ID_UNIQUE
    | STATX_SUBVOL
    | STATX_WRITE_ATOMIC
    | STATX_DIO_READ_ALIGN;
const BASE_STATX_MASK: u32 = KNOWN_STATX_MASK & !STATX_MNT_ID_UNIQUE;
const REQUIRED_STATX_MASK: u32 = STATX_BASIC_STATS | STATX_BTIME | STATX_MNT_ID;

const STATX_ATTR_COMPRESSED: u64 = 0x0000_0004;
const STATX_ATTR_IMMUTABLE: u64 = 0x0000_0010;
const STATX_ATTR_APPEND: u64 = 0x0000_0020;
const STATX_ATTR_NODUMP: u64 = 0x0000_0040;
const STATX_ATTR_ENCRYPTED: u64 = 0x0000_0800;
const STATX_ATTR_AUTOMOUNT: u64 = 0x0000_1000;
const STATX_ATTR_MOUNT_ROOT: u64 = 0x0000_2000;
const STATX_ATTR_VERITY: u64 = 0x0010_0000;
const STATX_ATTR_DAX: u64 = 0x0020_0000;
const STATX_ATTR_WRITE_ATOMIC: u64 = 0x0040_0000;
const KNOWN_STATX_ATTRIBUTES: u64 = STATX_ATTR_COMPRESSED
    | STATX_ATTR_IMMUTABLE
    | STATX_ATTR_APPEND
    | STATX_ATTR_NODUMP
    | STATX_ATTR_ENCRYPTED
    | STATX_ATTR_AUTOMOUNT
    | STATX_ATTR_MOUNT_ROOT
    | STATX_ATTR_VERITY
    | STATX_ATTR_DAX
    | STATX_ATTR_WRITE_ATOMIC;

/// Schema-4 names this exact requested policy.  V4 digests bind the token so a
/// future verifier cannot silently reinterpret an already-approved digest.
/// A local zero-name observation is necessary but not sufficient evidence.
pub(crate) const STABLE_COVER_XATTR_POLICY: &str = "absent-v1";
const STABLE_COVER_XATTR_POLICY_BYTES: &[u8] = b"absent-v1";
const STABLE_INODE_POLICY_ENCODING_BYTES: usize = 38;
const METADATA_DIGEST_DOMAIN: &[u8] = b"reverie-stable-cover-metadata-v4\0";
const TREE_DIGEST_DOMAIN: &[u8] = b"reverie-stable-cover-tree-v4\0";

#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct OpenHow {
    flags: u64,
    mode: u64,
    resolve: u64,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct KernelStatxTimestamp {
    seconds: i64,
    nanoseconds: u32,
    reserved: i32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct KernelStatx {
    mask: u32,
    block_size: u32,
    attributes: u64,
    link_count: u32,
    uid: u32,
    gid: u32,
    mode: u16,
    spare0: [u16; 1],
    inode: u64,
    size: u64,
    blocks: u64,
    attributes_mask: u64,
    access_time: KernelStatxTimestamp,
    birth_time: KernelStatxTimestamp,
    change_time: KernelStatxTimestamp,
    modification_time: KernelStatxTimestamp,
    rdev_major: u32,
    rdev_minor: u32,
    dev_major: u32,
    dev_minor: u32,
    mount_id: u64,
    dio_memory_alignment: u32,
    dio_offset_alignment: u32,
    subvolume: u64,
    atomic_write_unit_min: u32,
    atomic_write_unit_max: u32,
    atomic_write_segments_max: u32,
    dio_read_offset_alignment: u32,
    atomic_write_unit_max_opt: u32,
    spare2: [u32; 1],
    spare3: [u64; 8],
}

/// The Linux x86-64 UAPI `struct statfs`, as written by `SYS_fstatfs`.
///
/// This is deliberately not `libc::statfs64`: the glibc entry point is a
/// userspace ABI adapter and is not evidence that the kernel wrote the final
/// `f_flags` and `f_spare` words.  The stable-cover proof needs those raw words
/// because the reserved tail remains visible to the guest.  Keeping an exact
/// syscall layout also lets the read path poison every byte before entering
/// the kernel, so an unwritten tail cannot look like kernel-supplied zeroes.
#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct KernelStatfsX86_64 {
    filesystem_type: i64,
    block_size: i64,
    blocks: i64,
    blocks_free: i64,
    blocks_available: i64,
    files: i64,
    files_free: i64,
    filesystem_id: [i32; 2],
    maximum_name_length: i64,
    fragment_size: i64,
    mount_flags: i64,
    spare: [i64; 4],
}

const _: () = assert!(std::mem::size_of::<OpenHow>() == 24);
const _: () = assert!(std::mem::size_of::<KernelStatxTimestamp>() == 16);
const _: () = assert!(std::mem::size_of::<KernelStatx>() == 256);
const _: () = assert!(std::mem::size_of::<KernelStatfsX86_64>() == 120);
const _: () = assert!(std::mem::align_of::<KernelStatfsX86_64>() == 8);
const _: () = assert!(std::mem::offset_of!(KernelStatfsX86_64, filesystem_type) == 0);
const _: () = assert!(std::mem::offset_of!(KernelStatfsX86_64, block_size) == 8);
const _: () = assert!(std::mem::offset_of!(KernelStatfsX86_64, blocks) == 16);
const _: () = assert!(std::mem::offset_of!(KernelStatfsX86_64, blocks_free) == 24);
const _: () = assert!(std::mem::offset_of!(KernelStatfsX86_64, blocks_available) == 32);
const _: () = assert!(std::mem::offset_of!(KernelStatfsX86_64, files) == 40);
const _: () = assert!(std::mem::offset_of!(KernelStatfsX86_64, files_free) == 48);
const _: () = assert!(std::mem::offset_of!(KernelStatfsX86_64, filesystem_id) == 56);
const _: () = assert!(std::mem::offset_of!(KernelStatfsX86_64, maximum_name_length) == 64);
const _: () = assert!(std::mem::offset_of!(KernelStatfsX86_64, fragment_size) == 72);
const _: () = assert!(std::mem::offset_of!(KernelStatfsX86_64, mount_flags) == 80);
const _: () = assert!(std::mem::offset_of!(KernelStatfsX86_64, spare) == 88);
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
const _: () = assert!(libc::SYS_fstatfs == 138);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CoverEntryKind {
    Directory,
    Placeholder,
}

impl CoverEntryKind {
    const fn digest_tag(self) -> u8 {
        match self {
            Self::Directory => b'd',
            Self::Placeholder => b'f',
        }
    }

    const fn inode_kind(self) -> StableInodeKind {
        match self {
            Self::Directory => StableInodeKind::CoverDirectory,
            Self::Placeholder => StableInodeKind::CoverPlaceholder,
        }
    }
}

/// Canonical, injective encoding of one already-validated inode ioctl policy.
/// Keeping the bytes lets this verifier revalidate and digest the policy while
/// preserving `StableInodePolicy`'s private construction boundary.
#[derive(Clone, Debug, Eq, PartialEq)]
struct CanonicalInodePolicy([u8; STABLE_INODE_POLICY_ENCODING_BYTES]);

impl CanonicalInodePolicy {
    fn read(
        descriptor: RawFd,
        filesystem: &StableFilesystemMetadata,
        metadata: &CanonicalMetadata,
        kind: CoverEntryKind,
    ) -> io::Result<Self> {
        if StableFilesystemMetadata::read(descriptor)? != *filesystem {
            return Err(invalid_data(
                "cover inode descriptor differs from the root filesystem profile",
            ));
        }
        let policy =
            StableInodePolicy::read(descriptor, filesystem.filesystem_type, kind.inode_kind())?;
        policy.validate_statx(metadata.attributes, metadata.attributes_mask)?;
        let mut encoded = Vec::with_capacity(STABLE_INODE_POLICY_ENCODING_BYTES);
        policy.encode(&mut encoded);
        let encoded = encoded.try_into().map_err(|value: Vec<u8>| {
            invalid_data(format!(
                "stable inode policy encoding has length {}, expected {STABLE_INODE_POLICY_ENCODING_BYTES}",
                value.len()
            ))
        })?;
        Ok(Self(encoded))
    }

    fn encode(&self, output: &mut Vec<u8>) {
        output.extend_from_slice(&self.0);
    }
}

/// Source-mount fields that remain guest-visible after Detcore canonicalizes
/// the volatile free-space, free-inode, and filesystem-id fields of `statfs`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct StableFilesystemMetadata {
    filesystem_type: i64,
    block_size: u64,
    total_blocks: u64,
    total_files: u64,
    maximum_name_length: u64,
    fragment_size: u64,
    mount_flags: u64,
}

impl StableFilesystemMetadata {
    fn read(descriptor: RawFd) -> io::Result<Self> {
        let filesystem = raw_fstatfs_x86_64(descriptor)?;
        let filesystem_type = filesystem.filesystem_type;
        let block_size = nonnegative_filesystem_word(
            filesystem.block_size,
            "cover filesystem block size is invalid",
        )?;
        let total_blocks = nonnegative_filesystem_word(
            filesystem.blocks,
            "cover filesystem block count is invalid",
        )?;
        let total_files = nonnegative_filesystem_word(
            filesystem.files,
            "cover filesystem inode count is invalid",
        )?;
        let maximum_name_length = nonnegative_filesystem_word(
            filesystem.maximum_name_length,
            "cover filesystem maximum name length is invalid",
        )?;
        let fragment_size = nonnegative_filesystem_word(
            filesystem.fragment_size,
            "cover filesystem fragment size is invalid",
        )?;
        let mut mount = std::mem::MaybeUninit::<libc::statvfs>::zeroed();
        // SAFETY: `descriptor` is live for the call and `mount` is writable.
        if unsafe { libc::fstatvfs(descriptor, mount.as_mut_ptr()) } != 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: successful fstatvfs initialized the output.
        let mount = unsafe { mount.assume_init() };
        let mount_flags = normalize_kernel_statfs_mount_flags(&filesystem, mount.f_flag)?;
        if mount.f_bsize != block_size
            || mount.f_frsize != fragment_size
            || mount.f_blocks != total_blocks
            || mount.f_files != total_files
            || mount.f_namemax != maximum_name_length
        {
            return Err(invalid_data(
                "cover statfs and statvfs static geometry differ",
            ));
        }
        let observed = Self {
            filesystem_type,
            block_size,
            total_blocks,
            total_files,
            maximum_name_length,
            fragment_size,
            mount_flags,
        };
        observed.validate()?;
        Ok(observed)
    }

    fn validate(self) -> io::Result<()> {
        if !matches!(
            self.filesystem_type,
            EXT4_SUPER_MAGIC | BTRFS_SUPER_MAGIC | F2FS_SUPER_MAGIC | XFS_SUPER_MAGIC
        ) {
            return Err(invalid_data("cover is not on a supported local filesystem"));
        }
        if self.block_size == 0 || self.maximum_name_length == 0 || self.fragment_size == 0 {
            return Err(invalid_data("cover filesystem geometry contains zero"));
        }
        if self.mount_flags & REQUIRED_MOUNT_FLAGS != REQUIRED_MOUNT_FLAGS {
            return Err(invalid_data("cover mount is not read-only and no-atime"));
        }
        Ok(())
    }

    fn encode(&self, output: &mut Vec<u8>) {
        put_i64(output, self.filesystem_type);
        put_u64(output, self.block_size);
        put_u64(output, self.total_blocks);
        put_u64(output, self.total_files);
        put_u64(output, self.maximum_name_length);
        put_u64(output, self.fragment_size);
        put_u64(output, self.mount_flags);
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct CanonicalMetadata {
    identity: FileIdentity,
    mask: u32,
    block_size: u32,
    attributes: u64,
    link_count: u32,
    uid: u32,
    gid: u32,
    mode: u16,
    inode: u64,
    size: u64,
    blocks: u64,
    attributes_mask: u64,
    access_time: KernelStatxTimestamp,
    birth_time: KernelStatxTimestamp,
    change_time: KernelStatxTimestamp,
    modification_time: KernelStatxTimestamp,
    rdev_major: u32,
    rdev_minor: u32,
    dev_major: u32,
    dev_minor: u32,
    mount_id: u64,
    unique_mount_mask: u32,
    unique_mount_id: u64,
    dio_memory_alignment: u32,
    dio_offset_alignment: u32,
    subvolume: u64,
    atomic_write_unit_min: u32,
    atomic_write_unit_max: u32,
    atomic_write_segments_max: u32,
    dio_read_offset_alignment: u32,
    atomic_write_unit_max_opt: u32,
}

impl CanonicalMetadata {
    fn read(descriptor: RawFd) -> io::Result<Self> {
        let raw = statx_empty_path(descriptor, BASE_STATX_MASK)?;
        validate_statx_response(&raw, false)?;
        let unique = statx_empty_path(descriptor, BASE_STATX_MASK | STATX_MNT_ID_UNIQUE)?;
        validate_statx_response(&unique, true)?;
        require_same_statx_observation(&raw, &unique)?;
        let identity = descriptor_identity(descriptor)?;
        if identity.inode != raw.inode
            || linux_device_major(identity.device) != raw.dev_major
            || linux_device_minor(identity.device) != raw.dev_minor
        {
            return Err(invalid_data("fstat and statx identity differ"));
        }
        Ok(Self {
            identity,
            mask: raw.mask,
            block_size: raw.block_size,
            attributes: raw.attributes,
            link_count: raw.link_count,
            uid: raw.uid,
            gid: raw.gid,
            mode: raw.mode,
            inode: raw.inode,
            size: raw.size,
            blocks: raw.blocks,
            attributes_mask: raw.attributes_mask,
            access_time: raw.access_time,
            birth_time: raw.birth_time,
            change_time: raw.change_time,
            modification_time: raw.modification_time,
            rdev_major: raw.rdev_major,
            rdev_minor: raw.rdev_minor,
            dev_major: raw.dev_major,
            dev_minor: raw.dev_minor,
            mount_id: raw.mount_id,
            unique_mount_mask: unique.mask,
            unique_mount_id: unique.mount_id,
            dio_memory_alignment: raw.dio_memory_alignment,
            dio_offset_alignment: raw.dio_offset_alignment,
            subvolume: raw.subvolume,
            atomic_write_unit_min: raw.atomic_write_unit_min,
            atomic_write_unit_max: raw.atomic_write_unit_max,
            atomic_write_segments_max: raw.atomic_write_segments_max,
            dio_read_offset_alignment: raw.dio_read_offset_alignment,
            atomic_write_unit_max_opt: raw.atomic_write_unit_max_opt,
        })
    }

    fn kind(&self) -> io::Result<CoverEntryKind> {
        match u32::from(self.mode) & libc::S_IFMT {
            libc::S_IFDIR => Ok(CoverEntryKind::Directory),
            libc::S_IFREG => Ok(CoverEntryKind::Placeholder),
            _ => Err(invalid_data(
                "cover entry is not a directory or regular placeholder",
            )),
        }
    }

    fn validate_root(&self) -> io::Result<()> {
        if self.kind()? != CoverEntryKind::Directory || self.identity.inode == 0 {
            return Err(invalid_data("cover root is not an identified directory"));
        }
        Ok(())
    }

    fn validate_entry(&self, kind: CoverEntryKind) -> io::Result<()> {
        if self.kind()? != kind || self.identity.inode == 0 {
            return Err(invalid_data(
                "cover dentry type and opened inode type differ",
            ));
        }
        if kind == CoverEntryKind::Placeholder && (self.size != 0 || self.link_count != 1) {
            return Err(invalid_data(
                "cover placeholder is nonempty or has a hard link",
            ));
        }
        Ok(())
    }

    /// Encode the complete source-epoch statx and descriptor identity snapshot.
    fn encode(&self, output: &mut Vec<u8>) {
        put_u64(output, self.identity.device);
        put_u64(output, self.identity.inode);
        put_u32(output, self.mask);
        put_u32(output, self.block_size);
        put_u64(output, self.attributes);
        put_u32(output, self.link_count);
        put_u32(output, self.uid);
        put_u32(output, self.gid);
        put_u16(output, self.mode);
        put_u64(output, self.inode);
        put_u64(output, self.size);
        put_u64(output, self.blocks);
        put_u64(output, self.attributes_mask);
        encode_timestamp(output, self.access_time);
        encode_timestamp(output, self.birth_time);
        encode_timestamp(output, self.change_time);
        encode_timestamp(output, self.modification_time);
        put_u32(output, self.rdev_major);
        put_u32(output, self.rdev_minor);
        put_u32(output, self.dev_major);
        put_u32(output, self.dev_minor);
        put_u64(output, self.mount_id);
        put_u32(output, self.unique_mount_mask);
        put_u64(output, self.unique_mount_id);
        put_u32(output, self.dio_memory_alignment);
        put_u32(output, self.dio_offset_alignment);
        put_u64(output, self.subvolume);
        put_u32(output, self.atomic_write_unit_min);
        put_u32(output, self.atomic_write_unit_max);
        put_u32(output, self.atomic_write_segments_max);
        put_u32(output, self.dio_read_offset_alignment);
        put_u32(output, self.atomic_write_unit_max_opt);
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct TreeRecord {
    kind: CoverEntryKind,
    relative_path: Vec<u8>,
    metadata: CanonicalMetadata,
    inode_policy: CanonicalInodePolicy,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct DirectoryEntry {
    inode: u64,
    name: Vec<u8>,
    dentry_type: u8,
}

struct InodeObservation {
    kind: CoverEntryKind,
    descriptor: File,
    metadata: CanonicalMetadata,
    inode_policy: CanonicalInodePolicy,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct CoverSnapshot {
    identity: FileIdentity,
    root_metadata: CanonicalMetadata,
    root_inode_policy: CanonicalInodePolicy,
    filesystem: StableFilesystemMetadata,
    metadata_sha256: [u8; 32],
    tree_sha256: [u8; 32],
    /// The exact bytewise-sorted inventory authenticated by `tree_sha256`.
    /// Records and their metadata stay private; callers can only make narrow,
    /// immutable membership queries below.
    records: Vec<TreeRecord>,
    relative_path_bytes: usize,
}

/// One exact, retained persistent loader cover.
#[derive(Clone, Debug)]
pub(crate) struct StableLoaderCover {
    canonical_path: PathBuf,
    root: Arc<File>,
    snapshot: CoverSnapshot,
}

impl StableLoaderCover {
    /// Open and verify one already-published cover directory.
    ///
    /// The expected digests and file identity are manifest data. The metadata
    /// digest covers the complete source root statx snapshot and the source
    /// filesystem's static geometry and policy. The tree digest covers that
    /// filesystem profile plus the root and every directory/placeholder record
    /// in bytewise pathname order. This component result is not runnable
    /// authorization until paired with the authenticated source-epoch receipt
    /// required for `absent-v1` and global immutability.
    pub(crate) fn open_verified(
        path: &Path,
        expected_identity: FileIdentity,
        expected_metadata_sha256: [u8; 32],
        expected_tree_sha256: [u8; 32],
    ) -> io::Result<Self> {
        let path_cstring = exact_absolute_path(path)?;
        let root = openat2_file(
            libc::AT_FDCWD,
            &path_cstring,
            libc::O_PATH | libc::O_CLOEXEC | libc::O_DIRECTORY,
            RESOLVE_NO_MAGICLINKS | RESOLVE_NO_SYMLINKS,
        )?;
        verify_path_descriptor_flags(root.as_raw_fd())?;
        let snapshot = scan_cover(root.as_raw_fd())?;
        require_expected_snapshot(
            &snapshot,
            expected_identity,
            expected_metadata_sha256,
            expected_tree_sha256,
        )?;

        // Reopen the exact absolute spelling after enumeration. This detects an
        // observed pathname rebind; the retained descriptor remains authoritative.
        let rebound = openat2_file(
            libc::AT_FDCWD,
            &path_cstring,
            libc::O_PATH | libc::O_CLOEXEC | libc::O_DIRECTORY,
            RESOLVE_NO_MAGICLINKS | RESOLVE_NO_SYMLINKS,
        )?;
        verify_path_descriptor_flags(rebound.as_raw_fd())?;
        let rebound_metadata = CanonicalMetadata::read(rebound.as_raw_fd())?;
        let rebound_filesystem = StableFilesystemMetadata::read(rebound.as_raw_fd())?;
        if rebound_metadata != snapshot.root_metadata || rebound_filesystem != snapshot.filesystem {
            return Err(invalid_data("cover pathname changed during verification"));
        }
        if observe_directory_inode_policy(
            rebound.as_raw_fd(),
            &rebound_filesystem,
            &rebound_metadata,
        )? != snapshot.root_inode_policy
        {
            return Err(invalid_data(
                "cover pathname inode policy changed during verification",
            ));
        }

        Ok(Self {
            canonical_path: path.to_path_buf(),
            root: Arc::new(root),
            snapshot,
        })
    }

    pub(crate) fn canonical_path(&self) -> &Path {
        &self.canonical_path
    }

    pub(crate) const fn identity(&self) -> FileIdentity {
        self.snapshot.identity
    }

    pub(crate) const fn metadata_sha256(&self) -> [u8; 32] {
        self.snapshot.metadata_sha256
    }

    pub(crate) const fn tree_sha256(&self) -> [u8; 32] {
        self.snapshot.tree_sha256
    }

    pub(crate) fn entry_count(&self) -> usize {
        self.snapshot.records.len()
    }

    /// Whether the exact nonempty relative path names an authenticated cover
    /// directory.  This lets the schema binder prove every target ancestor.
    pub(crate) fn contains_directory(&self, relative_path: &[u8]) -> io::Result<bool> {
        validate_relative_path(relative_path)?;
        Ok(find_tree_record(&self.snapshot.records, relative_path)
            .is_some_and(|record| record.kind == CoverEntryKind::Directory))
    }

    /// Whether the exact nonempty relative path names an authenticated, empty,
    /// single-link regular placeholder.
    pub(crate) fn contains_empty_placeholder(&self, relative_path: &[u8]) -> io::Result<bool> {
        validate_relative_path(relative_path)?;
        Ok(find_tree_record(&self.snapshot.records, relative_path)
            .is_some_and(|record| record.kind == CoverEntryKind::Placeholder))
    }

    /// Prove that an exact nonempty relative path is absent while every parent
    /// component names an authenticated directory.
    ///
    /// This is the fail-closed absence operation for paths such as
    /// `etc/ld.so.preload`. Malformed aliases are errors, not absence, and a
    /// missing or non-directory ancestor is not accepted as the requested
    /// final-component absence proof.
    pub(crate) fn proves_absent_with_directory_ancestors(
        &self,
        relative_path: &[u8],
    ) -> io::Result<bool> {
        proves_absent_with_directory_ancestors(&self.snapshot.records, relative_path)
    }

    /// Return every authenticated placeholder pathname in canonical bytewise
    /// order.  Filtering a sorted inventory preserves ordering, so a schema
    /// binder can compare this iterator exactly with its sorted guest targets.
    pub(crate) fn empty_placeholder_paths(&self) -> impl Iterator<Item = &[u8]> + '_ {
        empty_placeholder_paths(&self.snapshot.records)
    }

    /// Compare the complete authenticated placeholder inventory with one
    /// caller-supplied canonical bytewise sequence.  Missing, extra, duplicate,
    /// or out-of-order target paths all fail closed.
    pub(crate) fn has_exact_empty_placeholder_paths<'a>(
        &self,
        expected: impl IntoIterator<Item = &'a [u8]>,
    ) -> io::Result<bool> {
        has_exact_empty_placeholder_paths(&self.snapshot.records, expected)
    }

    /// Revalidate both the retained source and its exact published pathname.
    pub(crate) fn revalidate_source(&self) -> io::Result<()> {
        verify_path_descriptor_flags(self.root.as_raw_fd())?;
        let observed = scan_cover(self.root.as_raw_fd())?;
        if observed != self.snapshot {
            return Err(invalid_data("retained cover changed after verification"));
        }

        let path_cstring = exact_absolute_path(&self.canonical_path)?;
        let rebound = openat2_file(
            libc::AT_FDCWD,
            &path_cstring,
            libc::O_PATH | libc::O_CLOEXEC | libc::O_DIRECTORY,
            RESOLVE_NO_MAGICLINKS | RESOLVE_NO_SYMLINKS,
        )?;
        verify_path_descriptor_flags(rebound.as_raw_fd())?;
        let rebound_metadata = CanonicalMetadata::read(rebound.as_raw_fd())?;
        let rebound_filesystem = StableFilesystemMetadata::read(rebound.as_raw_fd())?;
        if rebound_metadata != self.snapshot.root_metadata
            || rebound_filesystem != self.snapshot.filesystem
            || observe_directory_inode_policy(
                rebound.as_raw_fd(),
                &rebound_filesystem,
                &rebound_metadata,
            )? != self.snapshot.root_inode_policy
        {
            return Err(invalid_data("cover pathname changed after verification"));
        }
        Ok(())
    }

    /// Duplicate the retained source for the lower prepared mount epoch.
    ///
    /// No raw descriptor number or procfd spelling escapes this API. The caller
    /// receives an owned descriptor after a fresh complete source revalidation
    /// and must retain it through the mount and positive target-receipt checks.
    pub(crate) fn duplicate_revalidated_source(&self) -> io::Result<File> {
        self.revalidate_source()?;
        self.root.try_clone()
    }
}

fn find_tree_record<'a>(records: &'a [TreeRecord], relative_path: &[u8]) -> Option<&'a TreeRecord> {
    records
        .binary_search_by(|record| record.relative_path.as_slice().cmp(relative_path))
        .ok()
        .map(|index| &records[index])
}

fn empty_placeholder_paths(records: &[TreeRecord]) -> impl Iterator<Item = &[u8]> + '_ {
    records
        .iter()
        .filter(|record| record.kind == CoverEntryKind::Placeholder)
        .map(|record| record.relative_path.as_slice())
}

fn proves_absent_with_directory_ancestors(
    records: &[TreeRecord],
    relative_path: &[u8],
) -> io::Result<bool> {
    validate_relative_path(relative_path)?;
    if find_tree_record(records, relative_path).is_some() {
        return Ok(false);
    }
    for (index, byte) in relative_path.iter().enumerate() {
        if *byte == b'/'
            && !find_tree_record(records, &relative_path[..index])
                .is_some_and(|record| record.kind == CoverEntryKind::Directory)
        {
            return Ok(false);
        }
    }
    Ok(true)
}

fn has_exact_empty_placeholder_paths<'a>(
    records: &[TreeRecord],
    expected: impl IntoIterator<Item = &'a [u8]>,
) -> io::Result<bool> {
    let mut observed = empty_placeholder_paths(records);
    let mut previous = None::<&'a [u8]>;
    let mut exact = true;
    for path in expected {
        validate_relative_path(path)?;
        if previous.is_some_and(|previous| previous >= path) {
            return Err(invalid_data(
                "expected placeholder paths are duplicated or not strictly sorted",
            ));
        }
        if observed.next() != Some(path) {
            exact = false;
        }
        previous = Some(path);
    }
    Ok(exact && observed.next().is_none())
}

fn exact_absolute_path(path: &Path) -> io::Result<CString> {
    let bytes = path.as_os_str().as_bytes();
    if !path.is_absolute()
        || bytes.is_empty()
        || bytes.contains(&0)
        || bytes.len() > 1 && bytes.ends_with(b"/")
        || bytes.windows(2).any(|pair| pair == b"//")
        || bytes
            .split(|byte| *byte == b'/')
            .skip(1)
            .any(|component| component.is_empty() || component == b"." || component == b"..")
    {
        return Err(invalid_data(
            "cover path is not an exact lexically canonical absolute path",
        ));
    }
    CString::new(bytes).map_err(|_| invalid_data("cover path contains a NUL byte"))
}

fn validate_relative_path(path: &[u8]) -> io::Result<()> {
    if path.is_empty()
        || path.starts_with(b"/")
        || path.ends_with(b"/")
        || path.contains(&0)
        || path.windows(2).any(|pair| pair == b"//")
        || path
            .split(|byte| *byte == b'/')
            .any(|component| component.is_empty() || component == b"." || component == b"..")
    {
        return Err(invalid_data("cover relative path is not canonical"));
    }
    Ok(())
}

fn openat2_file(
    directory: RawFd,
    path: &CStr,
    flags: libc::c_int,
    resolve: u64,
) -> io::Result<File> {
    let how = OpenHow {
        flags: flags as u64,
        mode: 0,
        resolve,
    };
    // SAFETY: `path` is NUL terminated, `how` has the Linux open_how layout,
    // and a successful returned descriptor is transferred to exactly one File.
    let result = unsafe {
        libc::syscall(
            libc::SYS_openat2,
            directory,
            path.as_ptr(),
            &raw const how,
            std::mem::size_of::<OpenHow>(),
        )
    };
    if result < 0 {
        return Err(io::Error::last_os_error());
    }
    let descriptor = i32::try_from(result)
        .map_err(|_| invalid_data("openat2 returned an unrepresentable descriptor"))?;
    Ok(unsafe { File::from_raw_fd(descriptor) })
}

fn open_relative_path(directory: RawFd, name: &[u8], directory_only: bool) -> io::Result<File> {
    validate_dentry_name(name)?;
    let name = CString::new(name).map_err(|_| invalid_data("cover dentry contains NUL"))?;
    let type_flag = if directory_only { libc::O_DIRECTORY } else { 0 };
    openat2_file(
        directory,
        &name,
        libc::O_PATH | libc::O_CLOEXEC | type_flag,
        RESOLVE_BENEATH | RESOLVE_NO_XDEV | RESOLVE_NO_MAGICLINKS | RESOLVE_NO_SYMLINKS,
    )
}

fn open_enumeration_descriptor(directory: RawFd) -> io::Result<File> {
    openat2_file(
        directory,
        c".",
        libc::O_RDONLY | libc::O_CLOEXEC | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_NONBLOCK,
        RESOLVE_BENEATH | RESOLVE_NO_XDEV | RESOLVE_NO_MAGICLINKS | RESOLVE_NO_SYMLINKS,
    )
}

fn observe_directory_inode_policy(
    directory: RawFd,
    filesystem: &StableFilesystemMetadata,
    metadata: &CanonicalMetadata,
) -> io::Result<CanonicalInodePolicy> {
    let descriptor = open_enumeration_descriptor(directory)?;
    verify_enumeration_descriptor_flags(descriptor.as_raw_fd())?;
    if CanonicalMetadata::read(descriptor.as_raw_fd())? != *metadata {
        return Err(invalid_data(
            "cover directory observation descriptor differs from its path pin",
        ));
    }
    let policy = CanonicalInodePolicy::read(
        descriptor.as_raw_fd(),
        filesystem,
        metadata,
        CoverEntryKind::Directory,
    )?;
    verify_inode_observation(
        descriptor.as_raw_fd(),
        filesystem,
        metadata,
        &policy,
        CoverEntryKind::Directory,
    )?;
    Ok(policy)
}

fn open_placeholder_observation_descriptor(directory: RawFd, name: &[u8]) -> io::Result<File> {
    validate_dentry_name(name)?;
    let name = CString::new(name).map_err(|_| invalid_data("cover dentry contains NUL"))?;
    openat2_file(
        directory,
        &name,
        libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK,
        RESOLVE_BENEATH | RESOLVE_NO_XDEV | RESOLVE_NO_MAGICLINKS | RESOLVE_NO_SYMLINKS,
    )
}

fn verify_path_descriptor_flags(descriptor: RawFd) -> io::Result<()> {
    let status = unsafe { libc::fcntl(descriptor, libc::F_GETFL) };
    if status < 0 {
        return Err(io::Error::last_os_error());
    }
    let descriptor_flags = unsafe { libc::fcntl(descriptor, libc::F_GETFD) };
    if descriptor_flags < 0 {
        return Err(io::Error::last_os_error());
    }
    if status & libc::O_PATH != libc::O_PATH || descriptor_flags & libc::FD_CLOEXEC == 0 {
        return Err(invalid_data(
            "cover descriptor is not O_PATH and close-on-exec",
        ));
    }
    Ok(())
}

fn verify_enumeration_descriptor_flags(descriptor: RawFd) -> io::Result<()> {
    let status = unsafe { libc::fcntl(descriptor, libc::F_GETFL) };
    if status < 0 {
        return Err(io::Error::last_os_error());
    }
    let descriptor_flags = unsafe { libc::fcntl(descriptor, libc::F_GETFD) };
    if descriptor_flags < 0 {
        return Err(io::Error::last_os_error());
    }
    if status & libc::O_PATH != 0
        || status & libc::O_ACCMODE != libc::O_RDONLY
        || status & libc::O_NONBLOCK == 0
        || descriptor_flags & libc::FD_CLOEXEC == 0
    {
        return Err(invalid_data(
            "cover enumeration descriptor is not read-only and close-on-exec",
        ));
    }
    Ok(())
}

fn verify_placeholder_observation_descriptor_flags(descriptor: RawFd) -> io::Result<()> {
    let status = unsafe { libc::fcntl(descriptor, libc::F_GETFL) };
    if status < 0 {
        return Err(io::Error::last_os_error());
    }
    let descriptor_flags = unsafe { libc::fcntl(descriptor, libc::F_GETFD) };
    if descriptor_flags < 0 {
        return Err(io::Error::last_os_error());
    }
    if status & libc::O_PATH != 0
        || status & libc::O_ACCMODE != libc::O_RDONLY
        || status & libc::O_NONBLOCK == 0
        || descriptor_flags & libc::FD_CLOEXEC == 0
    {
        return Err(invalid_data(
            "cover placeholder observation descriptor is not read-only and close-on-exec",
        ));
    }
    Ok(())
}

/// Interpret one caller-visible `flistxattr(fd, NULL, 0)` result.
///
/// A positive size or any error fails.  Exact zero is only a local consistency
/// check; it cannot establish all-namespace `absent-v1` without the separate
/// authenticated source-epoch authority receipt.
fn require_caller_visible_xattr_zero_result(result: Result<usize, i32>) -> io::Result<()> {
    match result {
        Ok(0) => Ok(()),
        Ok(_) => Err(invalid_data(
            "cover inode has a caller-visible extended attribute",
        )),
        Err(_) => Err(invalid_data(
            "cover inode caller-visible extended-attribute list could not be checked",
        )),
    }
}

fn list_caller_visible_xattrs(descriptor: RawFd) -> Result<usize, i32> {
    // SAFETY: a null buffer and zero length ask Linux only for the exact list
    // length; `descriptor` remains owned by the caller for the duration.
    let result = unsafe { libc::flistxattr(descriptor, std::ptr::null_mut(), 0) };
    if result < 0 {
        Err(io::Error::last_os_error()
            .raw_os_error()
            .unwrap_or(libc::EIO))
    } else {
        Ok(result as usize)
    }
}

/// Revalidate the statx and ioctl policy of a descriptor-capable nofollow open
/// around one caller-visible xattr-list observation.  A successful local zero
/// is not accepted across an observed identity, metadata, or policy transition.
fn verify_inode_observation(
    descriptor: RawFd,
    filesystem: &StableFilesystemMetadata,
    expected_metadata: &CanonicalMetadata,
    expected_inode_policy: &CanonicalInodePolicy,
    kind: CoverEntryKind,
) -> io::Result<()> {
    if CanonicalMetadata::read(descriptor)? != *expected_metadata {
        return Err(invalid_data(
            "cover observation descriptor differs from pinned metadata",
        ));
    }
    if CanonicalInodePolicy::read(descriptor, filesystem, expected_metadata, kind)?
        != *expected_inode_policy
    {
        return Err(invalid_data(
            "cover inode policy differs from the authenticated snapshot",
        ));
    }
    require_caller_visible_xattr_zero_result(list_caller_visible_xattrs(descriptor))?;
    if CanonicalInodePolicy::read(descriptor, filesystem, expected_metadata, kind)?
        != *expected_inode_policy
    {
        return Err(invalid_data(
            "cover inode policy changed while listing caller-visible xattrs",
        ));
    }
    if CanonicalMetadata::read(descriptor)? != *expected_metadata {
        return Err(invalid_data(
            "cover metadata changed while listing caller-visible xattrs",
        ));
    }
    Ok(())
}

fn statx_empty_path(descriptor: RawFd, mask: u32) -> io::Result<KernelStatx> {
    let mut value = std::mem::MaybeUninit::<KernelStatx>::zeroed();
    let result = unsafe {
        libc::syscall(
            libc::SYS_statx,
            descriptor,
            c"".as_ptr(),
            libc::AT_EMPTY_PATH | libc::AT_NO_AUTOMOUNT | libc::AT_STATX_FORCE_SYNC,
            mask,
            value.as_mut_ptr(),
        )
    };
    if result != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(unsafe { value.assume_init() })
}

fn validate_statx_response(value: &KernelStatx, require_unique_mount: bool) -> io::Result<()> {
    let required = if require_unique_mount {
        (REQUIRED_STATX_MASK & !STATX_MNT_ID) | STATX_MNT_ID_UNIQUE
    } else {
        REQUIRED_STATX_MASK
    };
    if value.mask & required != required
        || !require_unique_mount && value.mask & STATX_MNT_ID_UNIQUE != 0
        || value.mask & !KNOWN_STATX_MASK != 0
        || value.attributes_mask & !KNOWN_STATX_ATTRIBUTES != 0
        || value.attributes & !value.attributes_mask != 0
        || value.spare0 != [0]
        || value.spare2 != [0]
        || value.spare3 != [0; 8]
        || [
            value.access_time,
            value.birth_time,
            value.change_time,
            value.modification_time,
        ]
        .iter()
        .any(|timestamp| timestamp.nanoseconds >= 1_000_000_000 || timestamp.reserved != 0)
    {
        return Err(invalid_data(
            "statx response is incomplete or has unknown fields",
        ));
    }
    Ok(())
}

fn require_same_statx_observation(base: &KernelStatx, unique: &KernelStatx) -> io::Result<()> {
    let mut normalized_base = *base;
    let mut normalized_unique = *unique;
    normalized_base.mask &= !(STATX_MNT_ID | STATX_MNT_ID_UNIQUE);
    normalized_unique.mask &= !(STATX_MNT_ID | STATX_MNT_ID_UNIQUE);
    normalized_base.mount_id = 0;
    normalized_unique.mount_id = 0;
    if normalized_unique != normalized_base {
        return Err(invalid_data(
            "cover metadata changed across statx mount-id queries",
        ));
    }
    Ok(())
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
fn raw_fstatfs_x86_64(descriptor: RawFd) -> io::Result<KernelStatfsX86_64> {
    let mut filesystem = std::mem::MaybeUninit::<KernelStatfsX86_64>::uninit();
    // SAFETY: the UAPI structure contains only integer fields, so every bit
    // pattern is valid.  Poisoning its complete, padding-free 120-byte layout
    // makes an incomplete kernel write fail the reserved-tail check instead of
    // inheriting caller-supplied zeroes.
    unsafe {
        std::ptr::write_bytes(
            filesystem.as_mut_ptr().cast::<u8>(),
            0xa5,
            std::mem::size_of::<KernelStatfsX86_64>(),
        );
    }
    // SAFETY: this is the exact Linux x86-64 `SYS_fstatfs` ABI, `descriptor`
    // is live for the call, and the 120-byte result buffer is writable.
    if unsafe { libc::syscall(libc::SYS_fstatfs, descriptor, filesystem.as_mut_ptr()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: every byte was initialized with poison before the syscall.  The
    // caller separately rejects an unchanged or otherwise nonzero spare tail.
    Ok(unsafe { filesystem.assume_init() })
}

#[cfg(not(all(target_os = "linux", target_arch = "x86_64")))]
fn raw_fstatfs_x86_64(_descriptor: RawFd) -> io::Result<KernelStatfsX86_64> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "stable cover requires the reviewed Linux x86-64 statfs ABI",
    ))
}

fn normalize_kernel_statfs_mount_flags(
    filesystem: &KernelStatfsX86_64,
    statvfs_flags: u64,
) -> io::Result<u64> {
    let raw_flags = nonnegative_filesystem_word(
        filesystem.mount_flags,
        "cover statfs mount flags are invalid",
    )?;
    if raw_flags & ST_VALID_FLAG == 0 {
        return Err(invalid_data("cover statfs mount flags lack ST_VALID"));
    }
    if filesystem.spare != [0; 4] {
        return Err(invalid_data("cover statfs returned nonzero reserved words"));
    }
    let normalized = raw_flags ^ ST_VALID_FLAG;
    if normalized != statvfs_flags {
        return Err(invalid_data("cover statfs and statvfs mount flags differ"));
    }
    Ok(normalized)
}

fn nonnegative_filesystem_word(value: i64, message: &'static str) -> io::Result<u64> {
    u64::try_from(value).map_err(|_| invalid_data(message))
}

fn descriptor_identity(descriptor: RawFd) -> io::Result<FileIdentity> {
    let mut value = std::mem::MaybeUninit::<libc::stat>::zeroed();
    if unsafe { libc::fstat(descriptor, value.as_mut_ptr()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    let value = unsafe { value.assume_init() };
    Ok(FileIdentity {
        device: value.st_dev,
        inode: value.st_ino,
    })
}

fn linux_device_major(device: u64) -> u32 {
    (((device >> 8) & 0x0000_0fff) | ((device >> 32) & 0xffff_f000)) as u32
}

fn linux_device_minor(device: u64) -> u32 {
    ((device & 0x0000_00ff) | ((device >> 12) & 0xffff_ff00)) as u32
}

fn scan_cover(root: RawFd) -> io::Result<CoverSnapshot> {
    let filesystem_before = StableFilesystemMetadata::read(root)?;
    let before = CanonicalMetadata::read(root)?;
    before.validate_root()?;
    let mut records = Vec::new();
    let mut identities = BTreeSet::from([before.identity]);
    let mut relative_path_bytes = 0usize;
    let mut inode_observations = Vec::new();
    let root_inode_policy = scan_directory(
        root,
        &filesystem_before,
        &[],
        before.mount_id,
        before.unique_mount_id,
        0,
        &mut records,
        &mut identities,
        &mut relative_path_bytes,
        &mut inode_observations,
    )?;
    // Every descriptor-capable nofollow pin remains open through the complete
    // walk.  Recheck all of them after traversal, rather than treating a local
    // post-child check as the end of whole-tree verification.
    for observation in &inode_observations {
        match observation.kind {
            CoverEntryKind::Directory => {
                verify_enumeration_descriptor_flags(observation.descriptor.as_raw_fd())?;
            }
            CoverEntryKind::Placeholder => {
                verify_placeholder_observation_descriptor_flags(
                    observation.descriptor.as_raw_fd(),
                )?;
            }
        }
        verify_inode_observation(
            observation.descriptor.as_raw_fd(),
            &filesystem_before,
            &observation.metadata,
            &observation.inode_policy,
            observation.kind,
        )?;
    }
    let after = CanonicalMetadata::read(root)?;
    let filesystem_after = StableFilesystemMetadata::read(root)?;
    if after != before || filesystem_after != filesystem_before {
        return Err(invalid_data("cover root changed while enumerating tree"));
    }
    // Depth-first directory order is not globally bytewise order when, for
    // example, a directory `a` and a sibling `a-` both exist. Canonicalize the
    // complete relative paths only after the descriptor-relative walk.
    records.sort_by(|left, right| left.relative_path.cmp(&right.relative_path));
    let metadata_sha256 = metadata_digest(&filesystem_before, &before, &root_inode_policy);
    let tree_sha256 = tree_digest(&filesystem_before, &before, &root_inode_policy, &records)?;
    Ok(CoverSnapshot {
        identity: before.identity,
        root_metadata: before,
        root_inode_policy,
        filesystem: filesystem_before,
        metadata_sha256,
        tree_sha256,
        records,
        relative_path_bytes,
    })
}

fn scan_directory(
    directory: RawFd,
    filesystem: &StableFilesystemMetadata,
    prefix: &[u8],
    root_mount_id: u64,
    root_unique_mount_id: u64,
    depth: usize,
    records: &mut Vec<TreeRecord>,
    identities: &mut BTreeSet<FileIdentity>,
    relative_path_bytes: &mut usize,
    inode_observations: &mut Vec<InodeObservation>,
) -> io::Result<CanonicalInodePolicy> {
    if depth > MAX_TREE_ENTRIES {
        return Err(invalid_data("cover tree recursion exceeds entry bound"));
    }
    let before = CanonicalMetadata::read(directory)?;
    if before.kind()? != CoverEntryKind::Directory
        || before.mount_id != root_mount_id
        || before.unique_mount_id != root_unique_mount_id
    {
        return Err(invalid_data(
            "cover directory crossed a mount or changed type",
        ));
    }
    let enumeration = open_enumeration_descriptor(directory)?;
    verify_enumeration_descriptor_flags(enumeration.as_raw_fd())?;
    if CanonicalMetadata::read(enumeration.as_raw_fd())? != before {
        return Err(invalid_data(
            "cover enumeration descriptor differs from directory pin",
        ));
    }
    let inode_policy = CanonicalInodePolicy::read(
        enumeration.as_raw_fd(),
        filesystem,
        &before,
        CoverEntryKind::Directory,
    )?;
    verify_inode_observation(
        enumeration.as_raw_fd(),
        filesystem,
        &before,
        &inode_policy,
        CoverEntryKind::Directory,
    )?;
    let entries = read_directory_entries(enumeration.as_raw_fd())?;

    for entry in entries {
        let name = entry.name;
        let dentry_type = entry.dentry_type;
        let kind = match dentry_type {
            value if value == libc::DT_DIR => CoverEntryKind::Directory,
            value if value == libc::DT_REG => CoverEntryKind::Placeholder,
            value if value == libc::DT_UNKNOWN => {
                return Err(invalid_data("cover dentry has an unknown type"));
            }
            _ => return Err(invalid_data("cover contains a symlink or special file")),
        };
        let mut relative_path = Vec::new();
        relative_path
            .try_reserve(prefix.len().saturating_add(name.len()).saturating_add(1))
            .map_err(|error| io::Error::other(format!("reserve cover path: {error}")))?;
        if !prefix.is_empty() {
            relative_path.extend_from_slice(prefix);
            relative_path.push(b'/');
        }
        relative_path.extend_from_slice(&name);
        validate_relative_path(&relative_path)?;
        if records.len() >= MAX_TREE_ENTRIES {
            return Err(invalid_data("cover tree exceeds entry-count bound"));
        }
        *relative_path_bytes = relative_path_bytes
            .checked_add(relative_path.len())
            .ok_or_else(|| invalid_data("cover relative-path byte count overflow"))?;
        if *relative_path_bytes > MAX_RELATIVE_PATH_BYTES {
            return Err(invalid_data("cover tree exceeds relative-path byte bound"));
        }

        let child = open_relative_path(directory, &name, kind == CoverEntryKind::Directory)?;
        verify_path_descriptor_flags(child.as_raw_fd())?;
        let child_before = CanonicalMetadata::read(child.as_raw_fd())?;
        child_before.validate_entry(kind)?;
        require_enumerated_inode(entry.inode, child_before.inode)?;
        if child_before.mount_id != root_mount_id
            || child_before.unique_mount_id != root_unique_mount_id
        {
            return Err(invalid_data("cover child crosses the root mount"));
        }
        insert_unique_identity(identities, child_before.identity)?;
        let (child_inode_policy, placeholder_observation) = if kind == CoverEntryKind::Placeholder {
            let descriptor = open_placeholder_observation_descriptor(directory, &name)?;
            verify_placeholder_observation_descriptor_flags(descriptor.as_raw_fd())?;
            let policy = CanonicalInodePolicy::read(
                descriptor.as_raw_fd(),
                filesystem,
                &child_before,
                kind,
            )?;
            verify_inode_observation(
                descriptor.as_raw_fd(),
                filesystem,
                &child_before,
                &policy,
                kind,
            )?;
            (policy, Some(descriptor))
        } else {
            let policy = scan_directory(
                child.as_raw_fd(),
                filesystem,
                &relative_path,
                root_mount_id,
                root_unique_mount_id,
                depth + 1,
                records,
                identities,
                relative_path_bytes,
                inode_observations,
            )?;
            (policy, None)
        };
        if records.len() >= MAX_TREE_ENTRIES {
            return Err(invalid_data("cover tree exceeds entry-count bound"));
        }
        records.push(TreeRecord {
            kind,
            relative_path: relative_path.clone(),
            metadata: child_before.clone(),
            inode_policy: child_inode_policy.clone(),
        });
        if let Some(descriptor) = &placeholder_observation {
            verify_inode_observation(
                descriptor.as_raw_fd(),
                filesystem,
                &child_before,
                &child_inode_policy,
                kind,
            )?;
        }
        let child_after = CanonicalMetadata::read(child.as_raw_fd())?;
        if child_after != child_before {
            return Err(invalid_data("cover child changed while enumerating tree"));
        }
        if let Some(descriptor) = placeholder_observation {
            inode_observations.push(InodeObservation {
                kind,
                descriptor,
                metadata: child_before,
                inode_policy: child_inode_policy,
            });
        }
    }
    verify_inode_observation(
        enumeration.as_raw_fd(),
        filesystem,
        &before,
        &inode_policy,
        CoverEntryKind::Directory,
    )?;
    let after = CanonicalMetadata::read(directory)?;
    if after != before {
        return Err(invalid_data(
            "cover directory changed while enumerating children",
        ));
    }
    inode_observations.push(InodeObservation {
        kind: CoverEntryKind::Directory,
        descriptor: enumeration,
        metadata: before,
        inode_policy: inode_policy.clone(),
    });
    Ok(inode_policy)
}

fn read_directory_entries(descriptor: RawFd) -> io::Result<Vec<DirectoryEntry>> {
    let mut result = Vec::new();
    let mut names = BTreeSet::new();
    let mut name_bytes = 0usize;
    let mut buffer = [0u8; GETDENTS_BUFFER_BYTES];
    loop {
        let count = unsafe {
            libc::syscall(
                libc::SYS_getdents64,
                descriptor,
                buffer.as_mut_ptr(),
                buffer.len(),
            )
        };
        if count < 0 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(error);
        }
        if count == 0 {
            break;
        }
        let count = usize::try_from(count)
            .map_err(|_| invalid_data("getdents64 byte count is not representable"))?;
        let mut offset = 0usize;
        while offset < count {
            if count - offset < 20 {
                return Err(invalid_data("getdents64 record is truncated"));
            }
            let record = &buffer[offset..count];
            let inode = u64::from_ne_bytes(record[..8].try_into().unwrap());
            let record_length = usize::from(u16::from_ne_bytes(record[16..18].try_into().unwrap()));
            if inode == 0 || record_length < 20 || record_length > record.len() {
                return Err(invalid_data("getdents64 record length or inode is invalid"));
            }
            let dentry_type = record[18];
            let name_region = &record[19..record_length];
            let nul = name_region
                .iter()
                .position(|byte| *byte == 0)
                .ok_or_else(|| invalid_data("getdents64 name is unterminated"))?;
            let name = &name_region[..nul];
            if name == b"." || name == b".." {
                if dentry_type != libc::DT_DIR {
                    return Err(invalid_data("dot dentry is not a directory"));
                }
            } else {
                validate_dentry_name(name)?;
                if result.len() >= MAX_TREE_ENTRIES {
                    return Err(invalid_data("cover directory exceeds entry-count bound"));
                }
                name_bytes = name_bytes
                    .checked_add(name.len())
                    .ok_or_else(|| invalid_data("cover dentry-name byte count overflow"))?;
                if name_bytes > MAX_RELATIVE_PATH_BYTES {
                    return Err(invalid_data(
                        "cover directory exceeds dentry-name byte bound",
                    ));
                }
                if !names.insert(name.to_vec()) {
                    return Err(invalid_data("cover directory repeats one dentry name"));
                }
                result.push(DirectoryEntry {
                    inode,
                    name: name.to_vec(),
                    dentry_type,
                });
            }
            offset = offset
                .checked_add(record_length)
                .ok_or_else(|| invalid_data("getdents64 offset overflow"))?;
        }
        if offset != count {
            return Err(invalid_data(
                "getdents64 records do not fill returned bytes",
            ));
        }
    }
    result.sort_by(|left, right| left.name.cmp(&right.name));
    Ok(result)
}

fn insert_unique_identity(
    identities: &mut BTreeSet<FileIdentity>,
    identity: FileIdentity,
) -> io::Result<()> {
    if !identities.insert(identity) {
        return Err(invalid_data("cover tree aliases an inode identity"));
    }
    Ok(())
}

fn require_enumerated_inode(enumerated: u64, opened: u64) -> io::Result<()> {
    if enumerated != opened {
        return Err(invalid_data(
            "opened cover child differs from enumerated dentry inode",
        ));
    }
    Ok(())
}

fn validate_dentry_name(name: &[u8]) -> io::Result<()> {
    if name.is_empty() || name == b"." || name == b".." || name.contains(&0) || name.contains(&b'/')
    {
        return Err(invalid_data("cover dentry name is not canonical"));
    }
    Ok(())
}

fn metadata_digest(
    filesystem: &StableFilesystemMetadata,
    metadata: &CanonicalMetadata,
    inode_policy: &CanonicalInodePolicy,
) -> [u8; 32] {
    metadata_digest_with_policy(
        STABLE_COVER_XATTR_POLICY_BYTES,
        filesystem,
        metadata,
        inode_policy,
    )
}

fn metadata_digest_with_policy(
    xattr_policy: &[u8],
    filesystem: &StableFilesystemMetadata,
    metadata: &CanonicalMetadata,
    inode_policy: &CanonicalInodePolicy,
) -> [u8; 32] {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(METADATA_DIGEST_DOMAIN);
    encode_xattr_policy(&mut bytes, xattr_policy);
    filesystem.encode(&mut bytes);
    metadata.encode(&mut bytes);
    inode_policy.encode(&mut bytes);
    Sha256::digest(bytes).into()
}

fn tree_digest(
    filesystem: &StableFilesystemMetadata,
    root: &CanonicalMetadata,
    root_inode_policy: &CanonicalInodePolicy,
    records: &[TreeRecord],
) -> io::Result<[u8; 32]> {
    tree_digest_with_policy(
        STABLE_COVER_XATTR_POLICY_BYTES,
        filesystem,
        root,
        root_inode_policy,
        records,
    )
}

fn tree_digest_with_policy(
    xattr_policy: &[u8],
    filesystem: &StableFilesystemMetadata,
    root: &CanonicalMetadata,
    root_inode_policy: &CanonicalInodePolicy,
    records: &[TreeRecord],
) -> io::Result<[u8; 32]> {
    if records.len() > MAX_TREE_ENTRIES {
        return Err(invalid_data("cover tree exceeds entry-count bound"));
    }
    let mut previous = None::<&[u8]>;
    let mut path_bytes = 0usize;
    let mut bytes = Vec::new();
    bytes.extend_from_slice(TREE_DIGEST_DOMAIN);
    encode_xattr_policy(&mut bytes, xattr_policy);
    filesystem.encode(&mut bytes);
    bytes.push(b'r');
    put_u32(&mut bytes, 0);
    root.encode(&mut bytes);
    root_inode_policy.encode(&mut bytes);
    for record in records {
        validate_relative_path(&record.relative_path)?;
        record.metadata.validate_entry(record.kind)?;
        if previous.is_some_and(|value| value >= record.relative_path.as_slice()) {
            return Err(invalid_data(
                "cover records are duplicated or not in canonical byte order",
            ));
        }
        previous = Some(record.relative_path.as_slice());
        path_bytes = path_bytes
            .checked_add(record.relative_path.len())
            .ok_or_else(|| invalid_data("cover relative-path byte count overflow"))?;
        if path_bytes > MAX_RELATIVE_PATH_BYTES {
            return Err(invalid_data("cover tree exceeds relative-path byte bound"));
        }
        bytes.push(record.kind.digest_tag());
        put_u32(
            &mut bytes,
            u32::try_from(record.relative_path.len())
                .map_err(|_| invalid_data("cover relative path length is not representable"))?,
        );
        bytes.extend_from_slice(&record.relative_path);
        record.metadata.encode(&mut bytes);
        record.inode_policy.encode(&mut bytes);
    }
    Ok(Sha256::digest(bytes).into())
}

fn encode_xattr_policy(output: &mut Vec<u8>, xattr_policy: &[u8]) {
    put_u32(
        output,
        u32::try_from(xattr_policy.len()).expect("the fixed xattr policy length fits u32"),
    );
    output.extend_from_slice(xattr_policy);
}

fn require_expected_snapshot(
    observed: &CoverSnapshot,
    expected_identity: FileIdentity,
    expected_metadata_sha256: [u8; 32],
    expected_tree_sha256: [u8; 32],
) -> io::Result<()> {
    if observed.identity != expected_identity
        || observed.metadata_sha256 != expected_metadata_sha256
        || observed.tree_sha256 != expected_tree_sha256
    {
        return Err(invalid_data(
            "cover identity, metadata digest, or tree digest differs",
        ));
    }
    Ok(())
}

fn encode_timestamp(output: &mut Vec<u8>, value: KernelStatxTimestamp) {
    put_i64(output, value.seconds);
    put_u32(output, value.nanoseconds);
}

fn put_u16(output: &mut Vec<u8>, value: u16) {
    output.extend_from_slice(&value.to_le_bytes());
}

fn put_u32(output: &mut Vec<u8>, value: u32) {
    output.extend_from_slice(&value.to_le_bytes());
}

fn put_u64(output: &mut Vec<u8>, value: u64) {
    output.extend_from_slice(&value.to_le_bytes());
}

fn put_i64(output: &mut Vec<u8>, value: i64) {
    output.extend_from_slice(&value.to_le_bytes());
}

fn invalid_data(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kernel_statfs(normalized_mount_flags: u64) -> KernelStatfsX86_64 {
        KernelStatfsX86_64 {
            filesystem_type: EXT4_SUPER_MAGIC,
            block_size: 4096,
            blocks: 1_048_576,
            blocks_free: 524_288,
            blocks_available: 500_000,
            files: 65_536,
            files_free: 32_768,
            filesystem_id: [0x1020_3040, 0x5060_7080],
            maximum_name_length: 255,
            fragment_size: 4096,
            mount_flags: (normalized_mount_flags | ST_VALID_FLAG) as i64,
            spare: [0; 4],
        }
    }

    fn filesystem() -> StableFilesystemMetadata {
        StableFilesystemMetadata {
            filesystem_type: EXT4_SUPER_MAGIC,
            block_size: 4096,
            total_blocks: 1_048_576,
            total_files: 65_536,
            maximum_name_length: 255,
            fragment_size: 4096,
            mount_flags: REQUIRED_MOUNT_FLAGS | 0x0002 | 0x0004,
        }
    }

    fn timestamp(seconds: i64, nanoseconds: u32) -> KernelStatxTimestamp {
        KernelStatxTimestamp {
            seconds,
            nanoseconds,
            reserved: 0,
        }
    }

    fn metadata(inode: u64, mode: u16, size: u64, links: u32) -> CanonicalMetadata {
        CanonicalMetadata {
            identity: FileIdentity {
                device: 0x801,
                inode,
            },
            mask: REQUIRED_STATX_MASK | STATX_DIOALIGN,
            block_size: 4096,
            attributes: STATX_ATTR_NODUMP,
            link_count: links,
            uid: 1000,
            gid: 1001,
            mode,
            inode,
            size,
            blocks: 0,
            attributes_mask: KNOWN_STATX_ATTRIBUTES,
            access_time: timestamp(11, 12),
            birth_time: timestamp(13, 14),
            change_time: timestamp(15, 16),
            modification_time: timestamp(17, 18),
            rdev_major: 0,
            rdev_minor: 0,
            dev_major: 8,
            dev_minor: 1,
            mount_id: 0xfeed_beef,
            unique_mount_mask: REQUIRED_STATX_MASK | STATX_DIOALIGN | STATX_MNT_ID_UNIQUE,
            unique_mount_id: 0x1_0000_0000 + 0xfeed_beef,
            dio_memory_alignment: 512,
            dio_offset_alignment: 4096,
            subvolume: 7,
            atomic_write_unit_min: 0,
            atomic_write_unit_max: 0,
            atomic_write_segments_max: 0,
            dio_read_offset_alignment: 512,
            atomic_write_unit_max_opt: 0,
        }
    }

    fn root_metadata() -> CanonicalMetadata {
        metadata(41, (libc::S_IFDIR | 0o555) as u16, 0, 2)
    }

    fn placeholder(inode: u64) -> CanonicalMetadata {
        metadata(inode, (libc::S_IFREG | 0o444) as u16, 0, 1)
    }

    fn inode_policy(kind: CoverEntryKind) -> CanonicalInodePolicy {
        let mut bytes = Vec::with_capacity(STABLE_INODE_POLICY_ENCODING_BYTES);
        bytes.push(1); // Ext4 profile.
        bytes.extend_from_slice(&EXT4_SUPER_MAGIC.to_le_bytes());
        bytes.push(match kind {
            CoverEntryKind::Directory => 2,
            CoverEntryKind::Placeholder => 3,
        });
        bytes.extend_from_slice(&0x0008_0050_u32.to_le_bytes()); // immutable, nodump, extent.
        bytes.extend_from_slice(&0x0000_0088_u32.to_le_bytes()); // immutable, nodump xflags.
        for value in [0_u32, 0, 0, 0, 11] {
            bytes.extend_from_slice(&value.to_le_bytes());
        }
        CanonicalInodePolicy(bytes.try_into().unwrap())
    }

    fn record(path: &[u8], inode: u64) -> TreeRecord {
        TreeRecord {
            kind: CoverEntryKind::Placeholder,
            relative_path: path.to_vec(),
            metadata: placeholder(inode),
            inode_policy: inode_policy(CoverEntryKind::Placeholder),
        }
    }

    fn directory_record(path: &[u8], inode: u64) -> TreeRecord {
        TreeRecord {
            kind: CoverEntryKind::Directory,
            relative_path: path.to_vec(),
            metadata: metadata(inode, (libc::S_IFDIR | 0o555) as u16, 0, 2),
            inode_policy: inode_policy(CoverEntryKind::Directory),
        }
    }

    fn digest_hex(digest: [u8; 32]) -> String {
        use std::fmt::Write;

        digest.iter().fold(String::new(), |mut output, byte| {
            write!(&mut output, "{byte:02x}").unwrap();
            output
        })
    }

    #[test]
    fn canonical_metadata_and_tree_digests_have_fixed_goldens() {
        let filesystem = filesystem();
        let root = root_metadata();
        let root_inode_policy = inode_policy(CoverEntryKind::Directory);
        let records = [record(b"ld.cache", 42), record(b"lib/libc.so.6", 43)];
        assert_eq!(
            digest_hex(metadata_digest(&filesystem, &root, &root_inode_policy)),
            "a9243adbe706f39c959cb818bc832e5929a6c12f6693195379d12cf3ea2710a5"
        );
        assert_eq!(
            digest_hex(tree_digest(&filesystem, &root, &root_inode_policy, &records).unwrap()),
            "fca0c80a6f82bdcf21e8cda3a4696872fd4f43c1b11398e21dca1beeccf4e69b"
        );
    }

    #[test]
    fn exact_xattr_policy_token_is_digest_bound() {
        let filesystem = filesystem();
        let root = root_metadata();
        let root_inode_policy = inode_policy(CoverEntryKind::Directory);
        let records = [record(b"ld.cache", 42), record(b"lib/libc.so.6", 43)];
        assert_eq!(
            STABLE_COVER_XATTR_POLICY.as_bytes(),
            STABLE_COVER_XATTR_POLICY_BYTES
        );
        assert_ne!(
            metadata_digest_with_policy(b"absent-v2", &filesystem, &root, &root_inode_policy,),
            metadata_digest(&filesystem, &root, &root_inode_policy)
        );
        assert_ne!(
            tree_digest_with_policy(
                b"absent-v2",
                &filesystem,
                &root,
                &root_inode_policy,
                &records,
            )
            .unwrap(),
            tree_digest(&filesystem, &root, &root_inode_policy, &records).unwrap()
        );
    }

    #[test]
    fn sorted_inventory_queries_distinguish_directories_placeholders_and_absence() {
        let records = [
            directory_record(b"etc", 42),
            record(b"etc/ld.so.cache", 43),
            directory_record(b"lib", 44),
            record(b"lib/ld-linux-x86-64.so.2", 45),
        ];

        assert_eq!(
            find_tree_record(&records, b"etc").map(|record| record.kind),
            Some(CoverEntryKind::Directory)
        );
        assert_eq!(
            find_tree_record(&records, b"etc/ld.so.cache").map(|record| record.kind),
            Some(CoverEntryKind::Placeholder)
        );
        assert!(proves_absent_with_directory_ancestors(&records, b"etc/ld.so.preload").unwrap());
        assert!(proves_absent_with_directory_ancestors(&records, b"runtime.so").unwrap());
        for not_proved_absent in [
            b"etc/ld.so.cache".as_slice(),
            b"etc/ld.so.cache/child",
            b"lib64/ld-linux-x86-64.so.2",
        ] {
            assert!(
                !proves_absent_with_directory_ancestors(&records, not_proved_absent).unwrap(),
                "proved absence through an existing path or invalid ancestor {not_proved_absent:?}"
            );
        }
        for malformed in [
            b"".as_slice(),
            b".",
            b"etc/../etc/ld.so.preload",
            b"etc//ld.so.preload",
            b"etc/ld.so.preload\0alias",
        ] {
            assert!(
                proves_absent_with_directory_ancestors(&records, malformed).is_err(),
                "treated malformed path as absence {malformed:?}"
            );
        }
        assert_eq!(
            empty_placeholder_paths(&records).collect::<Vec<_>>(),
            [
                b"etc/ld.so.cache".as_slice(),
                b"lib/ld-linux-x86-64.so.2".as_slice(),
            ]
        );
        assert!(
            has_exact_empty_placeholder_paths(
                &records,
                [
                    b"etc/ld.so.cache".as_slice(),
                    b"lib/ld-linux-x86-64.so.2".as_slice(),
                ]
            )
            .unwrap()
        );
        for inexact in [
            vec![b"etc/ld.so.cache".as_slice()],
            vec![
                b"etc/ld.so.cache".as_slice(),
                b"lib/ld-linux-x86-64.so.2".as_slice(),
                b"runtime.so".as_slice(),
            ],
        ] {
            assert!(!has_exact_empty_placeholder_paths(&records, inexact).unwrap());
        }
        for noncanonical in [
            vec![
                b"lib/ld-linux-x86-64.so.2".as_slice(),
                b"etc/ld.so.cache".as_slice(),
            ],
            vec![b"etc/ld.so.cache".as_slice(), b"etc/ld.so.cache".as_slice()],
            vec![b"etc/../etc/ld.so.cache".as_slice()],
        ] {
            assert!(has_exact_empty_placeholder_paths(&records, noncanonical).is_err());
        }
    }

    #[test]
    fn caller_visible_xattr_result_accepts_only_an_exact_zero() {
        require_caller_visible_xattr_zero_result(Ok(0)).unwrap();
        for positive in [1, 2, usize::MAX] {
            assert!(require_caller_visible_xattr_zero_result(Ok(positive)).is_err());
        }
        for error in [libc::ENOTSUP, libc::EPERM, libc::EIO, libc::ERANGE] {
            assert!(
                require_caller_visible_xattr_zero_result(Err(error)).is_err(),
                "accepted errno {error}"
            );
        }
    }

    #[test]
    fn live_caller_visible_xattr_enumeration_rejects_one_attribute_without_fallback() {
        use std::sync::atomic::AtomicU64;
        use std::sync::atomic::Ordering;

        struct TestFile(PathBuf);

        impl Drop for TestFile {
            fn drop(&mut self) {
                let _ = std::fs::remove_file(&self.0);
            }
        }

        static NEXT_FILE: AtomicU64 = AtomicU64::new(0);
        let sequence = NEXT_FILE.fetch_add(1, Ordering::Relaxed);
        let path = TestFile(std::env::temp_dir().join(format!(
            "reverie-stable-cover-xattr-{}-{sequence}",
            std::process::id()
        )));
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path.0)
            .unwrap();
        let file = File::open(&path.0).unwrap();
        require_caller_visible_xattr_zero_result(list_caller_visible_xattrs(file.as_raw_fd()))
            .unwrap();

        let name = c"user.reverie-stable-cover-test";
        let value = [0xa5u8];
        let result = unsafe {
            libc::fsetxattr(
                file.as_raw_fd(),
                name.as_ptr(),
                value.as_ptr().cast(),
                value.len(),
                0,
            )
        };
        assert_eq!(
            result,
            0,
            "fsetxattr failed: {}",
            io::Error::last_os_error()
        );
        let listed_bytes = list_caller_visible_xattrs(file.as_raw_fd()).unwrap();
        assert_eq!(listed_bytes, name.to_bytes_with_nul().len());
        assert!(require_caller_visible_xattr_zero_result(Ok(listed_bytes)).is_err());
    }

    #[test]
    fn every_encoded_metadata_and_filesystem_field_changes_the_digest() {
        let filesystem = filesystem();
        let baseline = root_metadata();
        let inode_policy = inode_policy(CoverEntryKind::Directory);
        let expected = metadata_digest(&filesystem, &baseline, &inode_policy);
        let metadata_mutators: &[fn(&mut CanonicalMetadata)] = &[
            |m| m.identity.device += 1,
            |m| m.identity.inode += 1,
            |m| m.mask ^= STATX_DIOALIGN,
            |m| m.block_size += 1,
            |m| m.attributes ^= STATX_ATTR_NODUMP,
            |m| m.link_count += 1,
            |m| m.uid += 1,
            |m| m.gid += 1,
            |m| m.mode ^= 1,
            |m| m.inode += 1,
            |m| m.size += 1,
            |m| m.blocks += 1,
            |m| m.attributes_mask ^= STATX_ATTR_DAX,
            |m| m.access_time.seconds += 1,
            |m| m.access_time.nanoseconds += 1,
            |m| m.birth_time.seconds += 1,
            |m| m.birth_time.nanoseconds += 1,
            |m| m.change_time.seconds += 1,
            |m| m.change_time.nanoseconds += 1,
            |m| m.modification_time.seconds += 1,
            |m| m.modification_time.nanoseconds += 1,
            |m| m.rdev_major += 1,
            |m| m.rdev_minor += 1,
            |m| m.dev_major += 1,
            |m| m.dev_minor += 1,
            |m| m.mount_id += 1,
            |m| m.unique_mount_mask ^= STATX_DIOALIGN,
            |m| m.unique_mount_id += 1,
            |m| m.dio_memory_alignment += 1,
            |m| m.dio_offset_alignment += 1,
            |m| m.subvolume += 1,
            |m| m.atomic_write_unit_min += 1,
            |m| m.atomic_write_unit_max += 1,
            |m| m.atomic_write_segments_max += 1,
            |m| m.dio_read_offset_alignment += 1,
            |m| m.atomic_write_unit_max_opt += 1,
        ];
        for (index, mutate) in metadata_mutators.iter().enumerate() {
            let mut changed = baseline.clone();
            mutate(&mut changed);
            assert_ne!(
                metadata_digest(&filesystem, &changed, &inode_policy),
                expected,
                "metadata field mutator {index} was not digest-bound"
            );
        }

        let filesystem_mutators: &[fn(&mut StableFilesystemMetadata)] = &[
            |m| m.filesystem_type ^= 1,
            |m| m.block_size += 1,
            |m| m.total_blocks += 1,
            |m| m.total_files += 1,
            |m| m.maximum_name_length += 1,
            |m| m.fragment_size += 1,
            |m| m.mount_flags ^= 0x0008,
        ];
        for (index, mutate) in filesystem_mutators.iter().enumerate() {
            let mut changed = filesystem;
            mutate(&mut changed);
            assert_ne!(
                metadata_digest(&changed, &baseline, &inode_policy),
                expected,
                "filesystem field mutator {index} was not digest-bound"
            );
        }

        for index in 0..STABLE_INODE_POLICY_ENCODING_BYTES {
            let mut changed = inode_policy.clone();
            changed.0[index] ^= 1;
            assert_ne!(
                metadata_digest(&filesystem, &baseline, &changed),
                expected,
                "inode-policy byte {index} was not digest-bound"
            );
        }
    }

    #[test]
    fn source_mount_ids_and_mount_root_attribute_are_not_normalized() {
        let filesystem = filesystem();
        let baseline = root_metadata();
        let inode_policy = inode_policy(CoverEntryKind::Directory);
        let expected = metadata_digest(&filesystem, &baseline, &inode_policy);

        let mut changed = baseline.clone();
        changed.mount_id += 1;
        assert_ne!(
            metadata_digest(&filesystem, &changed, &inode_policy),
            expected
        );

        let mut changed = baseline.clone();
        changed.unique_mount_id += 1;
        assert_ne!(
            metadata_digest(&filesystem, &changed, &inode_policy),
            expected
        );

        let mut changed = baseline;
        changed.attributes ^= STATX_ATTR_MOUNT_ROOT;
        assert_ne!(
            metadata_digest(&filesystem, &changed, &inode_policy),
            expected
        );
    }

    #[test]
    fn every_root_and_record_inode_policy_byte_changes_the_tree_digest() {
        let filesystem = filesystem();
        let root = root_metadata();
        let root_inode_policy = inode_policy(CoverEntryKind::Directory);
        let records = [record(b"placeholder", 42)];
        let expected = tree_digest(&filesystem, &root, &root_inode_policy, &records).unwrap();

        for index in 0..STABLE_INODE_POLICY_ENCODING_BYTES {
            let mut changed_root_policy = root_inode_policy.clone();
            changed_root_policy.0[index] ^= 1;
            assert_ne!(
                tree_digest(&filesystem, &root, &changed_root_policy, &records).unwrap(),
                expected,
                "root inode-policy byte {index} was not tree-digest-bound"
            );

            let mut changed_records = records.clone();
            changed_records[0].inode_policy.0[index] ^= 1;
            assert_ne!(
                tree_digest(&filesystem, &root, &root_inode_policy, &changed_records).unwrap(),
                expected,
                "record inode-policy byte {index} was not tree-digest-bound"
            );
        }
    }

    #[test]
    fn tree_digest_rejects_order_duplicates_noncanonical_paths_and_bounds() {
        let filesystem = filesystem();
        let root = root_metadata();
        let root_inode_policy = inode_policy(CoverEntryKind::Directory);
        assert!(
            tree_digest(
                &filesystem,
                &root,
                &root_inode_policy,
                &[record(b"b", 42), record(b"a", 43)],
            )
            .is_err()
        );
        assert!(
            tree_digest(
                &filesystem,
                &root,
                &root_inode_policy,
                &[record(b"a", 42), record(b"a", 43)],
            )
            .is_err()
        );
        for invalid in [b"./a".as_slice(), b"a/../b", b"/a", b"a//b", b"a/"] {
            assert!(
                tree_digest(
                    &filesystem,
                    &root,
                    &root_inode_policy,
                    &[record(invalid, 42)],
                )
                .is_err()
            );
        }

        let too_many = (0..=MAX_TREE_ENTRIES)
            .map(|index| record(format!("{index:03}").as_bytes(), index as u64 + 100))
            .collect::<Vec<_>>();
        assert!(tree_digest(&filesystem, &root, &root_inode_policy, &too_many).is_err());

        let oversized = vec![b'x'; MAX_RELATIVE_PATH_BYTES + 1];
        assert!(
            tree_digest(
                &filesystem,
                &root,
                &root_inode_policy,
                &[record(&oversized, 42)],
            )
            .is_err()
        );
    }

    #[test]
    fn filesystem_policy_requires_supported_readonly_noatime_geometry() {
        filesystem().validate().unwrap();

        let mut unsupported = filesystem();
        unsupported.filesystem_type = 0;
        assert!(unsupported.validate().is_err());

        for missing in [ST_RDONLY_FLAG, ST_NOATIME_FLAG] {
            let mut writable_or_atime = filesystem();
            writable_or_atime.mount_flags &= !missing;
            assert!(writable_or_atime.validate().is_err());
        }

        let geometry_mutators: [fn(&mut StableFilesystemMetadata); 3] = [
            |m: &mut StableFilesystemMetadata| m.block_size = 0,
            |m: &mut StableFilesystemMetadata| m.maximum_name_length = 0,
            |m: &mut StableFilesystemMetadata| m.fragment_size = 0,
        ];
        for mutate in geometry_mutators {
            let mut invalid = filesystem();
            mutate(&mut invalid);
            assert!(invalid.validate().is_err());
        }
    }

    #[test]
    fn statfs_tail_requires_valid_flags_zero_reservations_and_statvfs_agreement() {
        let normalized = REQUIRED_MOUNT_FLAGS | 0x0002 | 0x0004;
        let baseline = kernel_statfs(normalized);
        assert_eq!(
            normalize_kernel_statfs_mount_flags(&baseline, normalized).unwrap(),
            normalized
        );

        let mut missing_valid = baseline;
        missing_valid.mount_flags &= !(ST_VALID_FLAG as i64);
        assert_eq!(
            normalize_kernel_statfs_mount_flags(&missing_valid, normalized)
                .unwrap_err()
                .to_string(),
            "cover statfs mount flags lack ST_VALID"
        );

        for index in 0..4 {
            let mut corrupt = baseline;
            corrupt.spare[index] = 1;
            assert_eq!(
                normalize_kernel_statfs_mount_flags(&corrupt, normalized)
                    .unwrap_err()
                    .to_string(),
                "cover statfs returned nonzero reserved words",
                "accepted corrupt raw statfs spare word {index}"
            );
        }

        let mut unwritten_poison = baseline;
        unwritten_poison.spare = [0xa5a5_a5a5_a5a5_a5a5_u64 as i64; 4];
        assert_eq!(
            normalize_kernel_statfs_mount_flags(&unwritten_poison, normalized)
                .unwrap_err()
                .to_string(),
            "cover statfs returned nonzero reserved words"
        );

        let mut negative_flags = baseline;
        negative_flags.mount_flags = -1;
        assert_eq!(
            normalize_kernel_statfs_mount_flags(&negative_flags, normalized)
                .unwrap_err()
                .to_string(),
            "cover statfs mount flags are invalid"
        );

        assert_eq!(
            normalize_kernel_statfs_mount_flags(&baseline, normalized ^ ST_RDONLY_FLAG)
                .unwrap_err()
                .to_string(),
            "cover statfs and statvfs mount flags differ"
        );
    }

    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    #[test]
    fn raw_fstatfs_reads_and_validates_the_kernel_tail() {
        let descriptor = File::open(".").unwrap();
        let raw = raw_fstatfs_x86_64(descriptor.as_raw_fd()).unwrap();
        assert_eq!(raw.spare, [0; 4]);

        let mut mount = std::mem::MaybeUninit::<libc::statvfs>::zeroed();
        // SAFETY: `descriptor` is live and `mount` is a writable output buffer.
        assert_eq!(
            unsafe { libc::fstatvfs(descriptor.as_raw_fd(), mount.as_mut_ptr()) },
            0
        );
        // SAFETY: the successful `fstatvfs` initialized the output.
        let mount = unsafe { mount.assume_init() };
        assert_eq!(
            normalize_kernel_statfs_mount_flags(&raw, mount.f_flag).unwrap(),
            mount.f_flag
        );
    }

    #[test]
    fn duplicate_inode_identity_and_getdents_replacement_are_refused() {
        let identity = FileIdentity {
            device: 0x801,
            inode: 42,
        };
        let mut identities = BTreeSet::new();
        insert_unique_identity(&mut identities, identity).unwrap();
        assert!(insert_unique_identity(&mut identities, identity).is_err());

        require_enumerated_inode(42, 42).unwrap();
        assert!(require_enumerated_inode(42, 43).is_err());
    }

    #[test]
    fn live_getdents_scan_preserves_each_name_to_inode_link() {
        use std::os::unix::ffi::OsStrExt;
        use std::os::unix::fs::MetadataExt;
        use std::sync::atomic::AtomicU64;
        use std::sync::atomic::Ordering;

        struct TestDirectory(PathBuf);

        impl Drop for TestDirectory {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }

        static NEXT_DIRECTORY: AtomicU64 = AtomicU64::new(0);
        let sequence = NEXT_DIRECTORY.fetch_add(1, Ordering::Relaxed);
        let directory = TestDirectory(std::env::temp_dir().join(format!(
            "reverie-stable-cover-{}-{sequence}",
            std::process::id()
        )));
        std::fs::create_dir(&directory.0).unwrap();
        std::fs::File::create(directory.0.join("artifact")).unwrap();
        std::fs::create_dir(directory.0.join("lib")).unwrap();

        let open = File::open(&directory.0).unwrap();
        let entries = read_directory_entries(open.as_raw_fd()).unwrap();
        assert_eq!(
            entries
                .iter()
                .map(|entry| entry.name.as_slice())
                .collect::<Vec<_>>(),
            [b"artifact".as_slice(), b"lib".as_slice()]
        );
        for entry in entries {
            let opened_inode = std::fs::symlink_metadata(
                directory.0.join(std::ffi::OsStr::from_bytes(&entry.name)),
            )
            .unwrap()
            .ino();
            require_enumerated_inode(entry.inode, opened_inode).unwrap();
        }
    }

    #[test]
    fn placeholder_shape_refuses_content_and_hard_links() {
        let mut nonempty = placeholder(42);
        nonempty.size = 1;
        assert!(
            nonempty
                .validate_entry(CoverEntryKind::Placeholder)
                .is_err()
        );
        let mut linked = placeholder(42);
        linked.link_count = 2;
        assert!(linked.validate_entry(CoverEntryKind::Placeholder).is_err());
        assert!(
            placeholder(42)
                .validate_entry(CoverEntryKind::Directory)
                .is_err()
        );
    }

    #[test]
    fn absolute_and_relative_path_grammars_are_exact() {
        assert!(exact_absolute_path(Path::new("/stable/cover")).is_ok());
        for path in [
            "stable/cover",
            "/stable//cover",
            "/stable/./cover",
            "/stable/../cover",
            "/stable/cover/",
        ] {
            assert!(
                exact_absolute_path(Path::new(path)).is_err(),
                "accepted {path:?}"
            );
        }
        assert!(validate_relative_path(b"lib/x86_64/libc.so.6").is_ok());
        for path in [
            b"".as_slice(),
            b".",
            b"..",
            b"a/./b",
            b"a/../b",
            b"a//b",
            b"/a",
            b"a/",
        ] {
            assert!(validate_relative_path(path).is_err());
        }
    }
}
