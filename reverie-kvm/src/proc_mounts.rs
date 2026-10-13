/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

use std::io::Read;
use std::io::{self};
use std::os::fd::AsRawFd;

/// The namespace used by the backend's ordinary host filesystem operations.
/// Namespace-changing guest syscalls are unsupported, so fork and exec retain
/// this snapshot. The Tool remains responsible for deterministic normalization.
#[derive(Debug)]
pub(crate) struct ProcMountSnapshot {
    pub(crate) mountinfo: Vec<u8>,
    pub(crate) mounts: Vec<u8>,
    pub(crate) proc_fs: ProcFilesystemMetadata,
}

impl ProcMountSnapshot {
    pub(crate) fn capture() -> io::Result<Self> {
        Ok(Self {
            mountinfo: read_snapshot("/proc/self/mountinfo")?,
            mounts: read_snapshot("/proc/self/mounts")?,
            proc_fs: ProcFilesystemMetadata::capture()?,
        })
    }
}

/// Metadata of the actual proc mount used by the existing backend filesystem
/// view. This proves mount metadata only; ProcessBinding supplies task authority.
#[derive(Debug)]
pub(crate) struct ProcFilesystemMetadata {
    pub(crate) mount_id: u64,
    kind: libc::c_long,
    block_size: libc::c_long,
    blocks: libc::fsblkcnt_t,
    free_blocks: libc::fsblkcnt_t,
    available_blocks: libc::fsblkcnt_t,
    files: libc::fsfilcnt_t,
    free_files: libc::fsfilcnt_t,
    name_length: libc::c_long,
    fragment_size: libc::c_long,
    flags: libc::c_long,
}

impl ProcFilesystemMetadata {
    fn capture() -> io::Result<Self> {
        let root = std::fs::File::open("/proc")?;
        let mut stx = std::mem::MaybeUninit::<libc::statx>::zeroed();
        let mut stat = std::mem::MaybeUninit::<libc::statfs>::zeroed();
        // SAFETY: root and both writable output objects are live for the calls.
        if unsafe { libc::statx(root.as_raw_fd(), c"".as_ptr(), libc::AT_EMPTY_PATH,
            libc::STATX_TYPE | libc::STATX_MNT_ID, stx.as_mut_ptr()) } != 0
            || unsafe { libc::fstatfs(root.as_raw_fd(), stat.as_mut_ptr()) } != 0
        { return Err(io::Error::last_os_error()); }
        // SAFETY: both successful calls initialized their respective objects.
        let stx = unsafe { stx.assume_init() };
        let stat = unsafe { stat.assume_init() };
        let required = libc::STATX_TYPE | libc::STATX_MNT_ID;
        if stx.stx_mask & required != required || stx.stx_mode as libc::mode_t & libc::S_IFMT != libc::S_IFDIR
            || stat.f_type != 0x9fa0 // include/uapi/linux/magic.h: PROC_SUPER_MAGIC
        { return Err(io::Error::from_raw_os_error(libc::EOPNOTSUPP)); }
        Ok(Self {
            mount_id: stx.stx_mnt_id, kind: stat.f_type, block_size: stat.f_bsize,
            blocks: stat.f_blocks, free_blocks: stat.f_bfree, available_blocks: stat.f_bavail,
            files: stat.f_files, free_files: stat.f_ffree, name_length: stat.f_namelen,
            fragment_size: stat.f_frsize, flags: stat.f_flags,
        })
    }

    pub(crate) fn statfs(&self) -> libc::statfs {
        // SAFETY: statfs is plain data. Padding/spare fields and fsid are zero;
        // the existing shared/lower statfs normalizer supplies its same policy.
        let mut stat: libc::statfs = unsafe { std::mem::zeroed() };
        stat.f_type = self.kind; stat.f_bsize = self.block_size;
        stat.f_blocks = self.blocks; stat.f_bfree = self.free_blocks; stat.f_bavail = self.available_blocks;
        stat.f_files = self.files; stat.f_ffree = self.free_files; stat.f_namelen = self.name_length;
        stat.f_frsize = self.fragment_size; stat.f_flags = self.flags;
        stat
    }
}

fn read_snapshot(path: &str) -> io::Result<Vec<u8>> {
    // A backend capacity refusal must not become a truncated mount table.
    const MAX_SNAPSHOT_BYTES: u64 = 16 * 1024 * 1024;
    read_snapshot_bytes(std::fs::File::open(path)?, MAX_SNAPSHOT_BYTES, path)
}

fn read_snapshot_bytes(source: impl Read, limit: u64, path: &str) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    source.take(limit + 1).read_to_end(&mut bytes)?;
    if bytes.is_empty() || bytes.len() as u64 > limit {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{path} is empty or exceeds the backend mount snapshot capacity"),
        ));
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mount_snapshot_retains_exact_bytes_and_refuses_empty_or_oversized_input() {
        let input = b"10 9 0:1 / / rw - tmpfs tmpfs rw\n";
        assert_eq!(
            read_snapshot_bytes(input.as_slice(), input.len() as u64, "fixture").unwrap(),
            input
        );
        for (input, limit) in [(b"".as_slice(), 32), (b"12345".as_slice(), 4)] {
            assert_eq!(
                read_snapshot_bytes(input, limit, "fixture")
                    .unwrap_err()
                    .kind(),
                io::ErrorKind::InvalidData
            );
        }
    }

    #[test]
    fn mount_snapshot_propagates_read_error_without_partial_success() {
        struct Broken;
        impl Read for Broken {
            fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
                Err(io::Error::from_raw_os_error(libc::EACCES))
            }
        }
        assert_eq!(
            read_snapshot_bytes(Broken, 32, "fixture")
                .unwrap_err()
                .raw_os_error(),
            Some(libc::EACCES)
        );
    }
}
