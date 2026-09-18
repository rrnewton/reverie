//! Exact inode-ioctl policy for stable after-loader sources.
//!
//! `statx` does not expose all pathname- and behavior-affecting inode state.
//! This module samples the generic Linux file-attribute ioctls without a zero,
//! unsupported, or statx fallback and gives the stable-store and stable-cover
//! digests one canonical representation.

use std::io;
use std::os::fd::RawFd;

const EXT4_SUPER_MAGIC: i64 = 0xef53;
const BTRFS_SUPER_MAGIC: i64 = 0x9123_683e;
const F2FS_SUPER_MAGIC: i64 = 0xf2f5_2010;
const XFS_SUPER_MAGIC: i64 = 0x5846_5342;

const FS_IOC_GETFLAGS: libc::c_ulong = 0x8008_6601;
const FS_IOC_GETVERSION: libc::c_ulong = 0x8008_7601;
const EXT4_IOC_GETVERSION: libc::c_ulong = 0x8008_6603;
const FS_IOC_FSGETXATTR: libc::c_ulong = 0x801c_581f;

const FS_SECRM_FL: u32 = 0x0000_0001;
const FS_UNRM_FL: u32 = 0x0000_0002;
const FS_COMPR_FL: u32 = 0x0000_0004;
const FS_SYNC_FL: u32 = 0x0000_0008;
const FS_IMMUTABLE_FL: u32 = 0x0000_0010;
const FS_APPEND_FL: u32 = 0x0000_0020;
const FS_NODUMP_FL: u32 = 0x0000_0040;
const FS_NOATIME_FL: u32 = 0x0000_0080;
const FS_DIRTY_FL: u32 = 0x0000_0100;
const FS_COMPRBLK_FL: u32 = 0x0000_0200;
const FS_NOCOMP_FL: u32 = 0x0000_0400;
const FS_ENCRYPT_FL: u32 = 0x0000_0800;
const FS_INDEX_FL: u32 = 0x0000_1000;
const FS_IMAGIC_FL: u32 = 0x0000_2000;
const FS_JOURNAL_DATA_FL: u32 = 0x0000_4000;
const FS_NOTAIL_FL: u32 = 0x0000_8000;
const FS_DIRSYNC_FL: u32 = 0x0001_0000;
const FS_TOPDIR_FL: u32 = 0x0002_0000;
const FS_HUGE_FILE_FL: u32 = 0x0004_0000;
const FS_EXTENT_FL: u32 = 0x0008_0000;
const FS_VERITY_FL: u32 = 0x0010_0000;
const FS_EA_INODE_FL: u32 = 0x0020_0000;
const FS_EOFBLOCKS_FL: u32 = 0x0040_0000;
const FS_NOCOW_FL: u32 = 0x0080_0000;
const FS_DAX_FL: u32 = 0x0200_0000;
const FS_INLINE_DATA_FL: u32 = 0x1000_0000;
const FS_PROJINHERIT_FL: u32 = 0x2000_0000;
const FS_CASEFOLD_FL: u32 = 0x4000_0000;
const FS_RESERVED_FL: u32 = 0x8000_0000;
const KNOWN_FS_FLAGS: u32 = FS_SECRM_FL
    | FS_UNRM_FL
    | FS_COMPR_FL
    | FS_SYNC_FL
    | FS_IMMUTABLE_FL
    | FS_APPEND_FL
    | FS_NODUMP_FL
    | FS_NOATIME_FL
    | FS_DIRTY_FL
    | FS_COMPRBLK_FL
    | FS_NOCOMP_FL
    | FS_ENCRYPT_FL
    | FS_INDEX_FL
    | FS_IMAGIC_FL
    | FS_JOURNAL_DATA_FL
    | FS_NOTAIL_FL
    | FS_DIRSYNC_FL
    | FS_TOPDIR_FL
    | FS_HUGE_FILE_FL
    | FS_EXTENT_FL
    | FS_VERITY_FL
    | FS_EA_INODE_FL
    | FS_EOFBLOCKS_FL
    | FS_NOCOW_FL
    | FS_DAX_FL
    | FS_INLINE_DATA_FL
    | FS_PROJINHERIT_FL
    | FS_CASEFOLD_FL
    | FS_RESERVED_FL;

const FS_XFLAG_REALTIME: u32 = 0x0000_0001;
const FS_XFLAG_PREALLOC: u32 = 0x0000_0002;
const FS_XFLAG_IMMUTABLE: u32 = 0x0000_0008;
const FS_XFLAG_APPEND: u32 = 0x0000_0010;
const FS_XFLAG_SYNC: u32 = 0x0000_0020;
const FS_XFLAG_NOATIME: u32 = 0x0000_0040;
const FS_XFLAG_NODUMP: u32 = 0x0000_0080;
const FS_XFLAG_RTINHERIT: u32 = 0x0000_0100;
const FS_XFLAG_PROJINHERIT: u32 = 0x0000_0200;
const FS_XFLAG_NOSYMLINKS: u32 = 0x0000_0400;
const FS_XFLAG_EXTSIZE: u32 = 0x0000_0800;
const FS_XFLAG_EXTSZINHERIT: u32 = 0x0000_1000;
const FS_XFLAG_NODEFRAG: u32 = 0x0000_2000;
const FS_XFLAG_FILESTREAM: u32 = 0x0000_4000;
const FS_XFLAG_DAX: u32 = 0x0000_8000;
const FS_XFLAG_COWEXTSIZE: u32 = 0x0001_0000;
// Introduced after some libc/kernel-header snapshots used by this workspace.
const FS_XFLAG_VERITY: u32 = 0x0002_0000;
const FS_XFLAG_HASATTR: u32 = 0x8000_0000;
const KNOWN_FS_XFLAGS: u32 = FS_XFLAG_REALTIME
    | FS_XFLAG_PREALLOC
    | FS_XFLAG_IMMUTABLE
    | FS_XFLAG_APPEND
    | FS_XFLAG_SYNC
    | FS_XFLAG_NOATIME
    | FS_XFLAG_NODUMP
    | FS_XFLAG_RTINHERIT
    | FS_XFLAG_PROJINHERIT
    | FS_XFLAG_NOSYMLINKS
    | FS_XFLAG_EXTSIZE
    | FS_XFLAG_EXTSZINHERIT
    | FS_XFLAG_NODEFRAG
    | FS_XFLAG_FILESTREAM
    | FS_XFLAG_DAX
    | FS_XFLAG_COWEXTSIZE
    | FS_XFLAG_VERITY
    | FS_XFLAG_HASATTR;

const COMMON_STABLE_FS_FLAGS: u32 = FS_SYNC_FL | FS_IMMUTABLE_FL | FS_NODUMP_FL | FS_NOATIME_FL;
const COMMON_STABLE_FS_XFLAGS: u32 =
    FS_XFLAG_IMMUTABLE | FS_XFLAG_SYNC | FS_XFLAG_NOATIME | FS_XFLAG_NODUMP;

const STATX_ATTR_COMPRESSED: u64 = 0x0000_0004;
const STATX_ATTR_IMMUTABLE: u64 = 0x0000_0010;
const STATX_ATTR_APPEND: u64 = 0x0000_0020;
const STATX_ATTR_NODUMP: u64 = 0x0000_0040;
const STATX_ATTR_ENCRYPTED: u64 = 0x0000_0800;
const STATX_ATTR_VERITY: u64 = 0x0010_0000;
const STATX_ATTR_DAX: u64 = 0x0020_0000;
const STATX_INODE_POLICY_ATTRIBUTES: u64 = STATX_ATTR_COMPRESSED
    | STATX_ATTR_IMMUTABLE
    | STATX_ATTR_APPEND
    | STATX_ATTR_NODUMP
    | STATX_ATTR_ENCRYPTED
    | STATX_ATTR_VERITY
    | STATX_ATTR_DAX;

/// Kernel layout for `struct fsxattr` used by `FS_IOC_FSGETXATTR`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct KernelFsxattr {
    xflags: u32,
    extsize: u32,
    nextents: u32,
    project_id: u32,
    cowextsize: u32,
    padding: [u8; 8],
}

const _: () = assert!(std::mem::size_of::<KernelFsxattr>() == 28);
const _: () = assert!(std::mem::align_of::<KernelFsxattr>() == 4);
const _: () = assert!(std::mem::offset_of!(KernelFsxattr, xflags) == 0);
const _: () = assert!(std::mem::offset_of!(KernelFsxattr, cowextsize) == 16);
const _: () = assert!(std::mem::offset_of!(KernelFsxattr, padding) == 20);

/// Stable source role used to validate role-specific inode flags.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum StableInodeKind {
    /// Fs-verity regular artifact, executable or data-only.
    VerityArtifact,
    /// Stable cover directory, including its root.
    CoverDirectory,
    /// Empty regular file used only as a bind target.
    CoverPlaceholder,
}

impl StableInodeKind {
    const fn encoding_tag(self) -> u8 {
        match self {
            Self::VerityArtifact => 1,
            Self::CoverDirectory => 2,
            Self::CoverPlaceholder => 3,
        }
    }
}

/// Exact Linux 7.1 filesystem implementation whose ioctl/statx behavior was
/// reviewed. A profile is selected from `f_type`; unknown filesystems and
/// unsupported role combinations fail closed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum StableFilesystemProfile {
    Ext4,
    Btrfs,
    /// F2FS sources staged with `noinline_data,noinline_dentry`; every source
    /// inode is also checked to have `FS_INLINE_DATA_FL` clear.
    F2fsNoInline,
    Xfs,
}

impl StableFilesystemProfile {
    fn from_filesystem_type(filesystem_type: i64, kind: StableInodeKind) -> io::Result<Self> {
        let profile = match filesystem_type {
            EXT4_SUPER_MAGIC => Self::Ext4,
            BTRFS_SUPER_MAGIC => Self::Btrfs,
            F2FS_SUPER_MAGIC => Self::F2fsNoInline,
            XFS_SUPER_MAGIC => Self::Xfs,
            _ => return Err(invalid("unsupported stable inode filesystem profile")),
        };
        profile.validate_kind(kind)?;
        Ok(profile)
    }

    fn validate_kind(self, kind: StableInodeKind) -> io::Result<()> {
        if self == Self::Xfs && kind == StableInodeKind::VerityArtifact {
            return Err(invalid(
                "XFS is not a reviewed fs-verity artifact filesystem",
            ));
        }
        Ok(())
    }

    const fn filesystem_type(self) -> i64 {
        match self {
            Self::Ext4 => EXT4_SUPER_MAGIC,
            Self::Btrfs => BTRFS_SUPER_MAGIC,
            Self::F2fsNoInline => F2FS_SUPER_MAGIC,
            Self::Xfs => XFS_SUPER_MAGIC,
        }
    }

    const fn encoding_tag(self) -> u8 {
        match self {
            Self::Ext4 => 1,
            Self::Btrfs => 2,
            Self::F2fsNoInline => 3,
            Self::Xfs => 4,
        }
    }

    /// Exact subset of `statx.stx_attributes_mask` controlled by the reviewed
    /// Linux 7.1 VFS and filesystem implementations. The VFS always adds DAX;
    /// each filesystem adds the remaining capability bits below.
    const fn statx_capability_mask(self) -> u64 {
        let filesystem_mask = match self {
            Self::Ext4 | Self::F2fsNoInline => {
                STATX_ATTR_COMPRESSED
                    | STATX_ATTR_APPEND
                    | STATX_ATTR_ENCRYPTED
                    | STATX_ATTR_IMMUTABLE
                    | STATX_ATTR_NODUMP
                    | STATX_ATTR_VERITY
            }
            Self::Btrfs => {
                STATX_ATTR_COMPRESSED | STATX_ATTR_APPEND | STATX_ATTR_IMMUTABLE | STATX_ATTR_NODUMP
            }
            Self::Xfs => STATX_ATTR_APPEND | STATX_ATTR_IMMUTABLE | STATX_ATTR_NODUMP,
        };
        filesystem_mask | STATX_ATTR_DAX
    }

    const fn allowed_flags(self, kind: StableInodeKind) -> u32 {
        let filesystem_flags = match (self, kind) {
            (Self::Ext4, _) => FS_EXTENT_FL,
            (Self::Btrfs, _) => FS_NOCOW_FL,
            (
                Self::F2fsNoInline,
                StableInodeKind::VerityArtifact | StableInodeKind::CoverPlaceholder,
            ) => FS_NOCOW_FL,
            (Self::F2fsNoInline | Self::Xfs, _) => 0,
        };
        let role_flags = match (self, kind) {
            (Self::Ext4, StableInodeKind::VerityArtifact)
            | (Self::Btrfs, StableInodeKind::VerityArtifact)
            | (Self::F2fsNoInline, StableInodeKind::VerityArtifact) => FS_VERITY_FL,
            (Self::Ext4, StableInodeKind::CoverDirectory) => {
                FS_INDEX_FL | FS_DIRSYNC_FL | FS_TOPDIR_FL | FS_PROJINHERIT_FL
            }
            (Self::Btrfs, StableInodeKind::CoverDirectory) => FS_DIRSYNC_FL,
            (Self::F2fsNoInline, StableInodeKind::CoverDirectory) => {
                FS_INDEX_FL | FS_DIRSYNC_FL | FS_PROJINHERIT_FL
            }
            (Self::Xfs, StableInodeKind::CoverDirectory) => FS_PROJINHERIT_FL,
            _ => 0,
        };
        COMMON_STABLE_FS_FLAGS | filesystem_flags | role_flags
    }

    const fn allowed_xflags(self, kind: StableInodeKind) -> u32 {
        let filesystem_xflags = match self {
            Self::Xfs => FS_XFLAG_HASATTR,
            Self::Ext4 | Self::Btrfs | Self::F2fsNoInline => 0,
        };
        let role_xflags = match (self, kind) {
            (Self::Ext4, StableInodeKind::VerityArtifact)
            | (Self::Btrfs, StableInodeKind::VerityArtifact)
            | (Self::F2fsNoInline, StableInodeKind::VerityArtifact) => FS_XFLAG_VERITY,
            (Self::Ext4, StableInodeKind::CoverDirectory)
            | (Self::F2fsNoInline, StableInodeKind::CoverDirectory)
            | (Self::Xfs, StableInodeKind::CoverDirectory) => FS_XFLAG_PROJINHERIT,
            _ => 0,
        };
        COMMON_STABLE_FS_XFLAGS | filesystem_xflags | role_xflags
    }

    fn validate_payload(
        self,
        extsize: u32,
        nextents: u32,
        project_id: u32,
        cowextsize: u32,
    ) -> io::Result<()> {
        if extsize != 0 || cowextsize != 0 {
            return Err(invalid("stable inode has an unreviewed extent-size policy"));
        }
        match self {
            Self::Btrfs if nextents != 0 || project_id != 0 => {
                Err(invalid("Btrfs fileattr payload fields must be zero"))
            }
            Self::Ext4 | Self::F2fsNoInline if nextents != 0 => Err(invalid(
                "flags-based fileattr profile returned nonzero nextents",
            )),
            Self::Ext4 | Self::F2fsNoInline | Self::Xfs | Self::Btrfs => Ok(()),
        }
    }
}

/// Raw result of an inode-generation ioctl before the filesystem profile
/// requires support.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum InodeGenerationQuery {
    Value(u32),
    Unsupported(i32),
}

/// Validated snapshot of the reviewed generic file-attribute ioctl surfaces.
///
/// This does not prove xattr, ACL, file-capability, or LSM-label absence and
/// does not prove that another authority cannot mutate the inode. Those are
/// separate source-epoch broker obligations.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct StableInodePolicy {
    filesystem: StableFilesystemProfile,
    kind: StableInodeKind,
    flags: u32,
    xflags: u32,
    extsize: u32,
    nextents: u32,
    project_id: u32,
    cowextsize: u32,
    generation: u32,
}

impl StableInodePolicy {
    /// Read, validate, and retain the exact inode policy from a descriptor that
    /// supports ordinary ioctls. Every mandatory ioctl error rejects the source.
    pub(crate) fn read(
        descriptor: RawFd,
        filesystem_type: i64,
        kind: StableInodeKind,
    ) -> io::Result<Self> {
        let filesystem = StableFilesystemProfile::from_filesystem_type(filesystem_type, kind)?;
        let flags = mandatory_u32_ioctl(descriptor, FS_IOC_GETFLAGS, "FS_IOC_GETFLAGS")?;
        let fsx = read_fsxattr(descriptor)?;
        let generation = read_generation(descriptor, filesystem)?;
        let policy = Self {
            filesystem,
            kind,
            flags,
            xflags: fsx.xflags,
            extsize: fsx.extsize,
            nextents: fsx.nextents,
            project_id: fsx.project_id,
            cowextsize: fsx.cowextsize,
            generation,
        };
        policy.validate()?;
        Ok(policy)
    }

    /// Require the ioctl snapshot to agree with the exact reviewed statx
    /// capability profile and every corresponding reported value.
    ///
    /// Btrfs 7.1 reports the verity value without advertising the verity mask;
    /// that exact quirk is checked explicitly instead of being generalized to
    /// another filesystem or attribute.
    pub(crate) fn validate_statx(
        self,
        statx_attributes: u64,
        statx_attributes_mask: u64,
    ) -> io::Result<()> {
        let expected_mask = self.filesystem.statx_capability_mask();
        if statx_attributes_mask & STATX_INODE_POLICY_ATTRIBUTES != expected_mask {
            return Err(invalid(
                "stable inode statx capability mask differs from its filesystem profile",
            ));
        }
        let expected_unadvertised = if self.filesystem == StableFilesystemProfile::Btrfs
            && self.kind == StableInodeKind::VerityArtifact
        {
            STATX_ATTR_VERITY
        } else {
            0
        };
        if statx_attributes & STATX_INODE_POLICY_ATTRIBUTES & !statx_attributes_mask
            != expected_unadvertised
        {
            return Err(invalid(
                "stable inode has an unreviewed unadvertised statx attribute value",
            ));
        }
        for (statx, flag) in [
            (STATX_ATTR_COMPRESSED, FS_COMPR_FL),
            (STATX_ATTR_IMMUTABLE, FS_IMMUTABLE_FL),
            (STATX_ATTR_APPEND, FS_APPEND_FL),
            (STATX_ATTR_NODUMP, FS_NODUMP_FL),
            (STATX_ATTR_ENCRYPTED, FS_ENCRYPT_FL),
            (STATX_ATTR_VERITY, FS_VERITY_FL),
            (STATX_ATTR_DAX, FS_DAX_FL),
        ] {
            if statx_attributes_mask & statx != 0
                && (statx_attributes & statx != 0) != (self.flags & flag != 0)
            {
                return Err(invalid("statx and FS_IOC_GETFLAGS disagree"));
            }
        }
        Ok(())
    }

    /// Append the canonical little-endian snapshot to a containing digest.
    pub(crate) fn encode(self, output: &mut Vec<u8>) {
        output.push(self.filesystem.encoding_tag());
        output.extend_from_slice(&self.filesystem.filesystem_type().to_le_bytes());
        output.push(self.kind.encoding_tag());
        output.extend_from_slice(&self.flags.to_le_bytes());
        output.extend_from_slice(&self.xflags.to_le_bytes());
        output.extend_from_slice(&self.extsize.to_le_bytes());
        output.extend_from_slice(&self.nextents.to_le_bytes());
        output.extend_from_slice(&self.project_id.to_le_bytes());
        output.extend_from_slice(&self.cowextsize.to_le_bytes());
        output.extend_from_slice(&self.generation.to_le_bytes());
    }

    fn validate(self) -> io::Result<()> {
        self.filesystem.validate_kind(self.kind)?;
        if self.flags & !KNOWN_FS_FLAGS != 0 || self.xflags & !KNOWN_FS_XFLAGS != 0 {
            return Err(invalid(
                "stable inode returned unknown file-attribute flags",
            ));
        }
        let allowed_flags = self.filesystem.allowed_flags(self.kind);
        let allowed_xflags = self.filesystem.allowed_xflags(self.kind);
        if self.flags & !allowed_flags != 0 || self.xflags & !allowed_xflags != 0 {
            return Err(invalid(
                "stable inode has a forbidden file-attribute policy",
            ));
        }
        if self.flags & FS_IMMUTABLE_FL == 0 || self.xflags & FS_XFLAG_IMMUTABLE == 0 {
            return Err(invalid("stable inode is not immutable through both APIs"));
        }
        let verity_required = self.kind == StableInodeKind::VerityArtifact;
        if (self.flags & FS_VERITY_FL != 0) != verity_required
            || (self.xflags & FS_XFLAG_VERITY != 0) != verity_required
        {
            return Err(invalid("stable inode verity flags differ from its role"));
        }
        self.filesystem.validate_payload(
            self.extsize,
            self.nextents,
            self.project_id,
            self.cowextsize,
        )?;
        for (flag, xflag) in [
            (FS_SYNC_FL, FS_XFLAG_SYNC),
            (FS_IMMUTABLE_FL, FS_XFLAG_IMMUTABLE),
            (FS_APPEND_FL, FS_XFLAG_APPEND),
            (FS_NODUMP_FL, FS_XFLAG_NODUMP),
            (FS_NOATIME_FL, FS_XFLAG_NOATIME),
            (FS_DAX_FL, FS_XFLAG_DAX),
            (FS_PROJINHERIT_FL, FS_XFLAG_PROJINHERIT),
            (FS_VERITY_FL, FS_XFLAG_VERITY),
        ] {
            if (self.flags & flag != 0) != (self.xflags & xflag != 0) {
                return Err(invalid("FS_IOC_GETFLAGS and FS_IOC_FSGETXATTR disagree"));
            }
        }
        Ok(())
    }
}

fn mandatory_u32_ioctl(descriptor: RawFd, request: libc::c_ulong, label: &str) -> io::Result<u32> {
    let mut value = 0_u32;
    // SAFETY: Linux's generic helpers for these legacy requests write one u32,
    // even though the native request encoding retains historical sizeof(long).
    let result = unsafe { libc::ioctl(descriptor, request, &mut value) };
    if result != 0 {
        return Err(io::Error::other(format!(
            "{label} failed: {}",
            io::Error::last_os_error()
        )));
    }
    Ok(value)
}

fn read_fsxattr(descriptor: RawFd) -> io::Result<KernelFsxattr> {
    let mut value = KernelFsxattr {
        xflags: 0,
        extsize: 0,
        nextents: 0,
        project_id: 0,
        cowextsize: 0,
        padding: [0; 8],
    };
    // SAFETY: `value` has the exact 28-byte Linux UAPI layout and remains
    // writable for the duration of the ioctl.
    let result = unsafe { libc::ioctl(descriptor, FS_IOC_FSGETXATTR, &mut value) };
    if result != 0 {
        return Err(io::Error::other(format!(
            "FS_IOC_FSGETXATTR failed: {}",
            io::Error::last_os_error()
        )));
    }
    validate_fsxattr(value)
}

fn validate_fsxattr(value: KernelFsxattr) -> io::Result<KernelFsxattr> {
    if value.padding != [0; 8] {
        return Err(invalid("FS_IOC_FSGETXATTR returned nonzero padding"));
    }
    Ok(value)
}

fn read_generation(descriptor: RawFd, filesystem: StableFilesystemProfile) -> io::Result<u32> {
    let generic = optional_generation_ioctl(descriptor, FS_IOC_GETVERSION)?;
    if filesystem == StableFilesystemProfile::Ext4 {
        let old_ext4 = optional_generation_ioctl(descriptor, EXT4_IOC_GETVERSION)?;
        reconcile_ext4_generations(generic, old_ext4)
    } else {
        require_generation_value(generic)
    }
}

fn reconcile_ext4_generations(
    generic: InodeGenerationQuery,
    ext4: InodeGenerationQuery,
) -> io::Result<u32> {
    match (generic, ext4) {
        (InodeGenerationQuery::Value(left), InodeGenerationQuery::Value(right))
            if left == right =>
        {
            Ok(left)
        }
        _ => Err(invalid(
            "ext4 inode generation requests are unsupported or disagree",
        )),
    }
}

fn require_generation_value(result: InodeGenerationQuery) -> io::Result<u32> {
    match result {
        InodeGenerationQuery::Value(value) => Ok(value),
        InodeGenerationQuery::Unsupported(_) => Err(invalid(
            "stable filesystem does not support its required inode generation ioctl",
        )),
    }
}

fn optional_generation_ioctl(
    descriptor: RawFd,
    request: libc::c_ulong,
) -> io::Result<InodeGenerationQuery> {
    let mut value = 0_u32;
    // SAFETY: these legacy generation requests write one u32 to `value`.
    let result = unsafe { libc::ioctl(descriptor, request, &mut value) };
    if result == 0 {
        return classify_generation_result(Ok(value));
    }
    let errno = io::Error::last_os_error()
        .raw_os_error()
        .unwrap_or(libc::EIO);
    classify_generation_result(Err(errno))
}

fn classify_generation_result(result: Result<u32, i32>) -> io::Result<InodeGenerationQuery> {
    match result {
        Ok(value) => Ok(InodeGenerationQuery::Value(value)),
        Err(errno) if errno == libc::ENOTTY || errno == libc::EOPNOTSUPP => {
            Ok(InodeGenerationQuery::Unsupported(errno))
        }
        Err(errno) => Err(io::Error::from_raw_os_error(errno)),
    }
}

fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_for(filesystem: StableFilesystemProfile, kind: StableInodeKind) -> StableInodePolicy {
        let (role_flag, role_xflag) = if kind == StableInodeKind::VerityArtifact {
            (FS_VERITY_FL, FS_XFLAG_VERITY)
        } else {
            (0, 0)
        };
        StableInodePolicy {
            filesystem,
            kind,
            flags: FS_IMMUTABLE_FL | FS_NODUMP_FL | role_flag,
            xflags: FS_XFLAG_IMMUTABLE | FS_XFLAG_NODUMP | role_xflag,
            extsize: 0,
            nextents: 0,
            project_id: 0,
            cowextsize: 0,
            generation: 11,
        }
    }

    fn sample(kind: StableInodeKind) -> StableInodePolicy {
        sample_for(StableFilesystemProfile::Btrfs, kind)
    }

    #[test]
    fn ioctl_numbers_and_fsxattr_layout_are_exact() {
        assert_eq!(FS_IOC_GETFLAGS, 0x8008_6601);
        assert_eq!(FS_IOC_GETVERSION, 0x8008_7601);
        assert_eq!(EXT4_IOC_GETVERSION, 0x8008_6603);
        assert_eq!(FS_IOC_FSGETXATTR, 0x801c_581f);
        assert_eq!(std::mem::size_of::<KernelFsxattr>(), 28);
        assert_eq!(std::mem::align_of::<KernelFsxattr>(), 4);
    }

    #[test]
    fn role_policy_requires_immutable_and_exact_verity_state() {
        for kind in [
            StableInodeKind::VerityArtifact,
            StableInodeKind::CoverDirectory,
            StableInodeKind::CoverPlaceholder,
        ] {
            sample(kind).validate().unwrap();

            let mut missing_flag = sample(kind);
            missing_flag.flags &= !FS_IMMUTABLE_FL;
            assert!(missing_flag.validate().is_err());

            let mut missing_xflag = sample(kind);
            missing_xflag.xflags &= !FS_XFLAG_IMMUTABLE;
            assert!(missing_xflag.validate().is_err());
        }
        let mut artifact_as_cover = sample(StableInodeKind::VerityArtifact);
        artifact_as_cover.kind = StableInodeKind::CoverPlaceholder;
        assert!(artifact_as_cover.validate().is_err());
        let mut cover_as_artifact = sample(StableInodeKind::CoverPlaceholder);
        cover_as_artifact.kind = StableInodeKind::VerityArtifact;
        assert!(cover_as_artifact.validate().is_err());
    }

    #[test]
    fn unknown_forbidden_padding_and_extent_policies_fail_closed() {
        let kind = StableInodeKind::CoverPlaceholder;
        for flag in [FS_APPEND_FL, FS_ENCRYPT_FL, FS_DAX_FL, FS_CASEFOLD_FL] {
            let mut changed = sample(kind);
            changed.flags |= flag;
            assert!(changed.validate().is_err(), "accepted flag {flag:#x}");
        }
        for xflag in [
            FS_XFLAG_REALTIME,
            FS_XFLAG_APPEND,
            FS_XFLAG_DAX,
            FS_XFLAG_COWEXTSIZE,
        ] {
            let mut changed = sample(kind);
            changed.xflags |= xflag;
            assert!(changed.validate().is_err(), "accepted xflag {xflag:#x}");
        }
        let mut unknown = sample(kind);
        unknown.flags |= 0x0100_0000;
        assert!(unknown.validate().is_err());
        let mut unknown_xflag = sample(kind);
        unknown_xflag.xflags |= 0x0004_0000;
        assert!(unknown_xflag.validate().is_err());
        let mut extsize = sample(kind);
        extsize.extsize = 4096;
        assert!(extsize.validate().is_err());
        let mut cowextsize = sample(kind);
        cowextsize.cowextsize = 4096;
        assert!(cowextsize.validate().is_err());

        let mut fsx = KernelFsxattr {
            xflags: 0,
            extsize: 0,
            nextents: 0,
            project_id: 0,
            cowextsize: 0,
            padding: [0; 8],
        };
        validate_fsxattr(fsx).unwrap();
        fsx.padding[7] = 1;
        assert!(validate_fsxattr(fsx).is_err());
    }

    #[test]
    fn fileattr_flag_allowlists_are_filesystem_and_role_exact() {
        let mut ext4 = sample_for(
            StableFilesystemProfile::Ext4,
            StableInodeKind::CoverPlaceholder,
        );
        ext4.flags |= FS_EXTENT_FL;
        ext4.validate().unwrap();
        ext4.flags |= FS_NOCOW_FL;
        assert!(ext4.validate().is_err());
        let mut ext4_hasattr = sample_for(
            StableFilesystemProfile::Ext4,
            StableInodeKind::CoverPlaceholder,
        );
        ext4_hasattr.xflags |= FS_XFLAG_HASATTR;
        assert!(ext4_hasattr.validate().is_err());

        let mut btrfs = sample_for(
            StableFilesystemProfile::Btrfs,
            StableInodeKind::CoverPlaceholder,
        );
        btrfs.flags |= FS_NOCOW_FL;
        btrfs.validate().unwrap();
        btrfs.flags |= FS_EXTENT_FL;
        assert!(btrfs.validate().is_err());
        let mut btrfs_hasattr = sample_for(
            StableFilesystemProfile::Btrfs,
            StableInodeKind::CoverPlaceholder,
        );
        btrfs_hasattr.xflags |= FS_XFLAG_HASATTR;
        assert!(btrfs_hasattr.validate().is_err());

        let mut f2fs = sample_for(
            StableFilesystemProfile::F2fsNoInline,
            StableInodeKind::CoverPlaceholder,
        );
        f2fs.flags |= FS_NOCOW_FL;
        f2fs.validate().unwrap();
        let mut f2fs_artifact = sample_for(
            StableFilesystemProfile::F2fsNoInline,
            StableInodeKind::VerityArtifact,
        );
        f2fs_artifact.flags |= FS_NOCOW_FL;
        f2fs_artifact.validate().unwrap();
        f2fs.flags |= FS_EXTENT_FL;
        assert!(f2fs.validate().is_err());
        let mut f2fs_hasattr = sample_for(
            StableFilesystemProfile::F2fsNoInline,
            StableInodeKind::CoverPlaceholder,
        );
        f2fs_hasattr.xflags |= FS_XFLAG_HASATTR;
        assert!(f2fs_hasattr.validate().is_err());

        let mut xfs = sample_for(
            StableFilesystemProfile::Xfs,
            StableInodeKind::CoverPlaceholder,
        );
        xfs.xflags |= FS_XFLAG_HASATTR;
        xfs.validate().unwrap();
        xfs.flags |= FS_NOCOW_FL;
        assert!(xfs.validate().is_err());

        let mut ext4_directory = sample_for(
            StableFilesystemProfile::Ext4,
            StableInodeKind::CoverDirectory,
        );
        ext4_directory.flags |= FS_INDEX_FL | FS_DIRSYNC_FL | FS_TOPDIR_FL | FS_PROJINHERIT_FL;
        ext4_directory.xflags |= FS_XFLAG_PROJINHERIT;
        ext4_directory.validate().unwrap();

        let mut btrfs_directory = sample_for(
            StableFilesystemProfile::Btrfs,
            StableInodeKind::CoverDirectory,
        );
        btrfs_directory.flags |= FS_DIRSYNC_FL;
        btrfs_directory.validate().unwrap();
        btrfs_directory.flags |= FS_INDEX_FL;
        assert!(btrfs_directory.validate().is_err());

        let mut f2fs_directory = sample_for(
            StableFilesystemProfile::F2fsNoInline,
            StableInodeKind::CoverDirectory,
        );
        f2fs_directory.flags |= FS_INDEX_FL | FS_DIRSYNC_FL | FS_PROJINHERIT_FL;
        f2fs_directory.xflags |= FS_XFLAG_PROJINHERIT;
        f2fs_directory.validate().unwrap();
        f2fs_directory.flags |= FS_NOCOW_FL;
        assert!(f2fs_directory.validate().is_err());

        let mut xfs_directory = sample_for(
            StableFilesystemProfile::Xfs,
            StableInodeKind::CoverDirectory,
        );
        xfs_directory.flags |= FS_PROJINHERIT_FL;
        xfs_directory.xflags |= FS_XFLAG_PROJINHERIT;
        xfs_directory.validate().unwrap();
        xfs_directory.flags |= FS_DIRSYNC_FL;
        assert!(xfs_directory.validate().is_err());
    }

    #[test]
    fn fileattr_payload_shape_is_filesystem_exact() {
        for (filesystem, accepts_nextents, accepts_project_id) in [
            (StableFilesystemProfile::Ext4, false, true),
            (StableFilesystemProfile::Btrfs, false, false),
            (StableFilesystemProfile::F2fsNoInline, false, true),
            (StableFilesystemProfile::Xfs, true, true),
        ] {
            let baseline = sample_for(filesystem, StableInodeKind::CoverPlaceholder);
            baseline.validate().unwrap();

            let mut nextents = baseline;
            nextents.nextents = 1;
            assert_eq!(
                nextents.validate().is_ok(),
                accepts_nextents,
                "wrong nextents policy for {filesystem:?}",
            );

            let mut project_id = baseline;
            project_id.project_id = 1;
            assert_eq!(
                project_id.validate().is_ok(),
                accepts_project_id,
                "wrong project-id policy for {filesystem:?}",
            );

            let mut extsize = baseline;
            extsize.extsize = 1;
            assert!(extsize.validate().is_err());
            let mut cowextsize = baseline;
            cowextsize.cowextsize = 1;
            assert!(cowextsize.validate().is_err());
        }
    }

    #[test]
    fn every_shared_flag_mapping_must_agree() {
        let kind = StableInodeKind::VerityArtifact;
        for (flag, xflag) in [
            (FS_SYNC_FL, FS_XFLAG_SYNC),
            (FS_IMMUTABLE_FL, FS_XFLAG_IMMUTABLE),
            (FS_NODUMP_FL, FS_XFLAG_NODUMP),
            (FS_NOATIME_FL, FS_XFLAG_NOATIME),
            (FS_VERITY_FL, FS_XFLAG_VERITY),
        ] {
            let mut left = sample(kind);
            left.flags ^= flag;
            assert!(left.validate().is_err());
            let mut right = sample(kind);
            right.xflags ^= xflag;
            assert!(right.validate().is_err());
        }
    }

    #[test]
    fn filesystem_and_role_support_matrix_is_exact() {
        for (filesystem_type, profile) in [
            (EXT4_SUPER_MAGIC, StableFilesystemProfile::Ext4),
            (BTRFS_SUPER_MAGIC, StableFilesystemProfile::Btrfs),
            (F2FS_SUPER_MAGIC, StableFilesystemProfile::F2fsNoInline),
            (XFS_SUPER_MAGIC, StableFilesystemProfile::Xfs),
        ] {
            for kind in [
                StableInodeKind::CoverDirectory,
                StableInodeKind::CoverPlaceholder,
            ] {
                assert_eq!(
                    StableFilesystemProfile::from_filesystem_type(filesystem_type, kind).unwrap(),
                    profile
                );
            }
        }
        for filesystem_type in [EXT4_SUPER_MAGIC, BTRFS_SUPER_MAGIC, F2FS_SUPER_MAGIC] {
            StableFilesystemProfile::from_filesystem_type(
                filesystem_type,
                StableInodeKind::VerityArtifact,
            )
            .unwrap();
        }
        assert!(
            StableFilesystemProfile::from_filesystem_type(
                XFS_SUPER_MAGIC,
                StableInodeKind::VerityArtifact,
            )
            .is_err()
        );
        assert!(
            StableFilesystemProfile::from_filesystem_type(0, StableInodeKind::CoverDirectory,)
                .is_err()
        );
    }

    #[test]
    fn linux_7_1_statx_capability_profiles_are_exact() {
        let cases = [
            (
                StableFilesystemProfile::Ext4,
                StableInodeKind::VerityArtifact,
                STATX_ATTR_DAX
                    | STATX_ATTR_COMPRESSED
                    | STATX_ATTR_APPEND
                    | STATX_ATTR_ENCRYPTED
                    | STATX_ATTR_IMMUTABLE
                    | STATX_ATTR_NODUMP
                    | STATX_ATTR_VERITY,
            ),
            (
                StableFilesystemProfile::Btrfs,
                StableInodeKind::VerityArtifact,
                STATX_ATTR_DAX
                    | STATX_ATTR_COMPRESSED
                    | STATX_ATTR_APPEND
                    | STATX_ATTR_IMMUTABLE
                    | STATX_ATTR_NODUMP,
            ),
            (
                StableFilesystemProfile::F2fsNoInline,
                StableInodeKind::CoverDirectory,
                STATX_ATTR_DAX
                    | STATX_ATTR_COMPRESSED
                    | STATX_ATTR_APPEND
                    | STATX_ATTR_ENCRYPTED
                    | STATX_ATTR_IMMUTABLE
                    | STATX_ATTR_NODUMP
                    | STATX_ATTR_VERITY,
            ),
            (
                StableFilesystemProfile::Xfs,
                StableInodeKind::CoverPlaceholder,
                STATX_ATTR_DAX | STATX_ATTR_APPEND | STATX_ATTR_IMMUTABLE | STATX_ATTR_NODUMP,
            ),
        ];
        for (filesystem, kind, expected_mask) in cases {
            let policy = sample_for(filesystem, kind);
            let mut attributes = STATX_ATTR_IMMUTABLE | STATX_ATTR_NODUMP;
            if kind == StableInodeKind::VerityArtifact {
                attributes |= STATX_ATTR_VERITY;
            }
            assert_eq!(filesystem.statx_capability_mask(), expected_mask);
            policy.validate_statx(attributes, expected_mask).unwrap();

            for bit in [
                STATX_ATTR_COMPRESSED,
                STATX_ATTR_IMMUTABLE,
                STATX_ATTR_APPEND,
                STATX_ATTR_NODUMP,
                STATX_ATTR_ENCRYPTED,
                STATX_ATTR_VERITY,
                STATX_ATTR_DAX,
            ] {
                if expected_mask & bit != 0 {
                    assert!(
                        policy
                            .validate_statx(attributes, expected_mask & !bit)
                            .is_err(),
                        "accepted missing statx capability {bit:#x} for {filesystem:?}",
                    );
                } else {
                    assert!(
                        policy
                            .validate_statx(attributes, expected_mask | bit)
                            .is_err(),
                        "accepted extra statx capability {bit:#x} for {filesystem:?}",
                    );
                }
            }

            for bit in [
                STATX_ATTR_COMPRESSED,
                STATX_ATTR_IMMUTABLE,
                STATX_ATTR_APPEND,
                STATX_ATTR_NODUMP,
                STATX_ATTR_ENCRYPTED,
                STATX_ATTR_VERITY,
                STATX_ATTR_DAX,
            ] {
                if expected_mask & bit != 0 {
                    assert!(
                        policy
                            .validate_statx(attributes ^ bit, expected_mask)
                            .is_err(),
                        "accepted statx value mismatch {bit:#x} for {filesystem:?}",
                    );
                } else {
                    assert!(
                        policy
                            .validate_statx(attributes ^ bit, expected_mask)
                            .is_err(),
                        "accepted unadvertised statx value {bit:#x} for {filesystem:?}",
                    );
                }
            }
        }
    }

    #[test]
    fn btrfs_unadvertised_verity_value_is_still_role_exact() {
        let mask = StableFilesystemProfile::Btrfs.statx_capability_mask();
        let artifact = sample_for(
            StableFilesystemProfile::Btrfs,
            StableInodeKind::VerityArtifact,
        );
        let cover = sample_for(
            StableFilesystemProfile::Btrfs,
            StableInodeKind::CoverPlaceholder,
        );
        let base = STATX_ATTR_IMMUTABLE | STATX_ATTR_NODUMP;
        artifact
            .validate_statx(base | STATX_ATTR_VERITY, mask)
            .unwrap();
        assert!(artifact.validate_statx(base, mask).is_err());
        cover.validate_statx(base, mask).unwrap();
        assert!(
            cover
                .validate_statx(base | STATX_ATTR_VERITY, mask)
                .is_err()
        );
    }

    #[test]
    fn f2fs_profile_requires_non_inline_staging_for_every_role() {
        for kind in [
            StableInodeKind::VerityArtifact,
            StableInodeKind::CoverDirectory,
            StableInodeKind::CoverPlaceholder,
        ] {
            let mut inode = sample_for(StableFilesystemProfile::F2fsNoInline, kind);
            inode.flags |= FS_INLINE_DATA_FL;
            assert!(inode.validate().is_err());
        }
        for filesystem in [
            StableFilesystemProfile::Ext4,
            StableFilesystemProfile::Btrfs,
            StableFilesystemProfile::Xfs,
        ] {
            let mut cover = sample_for(filesystem, StableInodeKind::CoverDirectory);
            cover.flags |= FS_INLINE_DATA_FL;
            assert!(cover.validate().is_err());
        }
    }

    #[test]
    fn canonical_encoding_has_a_fixed_byte_and_length_golden() {
        let baseline = sample(StableInodeKind::VerityArtifact);
        let encoded = |value: StableInodePolicy| {
            let mut bytes = Vec::new();
            value.encode(&mut bytes);
            bytes
        };
        let expected = encoded(baseline);
        assert_eq!(expected.len(), 38);
        assert_eq!(
            expected,
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
        );

        let mutators: &[fn(&mut StableInodePolicy)] = &[
            |p| p.filesystem = StableFilesystemProfile::Ext4,
            |p| p.kind = StableInodeKind::CoverDirectory,
            |p| p.flags ^= FS_NODUMP_FL,
            |p| p.xflags ^= FS_XFLAG_NODUMP,
            |p| p.extsize += 1,
            |p| p.nextents += 1,
            |p| p.project_id += 1,
            |p| p.cowextsize += 1,
            |p| p.generation = 12,
        ];
        for (index, mutate) in mutators.iter().enumerate() {
            let mut changed = baseline;
            mutate(&mut changed);
            assert_ne!(
                encoded(changed),
                expected,
                "field mutator {index} was not encoded"
            );
        }
    }

    #[test]
    fn raw_generation_classification_is_exact_but_profiles_require_a_value() {
        assert_eq!(
            classify_generation_result(Ok(42)).unwrap(),
            InodeGenerationQuery::Value(42)
        );
        for errno in [libc::ENOTTY, libc::EOPNOTSUPP] {
            let classified = classify_generation_result(Err(errno)).unwrap();
            assert_eq!(classified, InodeGenerationQuery::Unsupported(errno));
            assert!(require_generation_value(classified).is_err());
        }
        for errno in [libc::EINVAL, libc::EPERM, libc::EIO] {
            assert_eq!(
                classify_generation_result(Err(errno))
                    .unwrap_err()
                    .raw_os_error(),
                Some(errno)
            );
        }
        assert_eq!(
            reconcile_ext4_generations(
                InodeGenerationQuery::Value(7),
                InodeGenerationQuery::Value(7)
            )
            .unwrap(),
            7
        );
        assert!(
            reconcile_ext4_generations(
                InodeGenerationQuery::Value(7),
                InodeGenerationQuery::Value(8)
            )
            .is_err()
        );
        assert!(
            reconcile_ext4_generations(
                InodeGenerationQuery::Value(7),
                InodeGenerationQuery::Unsupported(libc::ENOTTY)
            )
            .is_err()
        );
        assert_eq!(
            require_generation_value(InodeGenerationQuery::Value(9)).unwrap(),
            9
        );
    }
}
