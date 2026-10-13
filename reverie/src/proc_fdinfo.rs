/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! Owned evidence from a backend's current ordinary fdinfo description.
//!
//! This capability does not describe a physical proc namespace or a foreign
//! task. The Tool must independently admit the callback, current file table,
//! source and target open descriptions, and their metadata revision.

use reverie_syscalls::Errno;

use crate::Error;
use crate::SignalTaskIdentity;

/// The current callback and the task selected by the opened description.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CurrentFdinfoSourceBinding {
    /// The live, backend-registered callback task.
    pub reader: SignalTaskIdentity,
    /// The live task retained by the opened fdinfo description.
    pub subject: SignalTaskIdentity,
    /// The descriptor selected in that subject's original table.
    pub target_fd: i32,
}

/// Raw ordinary object identity from the exact pin used for a fresh render.
///
/// An inode is an object observation, not an open-description identity.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct OrdinaryFdinfoObject {
    /// Raw device major number.
    pub device_major: u32,
    /// Raw device minor number.
    pub device_minor: u32,
    /// Raw filesystem inode.
    pub inode: u64,
    /// Raw `S_IFMT` file type.
    pub file_type: u32,
    /// Raw mount ID of the pinned object.
    pub mount_id: u64,
}

/// Optional object evidence belonging to the same fresh render as the bytes.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CurrentFdinfoObjectReceipt {
    /// The still-current source binding.
    pub binding: CurrentFdinfoSourceBinding,
    /// The exact pinned ordinary object that produced the bytes.
    pub object: OrdinaryFdinfoObject,
    /// `F_GETFL` observed in the original target-table observation.
    pub status_flags: i32,
    /// The target slot's guest CLOEXEC, not a private pin's CLOEXEC.
    pub target_cloexec: bool,
    /// The original represented ordinary mount-table order.
    ///
    /// The Tool must join this to its producer-captured Config provenance.
    /// An object's mount, such as pipefs, need not be listed in mountinfo.
    pub mount_identity_order: Vec<u64>,
}

/// A private acquisition cannot supply positive current-object authority.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CurrentFdinfoProofGap {
    /// The source slot or opened description changed.
    SourceChanged,
    /// The selected target slot was replaced after the nonrendering probe.
    TargetChanged,
    /// The registered callback process or image changed.
    ActorChanged,
    /// The description selected a different authoritative file table.
    DifferentTable,
    /// The selected task generation is no longer live.
    RetiredSubject,
    /// The target is private, borrowed stdio, or otherwise unsupported.
    UnsupportedObject,
    /// Raw pinned-object metadata did not authenticate the rendered fields.
    ObjectObservationUnavailable,
    /// The original represented mount snapshot could not be parsed faithfully.
    UnsupportedMountView,
    /// A private acquisition's actual Close did not report success.
    CleanupUnavailable,
    /// The private drain was not a complete fresh snapshot from byte zero.
    IncompleteOrStaleSnapshot,
}

/// Snapshot and actual private cleanup facts, separate from raw read results.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CurrentFdinfoSnapshotFacts {
    /// The private snapshot's requested origin; positive snapshots use zero.
    pub origin: u64,
    /// The drain invoked exactly one fresh target render.
    pub fresh_render: bool,
    /// The complete fresh record was retained within the acquisition bound.
    pub complete: bool,
    /// The counted drain reached EOF from that same render.
    pub reached_eof: bool,
    /// The original backend sequence, including its buffered cursor, survived.
    pub sequence_restored: bool,
    /// The actual Close result of the held source pin.
    pub source_cleanup: Result<(), Errno>,
    /// The actual Close result if a target pin was acquired.
    pub target_cleanup: Option<Result<(), Errno>>,
    /// The actual Close result if a host proc-fdinfo reader was acquired.
    pub read_cleanup: Option<Result<(), Errno>>,
}

impl CurrentFdinfoSnapshotFacts {
    /// Whether the snapshot is complete; cleanup is a separate observation.
    pub fn is_complete(&self) -> bool {
        self.origin == 0
            && self.fresh_render
            && self.complete
            && self.reached_eof
            && self.sequence_restored
    }

    /// Whether every acquired private descriptor reported a successful Close.
    pub fn cleanup_succeeded(&self) -> bool {
        self.source_cleanup.is_ok()
            && self.target_cleanup.is_none_or(|result| result.is_ok())
            && self.read_cleanup.is_none_or(|result| result.is_ok())
    }
}

/// An ordinary retained description's result, with proof and cleanup separate.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CurrentFdinfoRead {
    /// A complete fresh snapshot. A proof gap does not erase successful bytes.
    Success {
        /// The whole fresh record, before the Tool's shared normalization.
        bytes: Vec<u8>,
        /// Evidence from the exact target pin that rendered `bytes`.
        receipt: Result<CurrentFdinfoObjectReceipt, CurrentFdinfoProofGap>,
        /// Owned origin, completeness, EOF and observed cleanup facts.
        facts: CurrentFdinfoSnapshotFacts,
    },
    /// No whole snapshot may be installed; ordinary raw fallback remains live.
    SnapshotUnavailable {
        /// Why the private capture cannot be used.
        gap: CurrentFdinfoProofGap,
        /// Facts retained after restoring the sequence and settling ownership.
        facts: CurrentFdinfoSnapshotFacts,
    },
    /// The retained description's actual raw read/render error.
    ///
    /// Private proof or cleanup errors never enter this variant.
    ReadErrno {
        /// The actual raw operation's errno.
        errno: Errno,
        /// Separate snapshot and actual cleanup facts.
        facts: CurrentFdinfoSnapshotFacts,
    },
}

/// A source lease owned by one currently registered callback and file table.
///
/// A probe acquires ownership without rendering or changing the sequence.
/// Implementations retain the original source entry/description and real OFD
/// pins; numbers, pointers serialized on a wire, or equal inodes cannot mint
/// this lease. The Tool must finish its own actor/table/revision join.
pub trait CurrentFdinfoSource: Send {
    /// The live callback and subject selected by the retained description.
    fn binding(&self) -> &CurrentFdinfoSourceBinding;

    /// Capture one fresh complete record from byte zero and settle ownership.
    ///
    /// The exact same target pin supplies both rendering and object evidence.
    /// Restore the complete original backend sequence on every path, so a
    /// declined installation cannot leave raw fallback at private EOF. The
    /// guest's logical read/pread cursor is unchanged. No backend pin remains
    /// when this returns; fatal failures use [`Error`], not a guest errno.
    fn capture(self: Box<Self>) -> Result<CurrentFdinfoRead, Error>;

    /// Settle a declined probe without rendering, resetting, or reading it.
    ///
    /// Returns the observed source Close result. Silent destruction is only
    /// abort safety and cannot be recorded as a successful cleanup observation.
    fn discard(self: Box<Self>) -> Result<CurrentFdinfoSnapshotFacts, Error>;
}
