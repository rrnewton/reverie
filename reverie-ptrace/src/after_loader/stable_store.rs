//! Fail-closed verification of already-published fs-verity artifacts.
//!
//! This module only admits an existing regular file. Publication, enabling
//! fs-verity, and arranging the guest-visible bind mount belong to the staging
//! or broker side of the after-loader design. In particular, there is no
//! mutable-file, lease, or memfd fallback here.
//!
//! This verifier binds the source filesystem type and policy flags, but it
//! deliberately does not claim that mutable `statfs` capacity counters are
//! deterministic. The common mount-epoch syscall gate must reject pre-entry
//! `statfs`/`fstatfs`/`statvfs` observation. After guest entry, mapped files and
//! `mm->exe_file` may still pin the temporary mount; the common Detcore
//! `statfs`/`fstatfs` canonicalization must therefore handle those references
//! identically for the ptrace reference and LiteInst, with exact controls.

use std::ffi::CStr;
use std::ffi::CString;
use std::fs::File;
use std::io;
use std::mem::MaybeUninit;
use std::os::fd::AsRawFd;
use std::os::fd::FromRawFd;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::FileExt;
use std::path::Component;
use std::path::Path;
use std::path::PathBuf;
use std::ptr;
use std::sync::Arc;

use sha2::Digest;
use sha2::Sha256;

use super::FileIdentity;
use super::inode_policy::StableInodeKind;
use super::inode_policy::StableInodePolicy;

const FS_VERITY_HASH_ALG_SHA256: u16 = 1;
const SHA256_DIGEST_BYTES: usize = 32;
const FS_IOC_MEASURE_VERITY_NUMBER: u8 = 134;
const READ_CHUNK_BYTES: usize = 16 * 1024;
const STATX_MNT_ID_UNIQUE: u32 = 0x0000_4000;
const STATX_SUBVOL: u32 = 0x0000_8000;
const STATX_WRITE_ATOMIC: u32 = 0x0001_0000;
const STATX_DIO_READ_ALIGN: u32 = 0x0002_0000;
const STABLE_STATX_MASK: u32 = libc::STATX_BASIC_STATS
    | libc::STATX_BTIME
    | libc::STATX_MNT_ID
    | libc::STATX_DIOALIGN
    | STATX_SUBVOL
    | STATX_WRITE_ATOMIC
    | STATX_DIO_READ_ALIGN;
const REQUIRED_STATX_MASK: u32 = libc::STATX_BASIC_STATS | libc::STATX_BTIME | libc::STATX_MNT_ID;
const STABLE_METADATA_DOMAIN: &[u8] = b"reverie-stable-verity-metadata-v3\0";
const CALLER_VISIBLE_XATTR_EVIDENCE: &[u8] = b"caller-visible-zero-v1";

/// The end-to-end extended-attribute policy required by the schema.
///
/// Schema parsing authenticates this exact token. The local `flistxattr`
/// observations below are necessary but cannot satisfy `absent-v1`: a zero
/// result proves only that no names are visible to this caller at that instant.
/// Runnable integration must additionally require a future authenticated
/// source-epoch receipt that covers every xattr namespace and the complete
/// verification-to-use interval. This source-only module does not fabricate
/// that broker evidence.
pub(crate) const STABLE_XATTR_POLICY: &str = "absent-v1";

/// Fixed portion of Linux's flexible-array `struct fsverity_digest`.
///
/// The ioctl request size is the size of this header, not the size of the
/// caller's allocation that follows it.
#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct FsVerityDigestHeader {
    digest_algorithm: u16,
    digest_size: u16,
}

/// One header followed immediately by capacity for a SHA-256 digest.
#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct FsVerityDigestPacket {
    header: FsVerityDigestHeader,
    digest: [u8; SHA256_DIGEST_BYTES],
}

impl FsVerityDigestPacket {
    fn for_sha256_measurement() -> Self {
        Self {
            header: FsVerityDigestHeader {
                digest_algorithm: 0,
                digest_size: SHA256_DIGEST_BYTES as u16,
            },
            digest: [0; SHA256_DIGEST_BYTES],
        }
    }
}

const _: () = assert!(std::mem::size_of::<FsVerityDigestHeader>() == 4);
const _: () = assert!(std::mem::offset_of!(FsVerityDigestPacket, header) == 0);
const _: () = assert!(
    std::mem::offset_of!(FsVerityDigestPacket, digest)
        == std::mem::size_of::<FsVerityDigestHeader>()
);
const _: () = assert!(
    std::mem::size_of::<FsVerityDigestPacket>()
        == std::mem::size_of::<FsVerityDigestHeader>() + SHA256_DIGEST_BYTES
);

nix::ioctl_readwrite!(
    /// Invoke Linux `FS_IOC_MEASURE_VERITY` with its flexible-array header.
    ///
    /// # Safety
    ///
    /// `data` must point to a writable `FsVerityDigestHeader` followed
    /// immediately by at least `digest_size` writable bytes. The complete
    /// allocation must remain live for the ioctl call.
    fs_ioc_measure_verity,
    b'f',
    FS_IOC_MEASURE_VERITY_NUMBER,
    FsVerityDigestHeader
);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct StableTimestamp {
    seconds: i64,
    nanoseconds: u32,
}

impl StableTimestamp {
    fn from_statx(timestamp: libc::statx_timestamp) -> io::Result<Self> {
        if timestamp.tv_nsec >= 1_000_000_000 {
            return Err(invalid_data("fs-verity artifact timestamp is invalid"));
        }
        Ok(Self {
            seconds: timestamp.tv_sec,
            nanoseconds: timestamp.tv_nsec,
        })
    }

    fn update_digest(self, digest: &mut Sha256) {
        digest.update(self.seconds.to_le_bytes());
        digest.update(self.nanoseconds.to_le_bytes());
    }
}

/// Canonical `statx` and mount snapshot for one store-epoch observation.
///
/// [`StableArtifactMetadata`] combines this with the ordinary descriptor's
/// inode-ioctl policy before hashing. The explicit device/inode pair makes
/// republishing identical bytes at a fresh inode, or changing ownership, mode,
/// timestamps, mount identity, or statx attributes, a hard failure rather than
/// a new accepted epoch.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct StableFileMetadata {
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
    access_time: StableTimestamp,
    birth_time: StableTimestamp,
    change_time: StableTimestamp,
    modification_time: StableTimestamp,
    rdev_major: u32,
    rdev_minor: u32,
    device_major: u32,
    device_minor: u32,
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
    atomic_write_unit_max_optimized: u32,
    filesystem_type: i64,
    mount_flags: u64,
}

impl StableFileMetadata {
    fn read(file: &File) -> io::Result<Self> {
        let raw = read_statx(file, STABLE_STATX_MASK)?;
        if raw.stx_mask & !STABLE_STATX_MASK != 0 {
            return Err(invalid_data(
                "fs-verity artifact returned unknown statx metadata",
            ));
        }
        if raw.stx_mask & REQUIRED_STATX_MASK != REQUIRED_STATX_MASK {
            return Err(invalid_data(
                "fs-verity artifact lacks required statx metadata",
            ));
        }
        if u32::from(raw.stx_mode) & libc::S_IFMT != libc::S_IFREG {
            return Err(invalid_data("fs-verity artifact is not a regular file"));
        }
        if raw.stx_nlink != 1 {
            return Err(invalid_data(
                "fs-verity artifact must have exactly one hard link",
            ));
        }
        // The O_PATH observation can require the verity value but cannot issue
        // the inode ioctls. Its filesystem-specific attributes-mask rule is
        // applied after fstatfs below; the ordinary-descriptor snapshot then
        // binds this value to both generic inode-attribute ioctl interfaces.
        if raw.stx_attributes & libc::STATX_ATTR_VERITY as u64 == 0 {
            return Err(invalid_data(
                "fs-verity artifact metadata does not report verity protection",
            ));
        }

        // Linux selects either the reusable mount id or the non-recycled
        // 64-bit unique id for one statx request. Authenticate both with two
        // explicit calls rather than conflating their shared output field.
        let unique_mount = read_statx(file, STABLE_STATX_MASK | STATX_MNT_ID_UNIQUE)?;
        if unique_mount.stx_mask & !(STABLE_STATX_MASK | STATX_MNT_ID_UNIQUE) != 0 {
            return Err(invalid_data(
                "fs-verity artifact returned unknown unique-mount statx metadata",
            ));
        }
        if unique_mount.stx_mask & STATX_MNT_ID_UNIQUE == 0 {
            return Err(invalid_data(
                "fs-verity artifact lacks a unique statx mount id",
            ));
        }
        if unique_mount.stx_ino != raw.stx_ino
            || unique_mount.stx_dev_major != raw.stx_dev_major
            || unique_mount.stx_dev_minor != raw.stx_dev_minor
        {
            return Err(invalid_data(
                "fs-verity artifact identity changed across statx mount-id queries",
            ));
        }

        let (filesystem_type, mount_flags) = stable_mount_policy(file)?;
        let verity_mask_advertised = raw.stx_attributes_mask & libc::STATX_ATTR_VERITY as u64 != 0;
        let verity_mask_expected = filesystem_type != libc::BTRFS_SUPER_MAGIC;
        if verity_mask_advertised != verity_mask_expected {
            return Err(invalid_data(
                "fs-verity artifact has the wrong statx verity capability for its filesystem",
            ));
        }
        Ok(Self {
            mask: raw.stx_mask,
            block_size: raw.stx_blksize,
            attributes: raw.stx_attributes,
            link_count: raw.stx_nlink,
            uid: raw.stx_uid,
            gid: raw.stx_gid,
            mode: raw.stx_mode,
            inode: raw.stx_ino,
            size: raw.stx_size,
            blocks: raw.stx_blocks,
            attributes_mask: raw.stx_attributes_mask,
            access_time: StableTimestamp::from_statx(raw.stx_atime)?,
            birth_time: StableTimestamp::from_statx(raw.stx_btime)?,
            change_time: StableTimestamp::from_statx(raw.stx_ctime)?,
            modification_time: StableTimestamp::from_statx(raw.stx_mtime)?,
            rdev_major: raw.stx_rdev_major,
            rdev_minor: raw.stx_rdev_minor,
            device_major: raw.stx_dev_major,
            device_minor: raw.stx_dev_minor,
            mount_id: raw.stx_mnt_id,
            unique_mount_mask: unique_mount.stx_mask,
            unique_mount_id: unique_mount.stx_mnt_id,
            dio_memory_alignment: raw.stx_dio_mem_align,
            dio_offset_alignment: raw.stx_dio_offset_align,
            subvolume: raw.stx_subvol,
            atomic_write_unit_min: raw.stx_atomic_write_unit_min,
            atomic_write_unit_max: raw.stx_atomic_write_unit_max,
            atomic_write_segments_max: raw.stx_atomic_write_segments_max,
            dio_read_offset_alignment: raw.stx_dio_read_offset_align,
            atomic_write_unit_max_optimized: raw.stx_atomic_write_unit_max_opt,
            filesystem_type,
            mount_flags,
        })
    }

    fn identity(self) -> FileIdentity {
        FileIdentity {
            device: libc::makedev(self.device_major, self.device_minor),
            inode: self.inode,
        }
    }

    fn update_digest(self, digest: &mut Sha256) {
        digest.update(self.mask.to_le_bytes());
        digest.update(self.block_size.to_le_bytes());
        digest.update(self.attributes.to_le_bytes());
        digest.update(self.link_count.to_le_bytes());
        digest.update(self.uid.to_le_bytes());
        digest.update(self.gid.to_le_bytes());
        digest.update(self.mode.to_le_bytes());
        digest.update(self.inode.to_le_bytes());
        digest.update(self.size.to_le_bytes());
        digest.update(self.blocks.to_le_bytes());
        digest.update(self.attributes_mask.to_le_bytes());
        self.access_time.update_digest(digest);
        self.birth_time.update_digest(digest);
        self.change_time.update_digest(digest);
        self.modification_time.update_digest(digest);
        digest.update(self.rdev_major.to_le_bytes());
        digest.update(self.rdev_minor.to_le_bytes());
        digest.update(self.device_major.to_le_bytes());
        digest.update(self.device_minor.to_le_bytes());
        digest.update(self.mount_id.to_le_bytes());
        digest.update(self.unique_mount_mask.to_le_bytes());
        digest.update(self.unique_mount_id.to_le_bytes());
        digest.update(self.dio_memory_alignment.to_le_bytes());
        digest.update(self.dio_offset_alignment.to_le_bytes());
        digest.update(self.subvolume.to_le_bytes());
        digest.update(self.atomic_write_unit_min.to_le_bytes());
        digest.update(self.atomic_write_unit_max.to_le_bytes());
        digest.update(self.atomic_write_segments_max.to_le_bytes());
        digest.update(self.dio_read_offset_alignment.to_le_bytes());
        digest.update(self.atomic_write_unit_max_optimized.to_le_bytes());
        digest.update(self.filesystem_type.to_le_bytes());
        digest.update(self.mount_flags.to_le_bytes());
    }
}

/// Complete descriptor-bound metadata retained for one stable source epoch.
///
/// The `file` snapshot can also be read from an `O_PATH` pin, while
/// `inode_policy` is read only from the ordinary descriptor obtained after that
/// pin has proved the inode type. Every ordinary-descriptor revalidation
/// reconstructs this whole value so ioctl policy changes cannot hide behind
/// unchanged `statx` fields.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct StableArtifactMetadata {
    file: StableFileMetadata,
    inode_policy: StableInodePolicy,
}

impl StableArtifactMetadata {
    fn read_from_ordinary_descriptor(file: &File) -> io::Result<Self> {
        let metadata = StableFileMetadata::read(file)?;
        let inode_policy = StableInodePolicy::read(
            file.as_raw_fd(),
            metadata.filesystem_type,
            StableInodeKind::VerityArtifact,
        )?;
        inode_policy.validate_statx(metadata.attributes, metadata.attributes_mask)?;
        Ok(Self {
            file: metadata,
            inode_policy,
        })
    }

    fn identity(self) -> FileIdentity {
        self.file.identity()
    }

    fn digest(self) -> [u8; SHA256_DIGEST_BYTES] {
        self.digest_with_contract(
            STABLE_XATTR_POLICY.as_bytes(),
            CALLER_VISIBLE_XATTR_EVIDENCE,
        )
    }

    fn digest_with_contract(
        self,
        required_xattr_policy: &[u8],
        caller_visible_xattr_evidence: &[u8],
    ) -> [u8; SHA256_DIGEST_BYTES] {
        let mut inode_policy = Vec::new();
        self.inode_policy.encode(&mut inode_policy);
        metadata_digest_from_encoding(
            self.file,
            &inode_policy,
            required_xattr_policy,
            caller_visible_xattr_evidence,
        )
    }
}

fn metadata_digest_from_encoding(
    metadata: StableFileMetadata,
    inode_policy: &[u8],
    required_xattr_policy: &[u8],
    caller_visible_xattr_evidence: &[u8],
) -> [u8; SHA256_DIGEST_BYTES] {
    let mut digest = Sha256::new();
    digest.update(STABLE_METADATA_DOMAIN);
    digest.update((required_xattr_policy.len() as u64).to_le_bytes());
    digest.update(required_xattr_policy);
    digest.update((caller_visible_xattr_evidence.len() as u64).to_le_bytes());
    digest.update(caller_visible_xattr_evidence);
    metadata.update_digest(&mut digest);
    digest.update((inode_policy.len() as u64).to_le_bytes());
    digest.update(inode_policy);
    digest.finalize().into()
}

fn read_statx(file: &File, mask: u32) -> io::Result<libc::statx> {
    let mut raw = MaybeUninit::<libc::statx>::zeroed();
    // SAFETY: `file` owns a live descriptor, the empty C string is valid for
    // AT_EMPTY_PATH, and `raw` points to writable statx storage.
    if unsafe {
        libc::statx(
            file.as_raw_fd(),
            c"".as_ptr(),
            libc::AT_EMPTY_PATH | libc::AT_NO_AUTOMOUNT | libc::AT_STATX_FORCE_SYNC,
            mask,
            raw.as_mut_ptr(),
        )
    } != 0
    {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: a successful statx initialized the output structure; zeroing
    // first also gives deterministic bytes to fields the kernel did not fill.
    Ok(unsafe { raw.assume_init() })
}

/// Manifest-bound identity for an already-published store artifact.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct StableVerityExpectation {
    content_sha256: [u8; SHA256_DIGEST_BYTES],
    verity_sha256: [u8; SHA256_DIGEST_BYTES],
    identity: FileIdentity,
    metadata_sha256: [u8; SHA256_DIGEST_BYTES],
}

impl StableVerityExpectation {
    pub(crate) const fn new(
        content_sha256: [u8; SHA256_DIGEST_BYTES],
        verity_sha256: [u8; SHA256_DIGEST_BYTES],
        identity: FileIdentity,
        metadata_sha256: [u8; SHA256_DIGEST_BYTES],
    ) -> Self {
        Self {
            content_sha256,
            verity_sha256,
            identity,
            metadata_sha256,
        }
    }

    pub(crate) const fn content_sha256(self) -> [u8; SHA256_DIGEST_BYTES] {
        self.content_sha256
    }

    pub(crate) const fn verity_sha256(self) -> [u8; SHA256_DIGEST_BYTES] {
        self.verity_sha256
    }

    pub(crate) const fn identity(self) -> FileIdentity {
        self.identity
    }

    pub(crate) const fn metadata_sha256(self) -> [u8; SHA256_DIGEST_BYTES] {
        self.metadata_sha256
    }
}

/// Evidence that the retained source had an executable permission bit and
/// admitted a nonempty private RX mapping.
///
/// The mapping is removed before construction completes. This proves that the
/// source mount did not reject this mapping as `noexec` at verification time;
/// it does not execute any byte or promise that mount policy cannot later
/// change.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ExecutableSourceProof {
    mapped_length: usize,
    executable_permission_bits: u32,
}

impl ExecutableSourceProof {
    /// Number of source bytes covered by the temporary RX mapping request.
    pub(crate) const fn mapped_length(self) -> usize {
        self.mapped_length
    }

    /// Executable Unix permission bits sampled from the retained source.
    pub(crate) const fn executable_permission_bits(self) -> u32 {
        self.executable_permission_bits
    }
}

/// An open, byte-exact regular file with a measured SHA-256 fs-verity digest.
///
/// The descriptor, byte snapshot, and complete store-epoch metadata are
/// retained together. The canonical pathname is descriptive after this
/// constructor returns. This type intentionally does not expose a procfd path
/// or raw descriptor: integration must transfer an owned descriptor into the
/// lower-level prepared mount-epoch plan that performs the bind and validates
/// the mounted target while retaining ownership.
///
/// Successful construction includes repeated caller-visible-zero xattr-name
/// observations, not an `absent-v1` source-epoch receipt. Integration must not
/// make this artifact runnable until the future authenticated broker receipt
/// supplies that separate proof.
#[derive(Clone, Debug)]
pub(crate) struct StableVerityArtifact {
    canonical_path: PathBuf,
    file: Arc<File>,
    bytes: Arc<[u8]>,
    metadata: StableArtifactMetadata,
    identity: FileIdentity,
    content_sha256: [u8; SHA256_DIGEST_BYTES],
    verity_sha256: [u8; SHA256_DIGEST_BYTES],
    length: u64,
    executable_source_proof: Option<ExecutableSourceProof>,
}

impl StableVerityArtifact {
    /// Open and verify one already-published artifact without following any
    /// symlink in its path.
    ///
    /// `path` must be an exact lexically canonical absolute Unix pathname.
    /// `expected` authenticates its bytes, kernel fs-verity measurement,
    /// device/inode identity, and the complete statx, mount, inode-ioctl, and
    /// local-evidence metadata digest. `maximum_bytes` is a hard read bound. An
    /// `openat2(O_PATH, RESOLVE_NO_SYMLINKS)` pin proves
    /// regular-file type before `/proc/self/fd/N` reopens that exact inode for
    /// reading, so a raced FIFO or device is never opened for I/O. The source
    /// filesystem must be a supported local fs-verity filesystem mounted
    /// read-only and noatime for the externally protected store epoch.
    ///
    /// When `require_exec` is true, at least one Unix executable permission bit
    /// is required, then a nonempty private `PROT_READ | PROT_EXEC` mapping is
    /// created from the retained descriptor and unmapped without executing it.
    /// When false, no execute-permission or execute-mount claim is made.
    pub(crate) fn open_verified(
        path: &Path,
        expected: StableVerityExpectation,
        maximum_bytes: usize,
        require_exec: bool,
    ) -> io::Result<Self> {
        let canonical_path = exact_absolute_path(path)?;
        let path_pin = open_path_pin(&canonical_path)?;
        let path_before = StableFileMetadata::read(&path_pin)?;
        require_expected_identity(path_before.identity(), expected)?;

        let file = reopen_pinned_regular(&path_pin, &canonical_path)?;
        verify_descriptor_flags(&file)?;
        let descriptor_before = StableArtifactMetadata::read_from_ordinary_descriptor(&file)?;
        require_same_file_metadata(
            path_before,
            descriptor_before.file,
            "opened descriptor and canonical path differ before verification",
        )?;
        require_expected_metadata(
            descriptor_before.identity(),
            descriptor_before.digest(),
            expected,
        )?;
        require_caller_visible_zero_xattr_names(&file)?;

        let maximum_u64 = u64::try_from(maximum_bytes)
            .map_err(|_| invalid_data("artifact byte bound is not representable"))?;
        if descriptor_before.file.size > maximum_u64 {
            return Err(invalid_data("fs-verity artifact exceeds its byte bound"));
        }

        let verity_sha256 = measure_sha256_verity(&file)?;
        require_exact_sha256(
            verity_sha256,
            expected.verity_sha256,
            "fs-verity artifact verity SHA-256 differs",
        )?;
        let bytes = read_exact_bounded(&file, descriptor_before.file.size, maximum_bytes)?;
        let content_sha256: [u8; SHA256_DIGEST_BYTES] = Sha256::digest(&bytes).into();
        require_exact_sha256(
            content_sha256,
            expected.content_sha256,
            "fs-verity artifact content SHA-256 differs",
        )?;

        let executable_source_proof = if require_exec {
            Some(prove_executable_source(
                &file,
                bytes.len(),
                u32::from(descriptor_before.file.mode),
            )?)
        } else {
            None
        };

        let descriptor_after = StableArtifactMetadata::read_from_ordinary_descriptor(&file)?;
        require_same_artifact_metadata(
            descriptor_before,
            descriptor_after,
            "artifact descriptor metadata changed during verification",
        )?;
        require_caller_visible_zero_xattr_names(&file)?;
        revalidate_path_source(
            &canonical_path,
            descriptor_before,
            "artifact path identity or metadata changed during verification",
        )?;

        Ok(Self {
            canonical_path,
            file: Arc::new(file),
            bytes: bytes.into(),
            metadata: descriptor_before,
            identity: descriptor_before.identity(),
            content_sha256,
            verity_sha256,
            length: descriptor_before.file.size,
            executable_source_proof,
        })
    }

    /// Exact canonical pathname sampled during verification.
    pub(crate) fn canonical_path(&self) -> &Path {
        &self.canonical_path
    }

    /// Retained byte-for-byte snapshot authenticated by [`Self::content_sha256`].
    pub(crate) fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Device and inode reported for the retained regular-file descriptor.
    pub(crate) const fn identity(&self) -> FileIdentity {
        self.identity
    }

    /// SHA-256 of the complete retained byte snapshot.
    pub(crate) const fn content_sha256(&self) -> [u8; SHA256_DIGEST_BYTES] {
        self.content_sha256
    }

    /// SHA-256 fs-verity file digest returned by `FS_IOC_MEASURE_VERITY`.
    ///
    /// This is the kernel's digest of the fs-verity descriptor, not a
    /// userspace reconstruction of the descriptor's raw Merkle-tree field.
    pub(crate) const fn verity_sha256(&self) -> [u8; SHA256_DIGEST_BYTES] {
        self.verity_sha256
    }

    /// Exact verified file length in bytes.
    pub(crate) const fn len(&self) -> u64 {
        self.length
    }

    /// Whether the verified file is empty.
    pub(crate) const fn is_empty(&self) -> bool {
        self.length == 0
    }

    /// RX-mapping evidence, or `None` when executable use was not requested.
    ///
    /// `None` does not assert that the source mount is `noexec`.
    pub(crate) const fn executable_source_proof(&self) -> Option<ExecutableSourceProof> {
        self.executable_source_proof
    }

    /// Digest of the manifest-bound descriptor metadata snapshot.
    ///
    /// The digest commits to the required `absent-v1` token and the distinct
    /// caller-visible-zero observation tag. It is not the future authenticated
    /// source-epoch receipt needed to satisfy `absent-v1`.
    pub(crate) fn metadata_sha256(&self) -> [u8; SHA256_DIGEST_BYTES] {
        self.metadata.digest()
    }

    /// Revalidate both the retained inode and its published pathname.
    pub(crate) fn revalidate_source(&self) -> io::Result<()> {
        self.revalidate_retained()?;
        revalidate_path_source(
            &self.canonical_path,
            self.metadata,
            "fs-verity artifact pathname changed after binding",
        )
    }

    /// Recheck all descriptor-bound evidence without consulting the pathname.
    pub(crate) fn revalidate_retained(&self) -> io::Result<()> {
        verify_descriptor_flags(&self.file)?;
        let metadata = StableArtifactMetadata::read_from_ordinary_descriptor(&self.file)?;
        require_same_artifact_metadata(
            self.metadata,
            metadata,
            "retained fs-verity artifact metadata changed",
        )?;
        require_caller_visible_zero_xattr_names(&self.file)?;
        if measure_sha256_verity(&self.file)? != self.verity_sha256 {
            return Err(invalid_data(
                "retained fs-verity artifact verity SHA-256 changed",
            ));
        }
        let bytes = read_exact_bounded(&self.file, self.length, self.bytes.len())?;
        if bytes.as_slice() != self.bytes.as_ref()
            || <[u8; SHA256_DIGEST_BYTES]>::from(Sha256::digest(&bytes)) != self.content_sha256
        {
            return Err(invalid_data("retained fs-verity artifact content changed"));
        }
        require_caller_visible_zero_xattr_names(&self.file)?;
        let metadata_after = StableArtifactMetadata::read_from_ordinary_descriptor(&self.file)?;
        require_same_artifact_metadata(
            self.metadata,
            metadata_after,
            "retained fs-verity artifact metadata changed during revalidation",
        )?;
        Ok(())
    }

    /// Duplicate the retained source for the lower prepared mount epoch.
    ///
    /// No raw descriptor number or procfd spelling escapes this API. The caller
    /// receives an independently owned close-on-exec descriptor only after a
    /// complete retained-and-path source revalidation, and must retain it
    /// through the mount and positive target-receipt checks.
    pub(crate) fn duplicate_revalidated_source(&self) -> io::Result<File> {
        self.revalidate_source()?;
        let duplicate = duplicate_readonly_cloexec(&self.file)?;
        let duplicate_metadata = StableArtifactMetadata::read_from_ordinary_descriptor(&duplicate)?;
        require_same_artifact_metadata(
            self.metadata,
            duplicate_metadata,
            "duplicated fs-verity artifact metadata differs",
        )?;
        Ok(duplicate)
    }
}

fn revalidate_path_source(
    canonical_path: &Path,
    expected_metadata: StableArtifactMetadata,
    metadata_message: &'static str,
) -> io::Result<()> {
    // O_PATH cannot be used with flistxattr. Pin the exact no-symlink pathname
    // first, authenticate its metadata, then reopen that pinned regular inode
    // read-only and compare the complete statx-plus-ioctl snapshot again. The
    // final flistxattr query remains only caller-visible-zero evidence; it does
    // not satisfy absent-v1 without the future authenticated source-epoch
    // receipt.
    let path_pin = open_path_pin(canonical_path)?;
    let path_metadata = StableFileMetadata::read(&path_pin)?;
    require_same_file_metadata(expected_metadata.file, path_metadata, metadata_message)?;

    let readable = reopen_pinned_regular(&path_pin, canonical_path)?;
    verify_descriptor_flags(&readable)?;
    let readable_metadata = StableArtifactMetadata::read_from_ordinary_descriptor(&readable)?;
    require_same_artifact_metadata(expected_metadata, readable_metadata, metadata_message)?;
    require_caller_visible_zero_xattr_names(&readable)
}

fn exact_absolute_path(path: &Path) -> io::Result<PathBuf> {
    let mut components = path.components();
    if !matches!(components.next(), Some(Component::RootDir)) {
        return Err(invalid_data("fs-verity artifact path is not absolute"));
    }
    let mut canonical = PathBuf::from("/");
    for component in components {
        let Component::Normal(component) = component else {
            return Err(invalid_data(
                "fs-verity artifact path is not lexically canonical",
            ));
        };
        canonical.push(component);
    }
    if canonical.as_os_str().as_bytes() != path.as_os_str().as_bytes() {
        return Err(invalid_data(
            "fs-verity artifact path is not its exact canonical spelling",
        ));
    }
    Ok(canonical)
}

fn open_path_pin(path: &Path) -> io::Result<File> {
    let path = CString::new(path.as_os_str().as_bytes())
        .map_err(|_| invalid_data("fs-verity artifact path contains NUL"))?;
    openat2_file(
        libc::AT_FDCWD,
        &path,
        libc::O_PATH | libc::O_CLOEXEC | libc::O_NOFOLLOW,
        libc::RESOLVE_NO_MAGICLINKS | libc::RESOLVE_NO_SYMLINKS,
    )
}

fn openat2_file(directory: i32, path: &CStr, flags: i32, resolve: u64) -> io::Result<File> {
    // libc marks open_how non-exhaustive, so initialize all current and future
    // padding to zero before setting the UAPI fields supported by this code.
    // SAFETY: an all-zero open_how is a valid starting state.
    let mut how = unsafe { MaybeUninit::<libc::open_how>::zeroed().assume_init() };
    how.flags = flags as u64;
    how.resolve = resolve;
    // SAFETY: all pointers and lengths describe live initialized objects. The
    // syscall returns a new owned descriptor or -1 with errno.
    let descriptor = unsafe {
        libc::syscall(
            libc::SYS_openat2,
            directory,
            path.as_ptr(),
            &how,
            std::mem::size_of::<libc::open_how>(),
        )
    };
    if descriptor < 0 {
        return Err(io::Error::last_os_error());
    }
    let descriptor = i32::try_from(descriptor)
        .map_err(|_| invalid_data("openat2 returned an unrepresentable descriptor"))?;
    // SAFETY: a successful openat2 returned this descriptor with unique
    // ownership to the caller.
    Ok(unsafe { File::from_raw_fd(descriptor) })
}

fn reopen_pinned_regular(path_pin: &File, canonical_path: &Path) -> io::Result<File> {
    let proc_fds = verified_proc_fd_directory()?;
    let descriptor_name = CString::new(path_pin.as_raw_fd().to_string())
        .map_err(|_| invalid_data("pinned artifact descriptor path contains NUL"))?;
    require_readlinkat(
        &proc_fds,
        &descriptor_name,
        canonical_path.as_os_str().as_bytes(),
        "pinned proc fd does not name the canonical artifact",
    )?;
    // The O_PATH descriptor was already statx-verified as a regular file on a
    // supported local filesystem. `proc_fds` pins the current process's fd
    // directory on a verified procfs mount, so following this one kernel-owned
    // magic link cannot be redirected through a caller-supplied fake `/proc`.
    // The required read-only,noatime source mount preserves atime without
    // requiring file ownership or CAP_FOWNER.
    // SAFETY: the directory and name identify a live procfs fd entry, and
    // openat returns a new descriptor or -1.
    let descriptor = unsafe {
        libc::openat(
            proc_fds.as_raw_fd(),
            descriptor_name.as_ptr(),
            libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NONBLOCK,
        )
    };
    if descriptor < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: a successful open returned this descriptor with unique ownership.
    Ok(unsafe { File::from_raw_fd(descriptor) })
}

fn verified_proc_fd_directory() -> io::Result<File> {
    let proc_root = openat2_file(
        libc::AT_FDCWD,
        c"/proc",
        libc::O_PATH | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
        libc::RESOLVE_NO_MAGICLINKS | libc::RESOLVE_NO_SYMLINKS,
    )?;
    require_filesystem_type(&proc_root, libc::PROC_SUPER_MAGIC)?;

    // Resolve the kernel-produced thread-self target, then open that ordinary
    // numeric path without following another magic link. `/proc/<tgid>/fd`
    // can refer to the leader's different fd table after CLONE_FILES is split;
    // `<tgid>/task/<tid>/fd` names the calling task's exact table.
    let thread_self = readlinkat_bounded(&proc_root, c"thread-self")?;
    validate_thread_self_target(&thread_self)?;
    let mut relative = thread_self;
    relative.extend_from_slice(b"/fd");
    let relative = CString::new(relative)
        .map_err(|_| invalid_data("current proc fd directory contains NUL"))?;
    let proc_fds = openat2_file(
        proc_root.as_raw_fd(),
        &relative,
        libc::O_PATH | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
        libc::RESOLVE_BENEATH
            | libc::RESOLVE_NO_XDEV
            | libc::RESOLVE_NO_MAGICLINKS
            | libc::RESOLVE_NO_SYMLINKS,
    )?;
    require_filesystem_type(&proc_fds, libc::PROC_SUPER_MAGIC)?;
    if !proc_fds.metadata()?.file_type().is_dir() {
        return Err(invalid_data("current proc fd path is not a directory"));
    }
    Ok(proc_fds)
}

fn validate_thread_self_target(target: &[u8]) -> io::Result<()> {
    let Some(separator) = target
        .windows(b"/task/".len())
        .position(|window| window == b"/task/")
    else {
        return Err(invalid_data("procfs thread-self target has invalid shape"));
    };
    let tgid = &target[..separator];
    let tid = &target[separator + b"/task/".len()..];
    let canonical_decimal = |component: &[u8]| {
        !component.is_empty()
            && (component.len() == 1 || component[0] != b'0')
            && component.iter().all(u8::is_ascii_digit)
            && component != b"0"
    };
    if !canonical_decimal(tgid) || !canonical_decimal(tid) {
        return Err(invalid_data("procfs thread-self target has invalid ids"));
    }
    Ok(())
}

fn readlinkat_bounded(directory: &File, name: &CStr) -> io::Result<Vec<u8>> {
    let mut target = vec![0_u8; libc::PATH_MAX as usize];
    // SAFETY: the directory and NUL-terminated name are live, while `target`
    // is writable storage of the advertised length.
    let length = unsafe {
        libc::readlinkat(
            directory.as_raw_fd(),
            name.as_ptr(),
            target.as_mut_ptr().cast(),
            target.len(),
        )
    };
    if length < 0 {
        return Err(io::Error::last_os_error());
    }
    let length = usize::try_from(length)
        .map_err(|_| invalid_data("procfs link length is not representable"))?;
    if length == target.len() {
        return Err(invalid_data("procfs link exceeds the byte bound"));
    }
    target.truncate(length);
    Ok(target)
}

fn require_readlinkat(
    directory: &File,
    name: &CStr,
    expected: &[u8],
    message: &'static str,
) -> io::Result<()> {
    let target = readlinkat_bounded(directory, name)?;
    if target != expected {
        return Err(invalid_data(message));
    }
    Ok(())
}

fn require_filesystem_type(file: &File, expected: libc::c_long) -> io::Result<()> {
    let observed = filesystem_type(file)?;
    if observed != expected {
        return Err(invalid_data("descriptor filesystem type differs"));
    }
    Ok(())
}

fn filesystem_type(file: &File) -> io::Result<libc::c_long> {
    let mut filesystem = MaybeUninit::<libc::statfs>::zeroed();
    // SAFETY: `file` owns a live descriptor and `filesystem` is writable.
    if unsafe { libc::fstatfs(file.as_raw_fd(), filesystem.as_mut_ptr()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: successful fstatfs initialized the output.
    Ok(unsafe { filesystem.assume_init() }.f_type)
}

fn stable_mount_policy(file: &File) -> io::Result<(i64, u64)> {
    let filesystem = filesystem_type(file)?;
    if ![
        libc::BTRFS_SUPER_MAGIC,
        libc::EXT4_SUPER_MAGIC,
        libc::F2FS_SUPER_MAGIC,
    ]
    .contains(&filesystem)
    {
        return Err(invalid_data(
            "fs-verity artifact is not on a supported local filesystem",
        ));
    }

    let mut mount = MaybeUninit::<libc::statvfs>::zeroed();
    // SAFETY: `file` owns a live descriptor and `mount` is writable.
    if unsafe { libc::fstatvfs(file.as_raw_fd(), mount.as_mut_ptr()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: successful fstatvfs initialized the output.
    let mount = unsafe { mount.assume_init() };
    let required = libc::ST_RDONLY | libc::ST_NOATIME;
    if mount.f_flag & required != required {
        return Err(invalid_data(
            "fs-verity store mount is not read-only and noatime",
        ));
    }
    Ok((filesystem as i64, mount.f_flag))
}

fn verify_descriptor_flags(file: &File) -> io::Result<()> {
    // SAFETY: `file` owns a live descriptor and F_GETFL/F_GETFD take no third
    // argument or caller-provided memory.
    let status_flags = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GETFL) };
    if status_flags < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: identical descriptor-lifetime and command argument reasoning.
    let descriptor_flags = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GETFD) };
    if descriptor_flags < 0 {
        return Err(io::Error::last_os_error());
    }
    if status_flags & libc::O_ACCMODE != libc::O_RDONLY || descriptor_flags & libc::FD_CLOEXEC == 0
    {
        return Err(invalid_data(
            "fs-verity artifact descriptor is not read-only and close-on-exec",
        ));
    }
    Ok(())
}

fn require_caller_visible_zero_xattr_names(file: &File) -> io::Result<()> {
    // A zero-sized list query returns the exact byte count required for every
    // name visible to this caller, including its trailing NUL. Exactly zero is
    // retained only as caller-visible-zero evidence at this observation. It
    // cannot prove absent-v1 across hidden namespaces or the source epoch; that
    // requires a future authenticated broker receipt. Any positive result is
    // rejected rather than read into a possibly truncated buffer, and every
    // syscall error (including ENOTSUP, EPERM, and ERANGE) remains a hard
    // failure.
    // SAFETY: `file` owns a live readable descriptor. A null output pointer is
    // valid for the documented zero-sized flistxattr length query.
    let result = unsafe { libc::flistxattr(file.as_raw_fd(), ptr::null_mut(), 0) };
    let result = if result < 0 {
        Err(io::Error::last_os_error())
    } else {
        usize::try_from(result)
            .map_err(|_| invalid_data("xattr enumeration length is not representable"))
    };
    require_caller_visible_zero_xattr_enumeration(result)
}

fn require_caller_visible_zero_xattr_enumeration(result: io::Result<usize>) -> io::Result<()> {
    match result {
        Ok(0) => Ok(()),
        Ok(_) => Err(invalid_data(
            "fs-verity artifact has an xattr name visible to the caller",
        )),
        Err(error) => Err(error),
    }
}

fn duplicate_readonly_cloexec(file: &File) -> io::Result<File> {
    let duplicate = file.try_clone()?;
    verify_descriptor_flags(&duplicate)?;
    Ok(duplicate)
}

fn require_expected_identity(
    observed: FileIdentity,
    expected: StableVerityExpectation,
) -> io::Result<()> {
    if observed != expected.identity {
        return Err(invalid_data("fs-verity artifact identity differs"));
    }
    Ok(())
}

fn require_expected_metadata(
    observed_identity: FileIdentity,
    observed_metadata_sha256: [u8; SHA256_DIGEST_BYTES],
    expected: StableVerityExpectation,
) -> io::Result<()> {
    require_expected_identity(observed_identity, expected)?;
    if observed_metadata_sha256 != expected.metadata_sha256 {
        return Err(invalid_data("fs-verity artifact metadata SHA-256 differs"));
    }
    Ok(())
}

fn require_exact_sha256(
    observed: [u8; SHA256_DIGEST_BYTES],
    expected: [u8; SHA256_DIGEST_BYTES],
    message: &'static str,
) -> io::Result<()> {
    if observed != expected {
        return Err(invalid_data(message));
    }
    Ok(())
}

fn require_same_file_metadata(
    expected: StableFileMetadata,
    observed: StableFileMetadata,
    message: &'static str,
) -> io::Result<()> {
    if observed != expected {
        return Err(invalid_data(message));
    }
    Ok(())
}

fn require_same_artifact_metadata(
    expected: StableArtifactMetadata,
    observed: StableArtifactMetadata,
    message: &'static str,
) -> io::Result<()> {
    if observed != expected {
        return Err(invalid_data(message));
    }
    Ok(())
}

fn measure_sha256_verity(file: &File) -> io::Result<[u8; SHA256_DIGEST_BYTES]> {
    let mut packet = FsVerityDigestPacket::for_sha256_measurement();
    // Deriving the header pointer from the complete packet preserves the
    // allocation that backs the flexible-array bytes. `repr(C)` and the
    // compile-time layout assertions above prove that the 4-byte header is at
    // offset zero and the advertised 32-byte capacity follows immediately.
    // The nix macro intentionally encodes `FsVerityDigestHeader` (4 bytes), not
    // `FsVerityDigestPacket` (36 bytes), into FS_IOC_MEASURE_VERITY. Linux uses
    // the input digest_size as the capacity and cannot write beyond it.
    let header = ptr::from_mut(&mut packet).cast::<FsVerityDigestHeader>();
    // SAFETY: `header` identifies a writable, correctly aligned 36-byte packet
    // for the ioctl's 4-byte header plus the declared 32-byte digest capacity.
    unsafe { fs_ioc_measure_verity(file.as_raw_fd(), header) }.map_err(io::Error::from)?;
    validate_sha256_verity_response(&packet)
}

fn validate_sha256_verity_response(
    packet: &FsVerityDigestPacket,
) -> io::Result<[u8; SHA256_DIGEST_BYTES]> {
    if packet.header.digest_algorithm != FS_VERITY_HASH_ALG_SHA256
        || usize::from(packet.header.digest_size) != SHA256_DIGEST_BYTES
    {
        return Err(invalid_data(
            "fs-verity measurement is not exactly one SHA-256 digest",
        ));
    }
    Ok(packet.digest)
}

fn read_exact_bounded(file: &File, expected_length: u64, maximum: usize) -> io::Result<Vec<u8>> {
    let expected_length = usize::try_from(expected_length)
        .map_err(|_| invalid_data("fs-verity artifact length is not addressable"))?;
    if expected_length > maximum {
        return Err(invalid_data("fs-verity artifact exceeds its byte bound"));
    }

    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(expected_length)
        .map_err(|error| io::Error::other(format!("reserve artifact snapshot: {error}")))?;
    let mut offset = 0_u64;
    let mut chunk = [0_u8; READ_CHUNK_BYTES];
    loop {
        let remaining = maximum.saturating_sub(bytes.len());
        let request_length = remaining.saturating_add(1).min(chunk.len());
        if request_length == 0 {
            return Err(invalid_data("fs-verity artifact read bound overflow"));
        }
        let read = loop {
            match file.read_at(&mut chunk[..request_length], offset) {
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                result => break result?,
            }
        };
        if read == 0 {
            break;
        }
        bytes
            .try_reserve(read)
            .map_err(|error| io::Error::other(format!("grow artifact snapshot: {error}")))?;
        bytes.extend_from_slice(&chunk[..read]);
        if bytes.len() > maximum {
            return Err(invalid_data("fs-verity artifact exceeds its byte bound"));
        }
        offset = offset
            .checked_add(
                u64::try_from(read)
                    .map_err(|_| invalid_data("artifact read length is not representable"))?,
            )
            .ok_or_else(|| invalid_data("artifact read offset overflow"))?;
    }
    if bytes.len() != expected_length {
        return Err(invalid_data(
            "fs-verity artifact length differs from its descriptor stamp",
        ));
    }
    Ok(bytes)
}

fn prove_executable_source(
    file: &File,
    length: usize,
    source_mode: u32,
) -> io::Result<ExecutableSourceProof> {
    if length == 0 {
        return Err(invalid_data(
            "empty fs-verity artifact cannot prove executable source access",
        ));
    }
    let executable_permission_bits = require_executable_permission_bits(source_mode)?;
    // Any nonzero length asks the kernel to establish at least one mapped page.
    // Limiting the requested range avoids implying that an arbitrary artifact
    // was eagerly faulted or validated in its entirety.
    let mapped_length = length.min(READ_CHUNK_BYTES);
    // SAFETY: the retained descriptor is a read-only regular file, the offset
    // is page-aligned zero, and `mapped_length` is nonzero. No mapped byte is
    // called or dereferenced; the mapping exists only to make the kernel apply
    // executable-mount policy to this source.
    let mapping = unsafe {
        libc::mmap(
            ptr::null_mut(),
            mapped_length,
            libc::PROT_READ | libc::PROT_EXEC,
            libc::MAP_PRIVATE,
            file.as_raw_fd(),
            0,
        )
    };
    if mapping == libc::MAP_FAILED {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `mapping` is exactly the base returned by the successful mmap,
    // and the length is exactly the nonzero length passed to that call.
    if unsafe { libc::munmap(mapping, mapped_length) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(ExecutableSourceProof {
        mapped_length,
        executable_permission_bits,
    })
}

fn require_executable_permission_bits(source_mode: u32) -> io::Result<u32> {
    let executable_permission_bits = source_mode & 0o111;
    if executable_permission_bits == 0 {
        return Err(invalid_data(
            "fs-verity executable source has no executable permission bit",
        ));
    }
    Ok(executable_permission_bits)
}

fn invalid_data(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

#[cfg(test)]
mod tests {
    use std::io::Read;
    use std::io::Write;

    use super::*;

    fn sample_metadata() -> StableFileMetadata {
        StableFileMetadata {
            mask: REQUIRED_STATX_MASK,
            block_size: 4096,
            // Linux 7.1 Btrfs reports all three values but leaves VERITY out
            // of the capability mask below.
            attributes: 0x0010_0050,
            link_count: 1,
            uid: 1000,
            gid: 1000,
            mode: (libc::S_IFREG | 0o555) as u16,
            inode: 17,
            size: 8192,
            blocks: 16,
            // AUTOMOUNT and DAX come from the VFS; Btrfs adds COMPRESSED,
            // APPEND, IMMUTABLE, and NODUMP.
            attributes_mask: 0x0020_1074,
            access_time: StableTimestamp {
                seconds: 1,
                nanoseconds: 2,
            },
            birth_time: StableTimestamp {
                seconds: 3,
                nanoseconds: 4,
            },
            change_time: StableTimestamp {
                seconds: 5,
                nanoseconds: 6,
            },
            modification_time: StableTimestamp {
                seconds: 7,
                nanoseconds: 8,
            },
            rdev_major: 0,
            rdev_minor: 0,
            device_major: 259,
            device_minor: 2,
            mount_id: 41,
            unique_mount_mask: REQUIRED_STATX_MASK | STATX_MNT_ID_UNIQUE,
            unique_mount_id: 4_294_967_337,
            dio_memory_alignment: 512,
            dio_offset_alignment: 512,
            subvolume: 256,
            atomic_write_unit_min: 0,
            atomic_write_unit_max: 0,
            atomic_write_segments_max: 0,
            dio_read_offset_alignment: 512,
            atomic_write_unit_max_optimized: 0,
            filesystem_type: libc::BTRFS_SUPER_MAGIC,
            mount_flags: libc::ST_RDONLY | libc::ST_NOATIME,
        }
    }

    fn sample_inode_policy_encoding() -> [u8; 38] {
        [
            0x02, // Btrfs profile.
            0x3e, 0x68, 0x23, 0x91, 0x00, 0x00, 0x00, 0x00, // Btrfs magic.
            0x01, // VerityArtifact.
            0x50, 0x00, 0x10, 0x00, // flags.
            0x88, 0x00, 0x02, 0x00, // xflags.
            0x00, 0x00, 0x00, 0x00, // extsize.
            0x00, 0x00, 0x00, 0x00, // nextents.
            0x00, 0x00, 0x00, 0x00, // project_id.
            0x00, 0x00, 0x00, 0x00, // cowextsize.
            0x0b, 0x00, 0x00, 0x00, // required generation value.
        ]
    }

    fn sample_metadata_digest(metadata: StableFileMetadata) -> [u8; SHA256_DIGEST_BYTES] {
        metadata_digest_from_encoding(
            metadata,
            &sample_inode_policy_encoding(),
            STABLE_XATTR_POLICY.as_bytes(),
            CALLER_VISIBLE_XATTR_EVIDENCE,
        )
    }

    #[test]
    fn measure_verity_request_encodes_only_the_flexible_header() {
        let header_request = nix::request_code_readwrite!(
            b'f',
            FS_IOC_MEASURE_VERITY_NUMBER,
            std::mem::size_of::<FsVerityDigestHeader>()
        );
        let packet_request = nix::request_code_readwrite!(
            b'f',
            FS_IOC_MEASURE_VERITY_NUMBER,
            std::mem::size_of::<FsVerityDigestPacket>()
        );

        assert_eq!(std::mem::size_of::<FsVerityDigestHeader>(), 4);
        assert_eq!(std::mem::offset_of!(FsVerityDigestPacket, digest), 4);
        assert_eq!(std::mem::size_of::<FsVerityDigestPacket>(), 36);
        assert_eq!(header_request, 0xc004_6686);
        assert_eq!(packet_request, 0xc024_6686);
        assert_ne!(header_request, packet_request);
    }

    #[test]
    fn verity_response_requires_sha256_and_exact_digest_length() {
        let digest = [0xa5; SHA256_DIGEST_BYTES];
        let valid = FsVerityDigestPacket {
            header: FsVerityDigestHeader {
                digest_algorithm: FS_VERITY_HASH_ALG_SHA256,
                digest_size: SHA256_DIGEST_BYTES as u16,
            },
            digest,
        };
        assert_eq!(validate_sha256_verity_response(&valid).unwrap(), digest);

        let mut wrong_algorithm = valid;
        wrong_algorithm.header.digest_algorithm = 2;
        assert!(validate_sha256_verity_response(&wrong_algorithm).is_err());

        let mut short = valid;
        short.header.digest_size -= 1;
        assert!(validate_sha256_verity_response(&short).is_err());

        let mut long = valid;
        long.header.digest_size += 1;
        assert!(validate_sha256_verity_response(&long).is_err());
    }

    #[test]
    fn executable_source_requires_an_executable_permission_bit() {
        assert_eq!(require_executable_permission_bits(0o100700).unwrap(), 0o100);
        assert_eq!(require_executable_permission_bits(0o100050).unwrap(), 0o010);
        assert_eq!(require_executable_permission_bits(0o100001).unwrap(), 0o001);
        assert!(require_executable_permission_bits(0o100644).is_err());
    }

    #[test]
    fn caller_visible_zero_xattr_evidence_accepts_only_zero_enumeration() {
        require_caller_visible_zero_xattr_enumeration(Ok(0)).unwrap();

        for positive_size in [1, 256, usize::MAX] {
            assert!(
                require_caller_visible_zero_xattr_enumeration(Ok(positive_size)).is_err(),
                "accepted positive xattr list size {positive_size}"
            );
        }

        for errno in [libc::ENOTSUP, libc::EPERM, libc::ERANGE, libc::EIO] {
            let error = require_caller_visible_zero_xattr_enumeration(Err(
                io::Error::from_raw_os_error(errno),
            ))
            .unwrap_err();
            assert_eq!(
                error.raw_os_error(),
                Some(errno),
                "did not preserve xattr enumeration errno {errno}"
            );
        }
    }

    #[test]
    fn readonly_cloexec_duplicate_has_independent_descriptor_ownership() {
        let mut descriptors = [-1; 2];
        // SAFETY: `descriptors` has storage for both descriptors written by
        // pipe2. On success each descriptor has unique ownership below.
        assert_eq!(
            unsafe { libc::pipe2(descriptors.as_mut_ptr(), libc::O_CLOEXEC) },
            0
        );
        // SAFETY: successful pipe2 returned these two uniquely owned fds.
        let original = unsafe { File::from_raw_fd(descriptors[0]) };
        // SAFETY: as above, for the pipe's write end.
        let mut writer = unsafe { File::from_raw_fd(descriptors[1]) };

        let mut duplicate = duplicate_readonly_cloexec(&original).unwrap();
        assert_ne!(original.as_raw_fd(), duplicate.as_raw_fd());
        // SAFETY: F_GETFD takes no third argument and duplicate remains live.
        let descriptor_flags = unsafe { libc::fcntl(duplicate.as_raw_fd(), libc::F_GETFD) };
        assert!(descriptor_flags >= 0);
        assert_ne!(descriptor_flags & libc::FD_CLOEXEC, 0);

        drop(original);
        writer.write_all(&[0xa5]).unwrap();
        let mut byte = [0_u8; 1];
        duplicate.read_exact(&mut byte).unwrap();
        assert_eq!(byte, [0xa5]);
    }

    #[test]
    fn artifact_path_must_be_exact_absolute_and_lexically_canonical() {
        assert_eq!(
            exact_absolute_path(Path::new("/stable/profile/artifact")).unwrap(),
            Path::new("/stable/profile/artifact")
        );
        for invalid in [
            "stable/profile/artifact",
            "/stable/./profile/artifact",
            "/stable/profile/../artifact",
            "/stable//profile/artifact",
            "/stable/profile/artifact/",
        ] {
            assert!(
                exact_absolute_path(Path::new(invalid)).is_err(),
                "accepted noncanonical artifact path {invalid:?}"
            );
        }
    }

    #[test]
    fn thread_self_target_requires_canonical_tgid_task_tid_shape() {
        for valid in [b"1/task/1" as &[u8], b"123/task/456"] {
            validate_thread_self_target(valid).unwrap();
        }
        for invalid in [
            b"" as &[u8],
            b"self",
            b"0/task/1",
            b"1/task/0",
            b"01/task/1",
            b"1/task/01",
            b"1/task/2/fd",
            b"1/task/2/task/3",
            b"-1/task/2",
            b"1/task/-2",
        ] {
            assert!(
                validate_thread_self_target(invalid).is_err(),
                "accepted invalid thread-self target {:?}",
                String::from_utf8_lossy(invalid)
            );
        }
    }

    #[test]
    fn metadata_digest_binds_inode_times_mount_ioctl_and_xattr_evidence() {
        let metadata = sample_metadata();
        let policy = sample_inode_policy_encoding();
        let expected = sample_metadata_digest(metadata);
        assert_eq!(
            expected,
            [
                0x35, 0x70, 0x43, 0x54, 0x6f, 0x0a, 0x25, 0xad, 0x69, 0x44, 0xa7, 0x5f, 0x0c, 0xc6,
                0x5a, 0xba, 0x4b, 0x49, 0x3a, 0xd4, 0x2d, 0xad, 0x0f, 0x93, 0x16, 0xe8, 0x9a, 0xa5,
                0xa6, 0x30, 0xd5, 0xa5,
            ]
        );
        assert_ne!(
            metadata_digest_from_encoding(
                metadata,
                &policy,
                b"absent-v2",
                CALLER_VISIBLE_XATTR_EVIDENCE,
            ),
            expected,
            "required xattr policy semantic tag was not digest-bound"
        );
        assert_ne!(
            metadata_digest_from_encoding(
                metadata,
                &policy,
                STABLE_XATTR_POLICY.as_bytes(),
                b"caller-visible-zero-v2",
            ),
            expected,
            "caller-visible xattr evidence tag was not digest-bound"
        );
        let mut changed_policy = policy;
        let last_policy_byte = changed_policy.len() - 1;
        changed_policy[last_policy_byte] ^= 1;
        assert_ne!(
            metadata_digest_from_encoding(
                metadata,
                &changed_policy,
                STABLE_XATTR_POLICY.as_bytes(),
                CALLER_VISIBLE_XATTR_EVIDENCE,
            ),
            expected,
            "inode ioctl policy encoding was not digest-bound"
        );

        let mutators: &[fn(&mut StableFileMetadata)] = &[
            |m| m.mask ^= 1,
            |m| m.block_size += 1,
            |m| m.attributes ^= 1,
            |m| m.link_count += 1,
            |m| m.uid += 1,
            |m| m.gid += 1,
            |m| m.mode ^= 1,
            |m| m.inode += 1,
            |m| m.size += 1,
            |m| m.blocks += 1,
            |m| m.attributes_mask ^= 1,
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
            |m| m.device_major += 1,
            |m| m.device_minor += 1,
            |m| m.mount_id += 1,
            |m| m.unique_mount_mask ^= 1,
            |m| m.unique_mount_id += 1,
            |m| m.dio_memory_alignment += 1,
            |m| m.dio_offset_alignment += 1,
            |m| m.subvolume += 1,
            |m| m.atomic_write_unit_min += 1,
            |m| m.atomic_write_unit_max += 1,
            |m| m.atomic_write_segments_max += 1,
            |m| m.dio_read_offset_alignment += 1,
            |m| m.atomic_write_unit_max_optimized += 1,
            |m| m.filesystem_type ^= 1,
            |m| m.mount_flags ^= 1,
        ];
        for (index, mutate) in mutators.iter().enumerate() {
            let mut changed = metadata;
            mutate(&mut changed);
            assert_ne!(
                sample_metadata_digest(changed),
                expected,
                "metadata field mutator {index} was not digest-bound"
            );
        }
    }

    #[test]
    fn manifest_expectation_binds_identity_and_complete_metadata_digest() {
        let metadata = sample_metadata();
        let metadata_sha256 = sample_metadata_digest(metadata);
        let expected = StableVerityExpectation::new(
            [0x11; SHA256_DIGEST_BYTES],
            [0x22; SHA256_DIGEST_BYTES],
            metadata.identity(),
            metadata_sha256,
        );
        require_expected_metadata(metadata.identity(), metadata_sha256, expected).unwrap();
        assert_eq!(expected.content_sha256(), [0x11; SHA256_DIGEST_BYTES]);
        assert_eq!(expected.verity_sha256(), [0x22; SHA256_DIGEST_BYTES]);
        assert_eq!(expected.identity(), metadata.identity());
        assert_eq!(expected.metadata_sha256(), metadata_sha256);
        assert_eq!(STABLE_XATTR_POLICY, "absent-v1");
        assert_eq!(CALLER_VISIBLE_XATTR_EVIDENCE, b"caller-visible-zero-v1");

        let wrong_identity = StableVerityExpectation::new(
            [0x11; SHA256_DIGEST_BYTES],
            [0x22; SHA256_DIGEST_BYTES],
            FileIdentity {
                device: metadata.identity().device,
                inode: metadata.identity().inode + 1,
            },
            metadata_sha256,
        );
        assert!(
            require_expected_metadata(metadata.identity(), metadata_sha256, wrong_identity)
                .is_err()
        );

        let wrong_metadata = StableVerityExpectation::new(
            [0x11; SHA256_DIGEST_BYTES],
            [0x22; SHA256_DIGEST_BYTES],
            metadata.identity(),
            [0x33; SHA256_DIGEST_BYTES],
        );
        assert!(
            require_expected_metadata(metadata.identity(), metadata_sha256, wrong_metadata)
                .is_err()
        );
    }

    #[test]
    fn manifest_digest_comparison_refuses_content_or_verity_difference() {
        let expected = [0x44; SHA256_DIGEST_BYTES];
        require_exact_sha256(expected, expected, "digest differs").unwrap();

        let mut different = expected;
        different[SHA256_DIGEST_BYTES - 1] ^= 1;
        assert!(require_exact_sha256(different, expected, "content differs").is_err());
        assert!(require_exact_sha256(expected, different, "verity differs").is_err());
    }
}
