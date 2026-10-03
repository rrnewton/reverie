/* Copyright (c) Meta Platforms, Inc. and affiliates. All rights reserved.
 * Licensed under the BSD-style license in the root LICENSE file. */

//! Narrow, unsafe evidence boundary for original terminal-query dispatch.
use std::sync::Arc;

use crate::Tid;
use crate::syscalls::SyscallArgs;

/// The closed kernel dispatches currently supported for native terminal queries.
/// Neither variant identifies a file or authenticates a kernel observation.
#[derive(Clone, Copy, Debug)]
pub enum OriginalIoctlDispatch {
    /// The actual selected file uses the pinned immutable `null_fops`, whose
    /// `unlocked_ioctl` is null. A null handler on an unknown table is not enough.
    NullFileOperations,
    /// A regular Btrfs inode whose actual file uses the exact pinned immutable
    /// `btrfs_file_operations`, on the audited kernel image.
    BtrfsRegularFileOperations,
}

/// One backend-owned, original native x86-64 ioctl ENTRY. Not cloneable: a
/// later equal tuple is a different attempt and receives a different identity.
pub struct OriginalIoctlEntry {
    identity: Arc<()>,
    tid: Tid,
    args: SyscallArgs,
}

/// Dispatch evidence bound to one borrowed original ENTRY. Safe Tool code
/// cannot construct this value or transfer it to another attempt.
pub struct OriginalIoctlEffect {
    identity: Arc<()>,
}

impl OriginalIoctlEntry {
    /// Construct the view at an actual original syscall ENTRY.
    ///
    /// # Safety
    /// The backend must retain the stopped task; verify native x86-64 ioctl,
    /// the complete effective arguments, and original (not injected) entry;
    /// and consume this view only for that exact attempt and task generation.
    pub unsafe fn from_original_backend_entry(tid: Tid, args: SyscallArgs) -> Self {
        Self {
            identity: Arc::new(()),
            tid,
            args,
        }
    }

    /// The backend-owned original task.
    pub fn tid(&self) -> Tid {
        self.tid
    }

    /// All six actual original operands, before any Tool rewrite.
    pub fn args(&self) -> SyscallArgs {
        self.args
    }

    /// Certify one of the closed dispatches for this exact selected OFD.
    ///
    /// # Safety
    /// The caller must join an authenticated kernel observation of the ACTUAL
    /// selected file to this task generation, current FD binding and original
    /// attempt. A pathname, stat/profile, command number, final errno, or caller
    /// assertion is not evidence. The observed dispatcher must remain immutable
    /// until this original operation returns. The kernel image must implement
    /// TCGETS/TIOCGWINSZ through the audited VFS fallback and security contract:
    /// neither the security hook nor the selected dispatcher may retain a user
    /// memory writer. The actual f_op pointer must identify the supported immutable table on
    /// the pinned kernel image, not merely an absent handler observed earlier.
    /// Btrfs additionally requires a regular inode and Btrfs superblock. Unknown kernels, reused descriptors and foreign OFDs
    /// must not reach this method. This certifies no completion or source read.
    pub unsafe fn certify_dispatch(
        &self,
        _dispatch: OriginalIoctlDispatch,
    ) -> Option<OriginalIoctlEffect> {
        matches!(self.args.arg1 as u32, 0x5401 | 0x5413).then(|| OriginalIoctlEffect {
            identity: Arc::clone(&self.identity),
        })
    }

    /// Check that the proof was issued for this exact view, not an equal tuple.
    pub fn accepts(&self, effect: &OriginalIoctlEffect) -> bool {
        Arc::ptr_eq(&self.identity, &effect.identity)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn safe_consumption_rejects_foreign_task_fd_command_and_equal_later_attempt() {
        let args = SyscallArgs::new(1, 0x5401, 0x1234, 0, 0, 0);
        // Component fixture only: no native dispatcher claim is made.
        let original =
            unsafe { OriginalIoctlEntry::from_original_backend_entry(Tid::from_raw(7), args) };
        let proof = unsafe { original.certify_dispatch(OriginalIoctlDispatch::NullFileOperations) }
            .unwrap();
        assert!(original.accepts(&proof));
        for (tid, fd, cmd) in [
            (8, 1, 0x5401),
            (7, 2, 0x5401),
            (7, 1, 0x5413),
            (7, 1, 0x5401),
        ] {
            let other = unsafe {
                OriginalIoctlEntry::from_original_backend_entry(
                    Tid::from_raw(tid),
                    SyscallArgs::new(fd, cmd, 0x1234, 0, 0, 0),
                )
            };
            assert!(!other.accepts(&proof));
        }
        let unsupported = unsafe {
            OriginalIoctlEntry::from_original_backend_entry(
                Tid::from_raw(7),
                SyscallArgs::new(1, 0x5402, 0x1234, 0, 0, 0),
            )
        };
        assert!(
            unsafe { unsupported.certify_dispatch(OriginalIoctlDispatch::NullFileOperations) }
                .is_none()
        );
    }
}
