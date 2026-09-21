/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! Authenticated, immutable carriers for fixed synthetic proc files.
//!
//! A carrier is a sealed shmem inode whose semantic identity is authenticated
//! by a top-level guest's private key.  The marker is deliberately a bearer
//! capability rather than an anti-replay token: aliases inherited by fork or
//! passed through `SCM_RIGHTS` remain valid under the same authority.

use std::collections::BTreeMap;
use std::ffi::CString;
use std::fmt;
use std::fs::File;
use std::io;
use std::io::Write;
use std::os::fd::AsRawFd;
use std::os::fd::FromRawFd;
use std::os::unix::fs::FileExt;
use std::sync::Arc;
use std::sync::OnceLock;

use hmac::Hmac;
use hmac::Mac;
use sha2::Sha256;

type HmacSha256 = Hmac<Sha256>;

pub(crate) const RESERVED_NAME_PREFIX: &[u8] = b"reverie-kvm.proc-carrier.v1";
pub(crate) const MAX_HANDLE_SZ: usize = 128;
pub(crate) const MAX_AUTHENTICATED_BYTES: usize = 16 * 1024 * 1024;

const HANDLE_ATTEMPTS: usize = 4;
const HANDLE_SIZE_GRANULE: usize = 4;
const STATX_MNT_ID_UNIQUE: libc::c_uint = 0x4000;
const AT_HANDLE_MNT_ID_UNIQUE: libc::c_int = 0x001;
const TMPFS_MAGIC: libc::c_long = 0x0102_1994;
const F_SEAL_FUTURE_WRITE: libc::c_int = 0x0010;
const F_SEAL_EXEC: libc::c_int = 0x0020;
const REQUIRED_SEALS: libc::c_int =
    libc::F_SEAL_SEAL | libc::F_SEAL_SHRINK | libc::F_SEAL_GROW | libc::F_SEAL_WRITE;
const ACCEPTED_SEALS_WITH_EXEC: libc::c_int = REQUIRED_SEALS | F_SEAL_EXEC;
const UTIME_OMIT_RAW: libc::c_long = 1_073_741_822;
const TAG_MODULUS: u128 = 9_223_372_036_854_775_807_000_000_000;
const TAG_REJECTION_FLOOR: u128 = 3_865_545_067_477_747_802_768_211_456;
const LINK_TARGET_CAPACITY: usize = 267;
const MEMFD_LINK_PREFIX: &[u8] = b"/memfd:";
const MEMFD_LINK_SUFFIX: &[u8] = b" (deleted)";
const TRANSCRIPT_DOMAIN: &[u8] = b"reverie-kvm.proc-carrier";
const TRANSCRIPT_VERSION: u32 = 1;
const KIND_PROC: &[u8] = b"proc";
const PRESERVED_STATUS_FLAGS: libc::c_int = libc::O_APPEND
    | libc::O_ASYNC
    | libc::O_DSYNC
    | libc::O_NOATIME
    | libc::O_NONBLOCK
    | libc::O_SYNC;

const REQUIRED_STATX_MASK: libc::c_uint = libc::STATX_TYPE
    | libc::STATX_MODE
    | libc::STATX_NLINK
    | libc::STATX_UID
    | libc::STATX_GID
    | libc::STATX_MTIME
    | libc::STATX_INO
    | libc::STATX_SIZE
    | libc::STATX_MNT_ID;

const FIELD_AUTHORITY: u16 = 1;
const FIELD_RESERVED_NAME: u16 = 2;
const FIELD_KIND: u16 = 3;
const FIELD_CANONICAL_PATH: u16 = 4;
const FIELD_VIRTUAL_FLAGS: u16 = 5;
const FIELD_MOUNT_MODE: u16 = 6;
const FIELD_LEGACY_HANDLE_MOUNT_ID: u16 = 7;
const FIELD_STATX_MOUNT_ID: u16 = 8;
const FIELD_STATX_UNIQUE_MOUNT_ID: u16 = 9;
const FIELD_UNIQUE_HANDLE_MOUNT_ID: u16 = 10;
const FIELD_HANDLE_LENGTH: u16 = 11;
const FIELD_HANDLE_TYPE: u16 = 12;
const FIELD_HANDLE_BYTES: u16 = 13;
const FIELD_FILESYSTEM_TYPE: u16 = 14;
const FIELD_DEVICE_MAJOR: u16 = 15;
const FIELD_DEVICE_MINOR: u16 = 16;
const FIELD_INODE: u16 = 17;
const FIELD_MODE: u16 = 18;
const FIELD_NLINK: u16 = 19;
const FIELD_UID: u16 = 20;
const FIELD_GID: u16 = 21;
const FIELD_RDEV_MAJOR: u16 = 22;
const FIELD_RDEV_MINOR: u16 = 23;
const FIELD_BLOCK_SIZE: u16 = 24;
const FIELD_ATTRIBUTES: u16 = 25;
const FIELD_SEALS: u16 = 26;
const FIELD_SIZE: u16 = 27;
const FIELD_CONTENT: u16 = 28;
const FIELD_SAMPLE_COUNTER: u16 = u16::MAX;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum MountIdMode {
    Legacy,
    StatxUnique,
    FullUnique,
}

impl MountIdMode {
    fn wire_value(self) -> u8 {
        match self {
            Self::Legacy => 0,
            Self::StatxUnique => 1,
            Self::FullUnique => 2,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct OpaqueHandle {
    kind: libc::c_int,
    bytes: Vec<u8>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct HandleIdentity {
    opaque: OpaqueHandle,
    legacy_mount_id: libc::c_int,
    unique_mount_id: Option<u64>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct StableStat {
    filesystem_type: i64,
    device_major: u32,
    device_minor: u32,
    inode: u64,
    mode: u16,
    nlink: u32,
    uid: u32,
    gid: u32,
    rdev_major: u32,
    rdev_minor: u32,
    block_size: u32,
    attributes: u64,
    size: u64,
    statx_mount_id: u64,
    statx_unique_mount_id: Option<u64>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct CarrierSnapshot {
    name: Vec<u8>,
    parsed: ParsedCarrierName,
    handle: HandleIdentity,
    stat: StableStat,
    seals: libc::c_int,
    tag: u128,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ParsedCarrierName {
    authority_id: [u8; 16],
    canonical_path: Vec<u8>,
    virtual_nofollow: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
struct CarrierCacheKey {
    handle_type: libc::c_int,
    handle_bytes: Vec<u8>,
    legacy_mount_id: libc::c_int,
    statx_mount_id: u64,
    statx_unique_mount_id: Option<u64>,
    unique_handle_mount_id: Option<u64>,
}

impl From<&CarrierSnapshot> for CarrierCacheKey {
    fn from(snapshot: &CarrierSnapshot) -> Self {
        Self {
            handle_type: snapshot.handle.opaque.kind,
            handle_bytes: snapshot.handle.opaque.bytes.clone(),
            legacy_mount_id: snapshot.handle.legacy_mount_id,
            statx_mount_id: snapshot.stat.statx_mount_id,
            statx_unique_mount_id: snapshot.stat.statx_unique_mount_id,
            unique_handle_mount_id: snapshot.handle.unique_mount_id,
        }
    }
}

#[derive(Clone)]
struct CachedAuthentication {
    snapshot: CarrierSnapshot,
    expected_tag: u128,
    result: AuthenticatedProcCarrier,
}

/// Per-syscall bound for content read and hashed while authenticating rights.
#[derive(Debug)]
pub(crate) struct CarrierAuthBudget {
    remaining: usize,
}

impl CarrierAuthBudget {
    pub(crate) fn new() -> Self {
        Self {
            remaining: MAX_AUTHENTICATED_BYTES,
        }
    }

    fn charge(&mut self, bytes: usize) -> Result<(), libc::c_int> {
        self.remaining = self.remaining.checked_sub(bytes).ok_or(libc::EFBIG)?;
        Ok(())
    }

    #[cfg(test)]
    fn with_limit(limit: usize) -> Self {
        Self { remaining: limit }
    }
}

impl Default for CarrierAuthBudget {
    fn default() -> Self {
        Self::new()
    }
}

/// Per-message authentication cache. It is intentionally separate from the
/// syscall-wide byte budget so `recvmmsg` can clear only this cache.
#[derive(Default)]
pub(crate) struct CarrierAuthCache {
    entries: BTreeMap<CarrierCacheKey, CachedAuthentication>,
}

impl CarrierAuthCache {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn clear(&mut self) {
        self.entries.clear();
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ProcCarrierCandidate {
    Ordinary,
    Reserved,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct AuthenticatedProcCarrier {
    pub(crate) canonical_path: Vec<u8>,
    pub(crate) virtual_nofollow: bool,
    pub(crate) content: Arc<[u8]>,
}

#[derive(Clone, Debug)]
struct ProbeProfile {
    mode: MountIdMode,
    filesystem_type: i64,
    device_major: u32,
    device_minor: u32,
    legacy_mount_id: libc::c_int,
    statx_mount_id: u64,
    statx_unique_mount_id: Option<u64>,
    unique_handle_mount_id: Option<u64>,
    seals: libc::c_int,
}

#[derive(Clone, Debug)]
struct ProbeFailure {
    phase: &'static str,
    reason: String,
}

impl ProbeFailure {
    fn message(phase: &'static str, reason: impl Into<String>) -> Self {
        Self {
            phase,
            reason: reason.into(),
        }
    }

    fn io(phase: &'static str, error: io::Error) -> Self {
        Self::message(phase, error.to_string())
    }

    fn into_public(self) -> crate::Error {
        crate::Error::ProcCarrierUnsupported {
            phase: self.phase,
            reason: self.reason,
        }
    }
}

/// Per-top-level-load authentication authority. The key is intentionally not
/// serializable and its Debug implementation is explicitly redacted.
pub(crate) struct ProcCarrierAuthority {
    key: [u8; 32],
    public_id: [u8; 16],
    profile: ProbeProfile,
}

impl fmt::Debug for ProcCarrierAuthority {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProcCarrierAuthority")
            .field("key", &"<redacted>")
            .field("public_id", &Hex(&self.public_id))
            .field("mount_mode", &self.profile.mode)
            .field("seals", &format_args!("{:#x}", self.profile.seals))
            .finish()
    }
}

impl ProcCarrierAuthority {
    pub(crate) fn new() -> crate::Result<Arc<Self>> {
        let (key, public_id) = random_authority_material()
            .map_err(|error| ProbeFailure::io("getrandom", error).into_public())?;
        let profile = qualified_profile().map_err(ProbeFailure::into_public)?;
        Ok(Arc::new(Self {
            key,
            public_id,
            profile,
        }))
    }

    /// Native executor fixtures construct hundreds of independent states. The
    /// process-wide host profile is already cached; every state still gets
    /// fresh secret and public identity bytes.
    #[cfg(any(test, feature = "native-test-support"))]
    pub(crate) fn new_for_tests() -> crate::Result<Arc<Self>> {
        Self::new()
    }

    #[cfg(any(test, feature = "native-test-support"))]
    pub(crate) fn public_id(&self) -> [u8; 16] {
        self.public_id
    }

    pub(crate) fn selected_seals(&self) -> libc::c_int {
        self.profile.seals
    }

    pub(crate) fn reserved_guest_name(name: &[u8]) -> bool {
        reserved_guest_name(name)
    }

    pub(crate) fn candidate_kind(&self, file: &File) -> Result<ProcCarrierCandidate, libc::c_int> {
        candidate_kind(file)
    }

    pub(crate) fn open_inspection_alias(&self, source: &File) -> Result<File, libc::c_int> {
        open_inspection_alias(source)
    }

    /// Create a fresh physical carrier and authenticate the exact object before
    /// returning it to the caller for guest-table publication.
    pub(crate) fn mint(
        &self,
        canonical_path: &[u8],
        content: &[u8],
        virtual_nofollow: bool,
        path_only: bool,
        guest_status_flags: libc::c_int,
    ) -> Result<File, libc::c_int> {
        if content.len() > MAX_AUTHENTICATED_BYTES || !canonical_proc_path(canonical_path) {
            return Err(libc::EINVAL);
        }
        let name = encode_carrier_name(self.public_id, canonical_path, virtual_nofollow)?;
        let c_name = CString::new(name.clone()).map_err(|_| libc::EINVAL)?;
        // SAFETY: c_name is live and NUL-terminated; the flags are Linux memfd flags.
        let raw = unsafe {
            libc::memfd_create(
                c_name.as_ptr(),
                (libc::MFD_CLOEXEC | libc::MFD_ALLOW_SEALING) as libc::c_uint,
            )
        };
        if raw < 0 {
            return Err(last_errno());
        }
        // SAFETY: memfd_create returned a new descriptor owned by this call.
        let mut constructor = unsafe { File::from_raw_fd(raw) };
        constructor.write_all(content).map_err(io_errno)?;
        // SAFETY: constructor owns a live descriptor and mode has permission bits only.
        if unsafe { libc::fchmod(constructor.as_raw_fd(), 0o444) } != 0 {
            return Err(last_errno());
        }
        // SAFETY: constructor is a sealable memfd and REQUIRED_SEALS is a valid mask.
        if unsafe { libc::fcntl(constructor.as_raw_fd(), libc::F_ADD_SEALS, REQUIRED_SEALS) } != 0 {
            return Err(last_errno());
        }
        if get_seals(&constructor)? != self.profile.seals {
            return Err(libc::EOPNOTSUPP);
        }

        let initial_inspection = open_inspection_alias(&constructor)?;
        let initial = self.capture_pair(&constructor, &initial_inspection)?;
        let transcript = transcript(&initial, content);
        let tag = rejection_sample(&self.key, &transcript);
        set_mtime_tag(&constructor, tag)?;
        drop(initial_inspection);

        let exposed = open_exposed_alias(&constructor, path_only, guest_status_flags)?;
        drop(constructor);
        let inspection = open_inspection_alias(&exposed)?;
        let mut budget = CarrierAuthBudget::new();
        let mut cache = CarrierAuthCache::new();
        let authenticated =
            self.authenticate_reserved(&exposed, &inspection, &mut budget, &mut cache)?;
        if authenticated.canonical_path != canonical_path
            || authenticated.virtual_nofollow != virtual_nofollow
            || authenticated.content.as_ref() != content
        {
            return Err(libc::EBADMSG);
        }
        Ok(exposed)
    }

    /// Authenticate a candidate already known to have a reserved name. The
    /// caller owns and retires both descriptors, which is important while the
    /// serialized guest file-table lock is held.
    pub(crate) fn authenticate_reserved(
        &self,
        source: &File,
        inspection: &File,
        budget: &mut CarrierAuthBudget,
        cache: &mut CarrierAuthCache,
    ) -> Result<AuthenticatedProcCarrier, libc::c_int> {
        self.authenticate_reserved_with_hook(source, inspection, budget, cache, || {})
    }

    fn authenticate_reserved_with_hook(
        &self,
        source: &File,
        inspection: &File,
        budget: &mut CarrierAuthBudget,
        cache: &mut CarrierAuthCache,
        after_content_read: impl FnOnce(),
    ) -> Result<AuthenticatedProcCarrier, libc::c_int> {
        let before = self.capture_pair(source, inspection)?;
        if before.parsed.authority_id != self.public_id {
            return Err(libc::EBADMSG);
        }
        let key = CarrierCacheKey::from(&before);
        if let Some(cached) = cache.entries.get(&key) {
            if !snapshot_matches_except_tag(&before, &cached.snapshot)
                || !constant_time_tag_eq(before.tag, cached.expected_tag)
            {
                return Err(libc::EBADMSG);
            }
            return Ok(cached.result.clone());
        }

        let size = usize::try_from(before.stat.size).map_err(|_| libc::EFBIG)?;
        budget.charge(size)?;
        let content = Arc::<[u8]>::from(read_exact_content(inspection, size)?);
        let expected_tag = rejection_sample(&self.key, &transcript(&before, &content));
        if !constant_time_tag_eq(before.tag, expected_tag) {
            return Err(libc::EBADMSG);
        }
        after_content_read();
        let after = self.capture_pair(source, inspection)?;
        if before != after || !constant_time_tag_eq(after.tag, expected_tag) {
            return Err(libc::EBADMSG);
        }
        let result = AuthenticatedProcCarrier {
            canonical_path: after.parsed.canonical_path.clone(),
            virtual_nofollow: after.parsed.virtual_nofollow,
            content,
        };
        cache.entries.insert(
            key,
            CachedAuthentication {
                snapshot: after,
                expected_tag,
                result: result.clone(),
            },
        );
        Ok(result)
    }

    fn capture_pair(
        &self,
        source: &File,
        inspection: &File,
    ) -> Result<CarrierSnapshot, libc::c_int> {
        let source_name = descriptor_link_target(source).map_err(io_errno)?;
        let inspection_name = descriptor_link_target(inspection).map_err(io_errno)?;
        if source_name != inspection_name {
            return Err(libc::EBADMSG);
        }
        let parsed = parse_carrier_target(&source_name)?;

        let source_observation = observe_basic(source).map_err(|_| libc::EBADMSG)?;
        let inspection_observation = observe_basic(inspection).map_err(|_| libc::EBADMSG)?;
        if source_observation != inspection_observation {
            return Err(libc::EBADMSG);
        }
        let (statx_unique_mount_id, unique_handle_mount_id) = match self.profile.mode {
            MountIdMode::Legacy => (None, None),
            MountIdMode::StatxUnique => (
                Some(required_unique_statx(source).map_err(|_| libc::EBADMSG)?),
                None,
            ),
            MountIdMode::FullUnique => {
                let statx_id = required_unique_statx(source).map_err(|_| libc::EBADMSG)?;
                let (opaque, handle_id) = file_handle(source, true).map_err(|_| libc::EBADMSG)?;
                if opaque != source_observation.handle.opaque || handle_id != statx_id {
                    return Err(libc::EBADMSG);
                }
                (Some(statx_id), Some(handle_id))
            }
        };
        let inspection_unique_statx = match self.profile.mode {
            MountIdMode::Legacy => None,
            MountIdMode::StatxUnique | MountIdMode::FullUnique => {
                Some(required_unique_statx(inspection).map_err(|_| libc::EBADMSG)?)
            }
        };
        if inspection_unique_statx != statx_unique_mount_id {
            return Err(libc::EBADMSG);
        }
        if self.profile.mode == MountIdMode::FullUnique {
            let (opaque, mount_id) = file_handle(inspection, true).map_err(|_| libc::EBADMSG)?;
            if opaque != source_observation.handle.opaque
                || Some(mount_id) != unique_handle_mount_id
            {
                return Err(libc::EBADMSG);
            }
        }

        let seals = get_seals(inspection).map_err(|_| libc::EBADMSG)?;
        let stat = StableStat {
            statx_unique_mount_id,
            ..source_observation.stat.clone()
        };
        let handle = HandleIdentity {
            unique_mount_id: unique_handle_mount_id,
            ..source_observation.handle.clone()
        };
        if stat.filesystem_type != self.profile.filesystem_type
            || stat.device_major != self.profile.device_major
            || stat.device_minor != self.profile.device_minor
            || handle.legacy_mount_id != self.profile.legacy_mount_id
            || stat.statx_mount_id != self.profile.statx_mount_id
            || stat.statx_unique_mount_id != self.profile.statx_unique_mount_id
            || handle.unique_mount_id != self.profile.unique_handle_mount_id
            || seals != self.profile.seals
            || stat.mode & libc::S_IFMT as u16 != libc::S_IFREG as u16
        {
            return Err(libc::EBADMSG);
        }
        let tag = timestamp_to_tag(
            source_observation.mtime_seconds,
            source_observation.mtime_nanoseconds,
        )?;
        Ok(CarrierSnapshot {
            name: carrier_name_from_target(&source_name)?.to_vec(),
            parsed,
            handle,
            stat,
            seals,
            tag,
        })
    }
}

fn qualified_profile() -> Result<ProbeProfile, ProbeFailure> {
    static PROFILE: OnceLock<Result<ProbeProfile, ProbeFailure>> = OnceLock::new();
    PROFILE.get_or_init(probe_profile).clone()
}

pub(crate) fn reserved_guest_name(name: &[u8]) -> bool {
    name.starts_with(RESERVED_NAME_PREFIX)
}

/// Detect a reserved candidate without trusting its name. Failure to inspect a
/// complete descriptor link is returned to the caller and is never Ordinary.
pub(crate) fn candidate_kind(file: &File) -> Result<ProcCarrierCandidate, libc::c_int> {
    candidate_kind_from_link(descriptor_link_target(file).map_err(io_errno))
}

fn candidate_kind_from_link(
    target: Result<Vec<u8>, libc::c_int>,
) -> Result<ProcCarrierCandidate, libc::c_int> {
    let target = target?;
    let Some(name) = target.strip_prefix(MEMFD_LINK_PREFIX) else {
        return Ok(ProcCarrierCandidate::Ordinary);
    };
    Ok(if reserved_guest_name(name) {
        ProcCarrierCandidate::Reserved
    } else {
        ProcCarrierCandidate::Ordinary
    })
}

/// Open a readable view without changing the source OFD or consuming its
/// offset. The caller owns this alias and controls deferred retirement.
pub(crate) fn open_inspection_alias(source: &File) -> Result<File, libc::c_int> {
    open_proc_fd_alias(source, libc::O_RDONLY | libc::O_CLOEXEC)
}

fn open_exposed_alias(
    source: &File,
    path_only: bool,
    guest_status_flags: libc::c_int,
) -> Result<File, libc::c_int> {
    let flags = if path_only {
        libc::O_PATH | libc::O_CLOEXEC
    } else {
        libc::O_RDONLY | libc::O_CLOEXEC | (guest_status_flags & PRESERVED_STATUS_FLAGS)
    };
    open_proc_fd_alias(source, flags)
}

fn open_proc_fd_alias(source: &File, flags: libc::c_int) -> Result<File, libc::c_int> {
    let path = CString::new(format!("/proc/self/fd/{}", source.as_raw_fd()))
        .expect("decimal descriptor path has no NUL");
    // SAFETY: path is NUL-terminated and source remains pinned for this call.
    let raw = unsafe { libc::open(path.as_ptr(), flags) };
    if raw < 0 {
        Err(last_errno())
    } else {
        // SAFETY: open returned a new owned descriptor.
        Ok(unsafe { File::from_raw_fd(raw) })
    }
}

fn encode_carrier_name(
    authority_id: [u8; 16],
    canonical_path: &[u8],
    virtual_nofollow: bool,
) -> Result<Vec<u8>, libc::c_int> {
    let mut name = RESERVED_NAME_PREFIX.to_vec();
    name.push(b'.');
    append_hex(&mut name, &authority_id);
    name.extend_from_slice(b".proc.");
    name.extend_from_slice(if virtual_nofollow { b"nf1." } else { b"nf0." });
    append_hex(&mut name, canonical_path);
    if name.len() > 249 {
        return Err(libc::ENAMETOOLONG);
    }
    Ok(name)
}

fn parse_carrier_target(target: &[u8]) -> Result<ParsedCarrierName, libc::c_int> {
    let name = carrier_name_from_target(target)?;
    let suffix = name
        .strip_prefix(RESERVED_NAME_PREFIX)
        .and_then(|suffix| suffix.strip_prefix(b"."))
        .ok_or(libc::EBADMSG)?;
    let mut fields = suffix.split(|byte| *byte == b'.');
    let authority = fields.next().ok_or(libc::EBADMSG)?;
    let kind = fields.next().ok_or(libc::EBADMSG)?;
    let nofollow = fields.next().ok_or(libc::EBADMSG)?;
    let path = fields.next().ok_or(libc::EBADMSG)?;
    if fields.next().is_some() || kind != KIND_PROC {
        return Err(libc::EBADMSG);
    }
    let authority = decode_hex(authority).ok_or(libc::EBADMSG)?;
    let authority_id: [u8; 16] = authority.try_into().map_err(|_| libc::EBADMSG)?;
    let virtual_nofollow = match nofollow {
        b"nf0" => false,
        b"nf1" => true,
        _ => return Err(libc::EBADMSG),
    };
    let canonical_path = decode_hex(path).ok_or(libc::EBADMSG)?;
    if !canonical_proc_path(&canonical_path) {
        return Err(libc::EBADMSG);
    }
    Ok(ParsedCarrierName {
        authority_id,
        canonical_path,
        virtual_nofollow,
    })
}

fn carrier_name_from_target(target: &[u8]) -> Result<&[u8], libc::c_int> {
    target
        .strip_prefix(MEMFD_LINK_PREFIX)
        .and_then(|name| name.strip_suffix(MEMFD_LINK_SUFFIX))
        .filter(|name| reserved_guest_name(name))
        .ok_or(libc::EBADMSG)
}

fn canonical_proc_path(path: &[u8]) -> bool {
    path.starts_with(b"/proc/")
        && !path.ends_with(b"/")
        && !path.contains(&0)
        && path
            .split(|byte| *byte == b'/')
            .skip(1)
            .all(|part| !part.is_empty() && part != b"." && part != b"..")
}

fn append_hex(output: &mut Vec<u8>, bytes: &[u8]) {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    for byte in bytes {
        output.push(HEX[usize::from(byte >> 4)]);
        output.push(HEX[usize::from(byte & 0x0f)]);
    }
}

fn decode_hex(value: &[u8]) -> Option<Vec<u8>> {
    if !value.len().is_multiple_of(2) {
        return None;
    }
    value
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| Some((decode_nibble(pair[0])? << 4) | decode_nibble(pair[1])?))
        .collect()
}

fn decode_nibble(value: u8) -> Option<u8> {
    match value {
        b'0'..=b'9' => Some(value - b'0'),
        b'a'..=b'f' => Some(value - b'a' + 10),
        _ => None,
    }
}

fn descriptor_link_target(file: &File) -> io::Result<Vec<u8>> {
    let path = CString::new(format!("/proc/self/fd/{}", file.as_raw_fd()))
        .expect("decimal descriptor path has no NUL");
    let mut bytes = [0_u8; LINK_TARGET_CAPACITY];
    // SAFETY: path is NUL-terminated and bytes is writable for its full length.
    let length = unsafe {
        libc::readlink(
            path.as_ptr(),
            bytes.as_mut_ptr().cast(),
            bytes.len() as libc::size_t,
        )
    };
    if length < 0 {
        return Err(io::Error::last_os_error());
    }
    complete_link_target(&bytes, length as usize)
}

fn complete_link_target(bytes: &[u8], length: usize) -> io::Result<Vec<u8>> {
    if length >= bytes.len() {
        return Err(io::Error::from_raw_os_error(libc::EOVERFLOW));
    }
    Ok(bytes[..length].to_vec())
}

fn random_authority_material() -> io::Result<([u8; 32], [u8; 16])> {
    let mut material = [0_u8; 48];
    let mut filled = 0;
    while filled < material.len() {
        // SAFETY: the remaining slice is writable and the kernel receives its exact length.
        let result = unsafe {
            libc::syscall(
                libc::SYS_getrandom,
                material[filled..].as_mut_ptr(),
                material.len() - filled,
                0,
            )
        };
        if result < 0 {
            let error = io::Error::last_os_error();
            if error.raw_os_error() == Some(libc::EINTR) {
                continue;
            }
            return Err(error);
        }
        if result == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "getrandom returned zero before filling authority material",
            ));
        }
        filled += result as usize;
    }
    let mut key = [0_u8; 32];
    key.copy_from_slice(&material[..32]);
    let mut public_id = [0_u8; 16];
    public_id.copy_from_slice(&material[32..]);
    Ok((key, public_id))
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct BasicObservation {
    handle: HandleIdentity,
    stat: StableStat,
    mtime_seconds: i64,
    mtime_nanoseconds: u32,
}

fn observe_basic(file: &File) -> io::Result<BasicObservation> {
    let (opaque, raw_legacy_mount_id) = file_handle(file, false)?;
    let legacy_mount_id = libc::c_int::try_from(raw_legacy_mount_id).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "legacy mount ID does not fit c_int",
        )
    })?;
    let statx = statx_empty(file, libc::STATX_BASIC_STATS | libc::STATX_MNT_ID)?;
    if statx.stx_mask & REQUIRED_STATX_MASK != REQUIRED_STATX_MASK {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            format!(
                "statx mask {:#x} lacks required bits {:#x}",
                statx.stx_mask, REQUIRED_STATX_MASK
            ),
        ));
    }
    let legacy_mount_id_u64 = u64::try_from(legacy_mount_id)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "negative legacy mount ID"))?;
    if statx.stx_mnt_id != legacy_mount_id_u64 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "legacy handle mount ID {legacy_mount_id} differs from statx mount ID {}",
                statx.stx_mnt_id
            ),
        ));
    }
    let mut filesystem = std::mem::MaybeUninit::<libc::statfs>::zeroed();
    // SAFETY: filesystem is writable and file owns a live descriptor.
    if unsafe { libc::fstatfs(file.as_raw_fd(), filesystem.as_mut_ptr()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: fstatfs initialized the value on success.
    let filesystem = unsafe { filesystem.assume_init() };
    Ok(BasicObservation {
        handle: HandleIdentity {
            opaque,
            legacy_mount_id,
            unique_mount_id: None,
        },
        stat: stable_stat_from_statx(&statx, filesystem.f_type, None),
        mtime_seconds: statx.stx_mtime.tv_sec,
        mtime_nanoseconds: statx.stx_mtime.tv_nsec,
    })
}

fn stable_stat_from_statx(
    statx: &libc::statx,
    filesystem_type: i64,
    statx_unique_mount_id: Option<u64>,
) -> StableStat {
    StableStat {
        filesystem_type,
        device_major: statx.stx_dev_major,
        device_minor: statx.stx_dev_minor,
        inode: statx.stx_ino,
        mode: statx.stx_mode,
        nlink: statx.stx_nlink,
        uid: statx.stx_uid,
        gid: statx.stx_gid,
        rdev_major: statx.stx_rdev_major,
        rdev_minor: statx.stx_rdev_minor,
        block_size: statx.stx_blksize,
        attributes: statx.stx_attributes,
        size: statx.stx_size,
        statx_mount_id: statx.stx_mnt_id,
        statx_unique_mount_id,
    }
}

fn statx_empty(file: &File, mask: libc::c_uint) -> io::Result<libc::statx> {
    let mut stat = std::mem::MaybeUninit::<libc::statx>::zeroed();
    // SAFETY: the empty path is NUL-terminated, file is live, and stat is writable.
    let result = unsafe {
        libc::statx(
            file.as_raw_fd(),
            c"".as_ptr(),
            libc::AT_EMPTY_PATH,
            mask,
            stat.as_mut_ptr(),
        )
    };
    if result != 0 {
        Err(io::Error::last_os_error())
    } else {
        // SAFETY: statx initialized the output on success.
        Ok(unsafe { stat.assume_init() })
    }
}

fn optional_unique_statx(file: &File) -> io::Result<Option<u64>> {
    match statx_empty(file, libc::STATX_BASIC_STATS | STATX_MNT_ID_UNIQUE) {
        Ok(stat) if stat.stx_mask & STATX_MNT_ID_UNIQUE != 0 => Ok(Some(stat.stx_mnt_id)),
        Ok(_) => Ok(None),
        Err(error) if unsupported_probe_errno(&error) => Ok(None),
        Err(error) => Err(error),
    }
}

fn required_unique_statx(file: &File) -> io::Result<u64> {
    optional_unique_statx(file)?.ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::Unsupported,
            "selected unique statx mode is no longer available",
        )
    })
}

fn file_handle(file: &File, unique_mount_id: bool) -> io::Result<(OpaqueHandle, u64)> {
    let mut capacity = 0usize;
    for _ in 0..HANDLE_ATTEMPTS {
        let storage_bytes = std::mem::size_of::<libc::file_handle>()
            .checked_add(capacity)
            .ok_or_else(|| io::Error::from_raw_os_error(libc::EOVERFLOW))?;
        let words = storage_bytes.div_ceil(std::mem::size_of::<u64>()).max(1);
        let mut storage = vec![0_u64; words];
        let handle = storage.as_mut_ptr().cast::<libc::file_handle>();
        // SAFETY: storage is at least the header size, aligned to u64, and remains live.
        unsafe {
            (*handle).handle_bytes = capacity as libc::c_uint;
            (*handle).handle_type = 0;
        }
        let (result, mount_id) = if unique_mount_id {
            let mut output = 0_u64;
            // SAFETY: all pointers are live. Raw syscall is required because the
            // libc wrapper types this Linux-6.12 output as c_int rather than u64.
            let result = unsafe {
                libc::syscall(
                    libc::SYS_name_to_handle_at,
                    file.as_raw_fd(),
                    c"".as_ptr(),
                    handle,
                    std::ptr::from_mut(&mut output),
                    libc::AT_EMPTY_PATH | AT_HANDLE_MNT_ID_UNIQUE,
                )
            };
            (result, output)
        } else {
            let mut output: libc::c_int = 0;
            // SAFETY: all pointers are live and output has the legacy c_int ABI.
            let result = unsafe {
                libc::syscall(
                    libc::SYS_name_to_handle_at,
                    file.as_raw_fd(),
                    c"".as_ptr(),
                    handle,
                    std::ptr::from_mut(&mut output),
                    libc::AT_EMPTY_PATH,
                )
            };
            let mount_id = u64::try_from(output).map_err(|_| {
                io::Error::new(io::ErrorKind::InvalidData, "negative legacy mount ID")
            })?;
            (result, mount_id)
        };
        // SAFETY: the kernel may update only the initialized header fields.
        let returned = unsafe { (*handle).handle_bytes as usize };
        if result == 0 {
            validate_handle_size(capacity, returned)?;
            // SAFETY: returned is within the allocation, immediately after the header.
            let bytes = unsafe {
                std::slice::from_raw_parts(
                    handle
                        .cast::<u8>()
                        .add(std::mem::size_of::<libc::file_handle>()),
                    returned,
                )
            }
            .to_vec();
            // SAFETY: successful syscall initialized handle_type.
            let kind = unsafe { (*handle).handle_type };
            return Ok((OpaqueHandle { kind, bytes }, mount_id));
        }
        let error = io::Error::last_os_error();
        if error.raw_os_error() != Some(libc::EOVERFLOW) {
            return Err(error);
        }
        validate_requested_handle_size(capacity, returned)?;
        capacity = returned;
    }
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "name_to_handle_at exceeded four sizing attempts",
    ))
}

fn validate_requested_handle_size(current: usize, requested: usize) -> io::Result<()> {
    if requested == 0
        || !requested.is_multiple_of(HANDLE_SIZE_GRANULE)
        || requested > MAX_HANDLE_SZ
        || requested <= current
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("invalid handle growth from {current} to {requested}"),
        ));
    }
    Ok(())
}

fn validate_handle_size(capacity: usize, returned: usize) -> io::Result<()> {
    if returned == 0
        || !returned.is_multiple_of(HANDLE_SIZE_GRANULE)
        || returned > MAX_HANDLE_SZ
        || returned > capacity
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("invalid successful handle size {returned} for capacity {capacity}"),
        ));
    }
    Ok(())
}

fn optional_unique_handle(file: &File) -> io::Result<Option<(OpaqueHandle, u64)>> {
    match file_handle(file, true) {
        Ok((handle, mount_id)) => Ok(Some((handle, mount_id))),
        Err(error) if unsupported_probe_errno(&error) => Ok(None),
        Err(error) => Err(error),
    }
}

fn unsupported_probe_errno(error: &io::Error) -> bool {
    matches!(
        error.raw_os_error(),
        Some(libc::EINVAL | libc::EOPNOTSUPP | libc::ENOSYS)
    )
}

fn get_seals(file: &File) -> Result<libc::c_int, libc::c_int> {
    // SAFETY: file owns a live descriptor and F_GET_SEALS takes no third argument.
    let seals = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GET_SEALS) };
    if seals < 0 {
        Err(last_errno())
    } else {
        Ok(seals)
    }
}

fn accepted_seal_profile(seals: libc::c_int) -> bool {
    matches!(seals, REQUIRED_SEALS | ACCEPTED_SEALS_WITH_EXEC) && seals & F_SEAL_FUTURE_WRITE == 0
}

struct ProbeFiles {
    constructor: File,
    readonly: File,
    path_only: File,
}

fn create_probe_files(name: &std::ffi::CStr, byte: u8) -> Result<ProbeFiles, ProbeFailure> {
    // SAFETY: name is NUL-terminated and flags are valid on the Linux-5.8 baseline.
    let raw = unsafe {
        libc::memfd_create(
            name.as_ptr(),
            (libc::MFD_CLOEXEC | libc::MFD_ALLOW_SEALING) as libc::c_uint,
        )
    };
    if raw < 0 {
        return Err(ProbeFailure::io("memfd_create", io::Error::last_os_error()));
    }
    // SAFETY: memfd_create returned a new owned descriptor.
    let mut constructor = unsafe { File::from_raw_fd(raw) };
    constructor
        .write_all(&[byte])
        .map_err(|error| ProbeFailure::io("probe content write", error))?;
    // SAFETY: constructor owns a live descriptor.
    if unsafe { libc::fchmod(constructor.as_raw_fd(), 0o444) } != 0 {
        return Err(ProbeFailure::io("probe chmod", io::Error::last_os_error()));
    }
    // SAFETY: constructor is a live sealable memfd.
    if unsafe { libc::fcntl(constructor.as_raw_fd(), libc::F_ADD_SEALS, REQUIRED_SEALS) } != 0 {
        return Err(ProbeFailure::io("probe seals", io::Error::last_os_error()));
    }
    set_mtime_tag(&constructor, TAG_MODULUS - 1)
        .map_err(|errno| ProbeFailure::message("probe time64 mtime write", errno.to_string()))?;
    let readonly = open_proc_fd_alias(&constructor, libc::O_RDONLY | libc::O_CLOEXEC)
        .map_err(|errno| ProbeFailure::message("probe O_RDONLY reopen", errno.to_string()))?;
    let path_only = open_proc_fd_alias(&constructor, libc::O_PATH | libc::O_CLOEXEC)
        .map_err(|errno| ProbeFailure::message("probe O_PATH reopen", errno.to_string()))?;
    Ok(ProbeFiles {
        constructor,
        readonly,
        path_only,
    })
}

fn validate_probe_content(files: &ProbeFiles, expected: u8) -> Result<(), ProbeFailure> {
    for (phase, file) in [
        ("probe constructor content", &files.constructor),
        ("probe readonly content", &files.readonly),
    ] {
        let content = read_exact_content(file, 1)
            .map_err(|errno| ProbeFailure::message(phase, errno.to_string()))?;
        if content != [expected] {
            return Err(ProbeFailure::message(phase, "content mismatch"));
        }
    }
    let path_alias = open_inspection_alias(&files.path_only)
        .map_err(|errno| ProbeFailure::message("probe O_PATH content alias", errno.to_string()))?;
    let content = read_exact_content(&path_alias, 1)
        .map_err(|errno| ProbeFailure::message("probe O_PATH content", errno.to_string()))?;
    if content != [expected] {
        return Err(ProbeFailure::message(
            "probe O_PATH content",
            "content mismatch",
        ));
    }
    Ok(())
}

fn probe_profile() -> Result<ProbeProfile, ProbeFailure> {
    let first = create_probe_files(c"reverie-kvm-proc-carrier-probe-a", b'a')?;
    let second = create_probe_files(c"reverie-kvm-proc-carrier-probe-b", b'b')?;
    validate_probe_content(&first, b'a')?;
    validate_probe_content(&second, b'b')?;
    let views = [
        &first.constructor,
        &first.readonly,
        &first.path_only,
        &second.constructor,
        &second.readonly,
        &second.path_only,
    ];
    let observations = views
        .iter()
        .map(|file| observe_basic(file).map_err(|error| ProbeFailure::io("identity probe", error)))
        .collect::<Result<Vec<_>, _>>()?;
    validate_basic_probe(&observations)?;

    let observed_seals = [
        &first.constructor,
        &first.readonly,
        &second.constructor,
        &second.readonly,
    ]
    .map(|file| {
        get_seals(file)
            .map_err(|errno| ProbeFailure::message("probe seal readback", errno.to_string()))
    })
    .into_iter()
    .collect::<Result<Vec<_>, _>>()?;
    let seals = observed_seals[0];
    if !accepted_seal_profile(seals) || observed_seals.iter().any(|value| *value != seals) {
        return Err(ProbeFailure::message(
            "probe seal profile",
            format!("expected one exact 0x0f/0x2f profile, observed {observed_seals:#x?}"),
        ));
    }

    let unique_statx = views
        .iter()
        .map(|file| {
            optional_unique_statx(file)
                .map_err(|error| ProbeFailure::io("unique statx probe", error))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let statx_unique_mount_id = consistent_optional_ids(&unique_statx, "unique statx probe")?;
    let unique_handles = views
        .iter()
        .map(|file| {
            optional_unique_handle(file)
                .map_err(|error| ProbeFailure::io("unique handle probe", error))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let unique_handle_mount_id =
        validate_unique_handles(&observations, &unique_handles, statx_unique_mount_id)?;
    let mode = select_mount_mode(statx_unique_mount_id, unique_handle_mount_id)?;
    let baseline = &observations[0];
    Ok(ProbeProfile {
        mode,
        filesystem_type: baseline.stat.filesystem_type,
        device_major: baseline.stat.device_major,
        device_minor: baseline.stat.device_minor,
        legacy_mount_id: baseline.handle.legacy_mount_id,
        statx_mount_id: baseline.stat.statx_mount_id,
        statx_unique_mount_id,
        unique_handle_mount_id,
        seals,
    })
}

fn select_mount_mode(
    statx_unique_mount_id: Option<u64>,
    unique_handle_mount_id: Option<u64>,
) -> Result<MountIdMode, ProbeFailure> {
    match (statx_unique_mount_id, unique_handle_mount_id) {
        (None, None) => Ok(MountIdMode::Legacy),
        (Some(_), None) => Ok(MountIdMode::StatxUnique),
        (Some(_), Some(_)) => Ok(MountIdMode::FullUnique),
        (None, Some(_)) => Err(ProbeFailure::message(
            "mount identity mode",
            "unique handle ID appeared without unique statx",
        )),
    }
}

fn validate_basic_probe(observations: &[BasicObservation]) -> Result<(), ProbeFailure> {
    if observations.len() != 6 {
        return Err(ProbeFailure::message(
            "identity probe",
            "probe did not retain two three-view objects",
        ));
    }
    let first = &observations[0];
    if first.stat.filesystem_type != TMPFS_MAGIC
        || first.stat.mode != (libc::S_IFREG as u16 | 0o444)
        || first.stat.size != 1
        || first.stat.nlink != 0
    {
        return Err(ProbeFailure::message(
            "identity probe",
            format!(
                "carrier lacks exact tmpfs/regular-0444/size-1/nlink-0 properties: fs={:#x}, mode={:#o}, size={}, nlink={}",
                first.stat.filesystem_type, first.stat.mode, first.stat.size, first.stat.nlink
            ),
        ));
    }
    for observation in observations {
        if observation.stat.filesystem_type != first.stat.filesystem_type
            || observation.stat.mode != (libc::S_IFREG as u16 | 0o444)
            || observation.stat.size != 1
            || observation.stat.nlink != 0
            || observation.stat.device_major != first.stat.device_major
            || observation.stat.device_minor != first.stat.device_minor
            || observation.handle.legacy_mount_id != first.handle.legacy_mount_id
            || observation.stat.statx_mount_id != first.stat.statx_mount_id
            || observation.mtime_seconds != i64::MAX - 1
            || observation.mtime_nanoseconds != 999_999_999
        {
            return Err(ProbeFailure::message(
                "identity probe",
                "probe views disagree on filesystem/device/mount/time64 identity",
            ));
        }
    }
    if observations[0].handle != observations[1].handle
        || observations[0].handle != observations[2].handle
        || observations[0].stat != observations[1].stat
        || observations[0].stat != observations[2].stat
        || observations[3].handle != observations[4].handle
        || observations[3].handle != observations[5].handle
        || observations[3].stat != observations[4].stat
        || observations[3].stat != observations[5].stat
    {
        return Err(ProbeFailure::message(
            "identity probe",
            "O_RDWR/O_RDONLY/O_PATH views are not stable",
        ));
    }
    if observations[0].handle.opaque == observations[3].handle.opaque
        || observations[0].stat.inode == observations[3].stat.inode
    {
        return Err(ProbeFailure::message(
            "identity probe",
            "simultaneous memfds do not have distinct handles/inodes",
        ));
    }
    Ok(())
}

fn consistent_optional_ids(
    values: &[Option<u64>],
    phase: &'static str,
) -> Result<Option<u64>, ProbeFailure> {
    let Some(first) = values.first().copied() else {
        return Err(ProbeFailure::message(phase, "empty capability sample"));
    };
    if values.iter().copied().all(|value| value == first) {
        Ok(first)
    } else {
        Err(ProbeFailure::message(
            phase,
            "capability appeared or changed on only some retained views",
        ))
    }
}

fn validate_unique_handles(
    basics: &[BasicObservation],
    values: &[Option<(OpaqueHandle, u64)>],
    statx_unique_mount_id: Option<u64>,
) -> Result<Option<u64>, ProbeFailure> {
    if values.len() != basics.len() || values.is_empty() {
        return Err(ProbeFailure::message(
            "unique handle probe",
            "unique handle sample cardinality mismatch",
        ));
    }
    if values.iter().all(Option::is_none) {
        return Ok(None);
    }
    if values.iter().any(Option::is_none) {
        return Err(ProbeFailure::message(
            "unique handle probe",
            "unique handle mode appeared on only some retained views",
        ));
    }
    let expected_mount = statx_unique_mount_id.ok_or_else(|| {
        ProbeFailure::message(
            "unique handle probe",
            "unique handle mode lacks unique statx identity",
        )
    })?;
    for (basic, value) in basics.iter().zip(values) {
        let (handle, mount_id) = value.as_ref().expect("checked all values are present");
        if handle != &basic.handle.opaque || *mount_id != expected_mount {
            return Err(ProbeFailure::message(
                "unique handle probe",
                "unique handle differs from legacy handle or unique statx mount ID",
            ));
        }
    }
    Ok(Some(expected_mount))
}

fn set_mtime_tag(file: &File, tag: u128) -> Result<(), libc::c_int> {
    let (seconds, nanoseconds) = tag_to_timestamp(tag).ok_or(libc::EINVAL)?;
    let times = [
        libc::timespec {
            tv_sec: 0,
            tv_nsec: UTIME_OMIT_RAW,
        },
        libc::timespec {
            tv_sec: seconds,
            tv_nsec: nanoseconds,
        },
    ];
    // SAFETY: file is live and times contains two initialized timespecs.
    if unsafe { libc::futimens(file.as_raw_fd(), times.as_ptr()) } != 0 {
        Err(last_errno())
    } else {
        Ok(())
    }
}

fn tag_to_timestamp(tag: u128) -> Option<(libc::time_t, libc::c_long)> {
    if tag >= TAG_MODULUS {
        return None;
    }
    let seconds = i64::try_from(tag / 1_000_000_000).ok()?;
    let nanoseconds = libc::c_long::try_from(tag % 1_000_000_000).ok()?;
    Some((seconds as libc::time_t, nanoseconds))
}

fn timestamp_to_tag(seconds: i64, nanoseconds: u32) -> Result<u128, libc::c_int> {
    if !(0..i64::MAX).contains(&seconds) || nanoseconds >= 1_000_000_000 {
        return Err(libc::EBADMSG);
    }
    Ok(seconds as u128 * 1_000_000_000 + u128::from(nanoseconds))
}

fn transcript(snapshot: &CarrierSnapshot, content: &[u8]) -> Vec<u8> {
    let mut transcript = Vec::new();
    transcript.extend_from_slice(TRANSCRIPT_DOMAIN);
    transcript.extend_from_slice(&TRANSCRIPT_VERSION.to_be_bytes());
    push_tlv(
        &mut transcript,
        FIELD_AUTHORITY,
        &snapshot.parsed.authority_id,
    );
    push_tlv(&mut transcript, FIELD_RESERVED_NAME, &snapshot.name);
    push_tlv(&mut transcript, FIELD_KIND, KIND_PROC);
    push_tlv(
        &mut transcript,
        FIELD_CANONICAL_PATH,
        &snapshot.parsed.canonical_path,
    );
    push_tlv(
        &mut transcript,
        FIELD_VIRTUAL_FLAGS,
        &(if snapshot.parsed.virtual_nofollow {
            libc::O_NOFOLLOW as u32
        } else {
            0
        })
        .to_be_bytes(),
    );
    push_tlv(
        &mut transcript,
        FIELD_MOUNT_MODE,
        &[mount_mode_from_snapshot(snapshot).wire_value()],
    );
    push_tlv(
        &mut transcript,
        FIELD_LEGACY_HANDLE_MOUNT_ID,
        &snapshot.handle.legacy_mount_id.to_be_bytes(),
    );
    push_tlv(
        &mut transcript,
        FIELD_STATX_MOUNT_ID,
        &snapshot.stat.statx_mount_id.to_be_bytes(),
    );
    if let Some(value) = snapshot.stat.statx_unique_mount_id {
        push_tlv(
            &mut transcript,
            FIELD_STATX_UNIQUE_MOUNT_ID,
            &value.to_be_bytes(),
        );
    }
    if let Some(value) = snapshot.handle.unique_mount_id {
        push_tlv(
            &mut transcript,
            FIELD_UNIQUE_HANDLE_MOUNT_ID,
            &value.to_be_bytes(),
        );
    }
    push_tlv(
        &mut transcript,
        FIELD_HANDLE_LENGTH,
        &(snapshot.handle.opaque.bytes.len() as u32).to_be_bytes(),
    );
    push_tlv(
        &mut transcript,
        FIELD_HANDLE_TYPE,
        &snapshot.handle.opaque.kind.to_be_bytes(),
    );
    push_tlv(
        &mut transcript,
        FIELD_HANDLE_BYTES,
        &snapshot.handle.opaque.bytes,
    );
    push_tlv(
        &mut transcript,
        FIELD_FILESYSTEM_TYPE,
        &snapshot.stat.filesystem_type.to_be_bytes(),
    );
    push_tlv(
        &mut transcript,
        FIELD_DEVICE_MAJOR,
        &snapshot.stat.device_major.to_be_bytes(),
    );
    push_tlv(
        &mut transcript,
        FIELD_DEVICE_MINOR,
        &snapshot.stat.device_minor.to_be_bytes(),
    );
    push_tlv(
        &mut transcript,
        FIELD_INODE,
        &snapshot.stat.inode.to_be_bytes(),
    );
    push_tlv(
        &mut transcript,
        FIELD_MODE,
        &snapshot.stat.mode.to_be_bytes(),
    );
    push_tlv(
        &mut transcript,
        FIELD_NLINK,
        &snapshot.stat.nlink.to_be_bytes(),
    );
    push_tlv(&mut transcript, FIELD_UID, &snapshot.stat.uid.to_be_bytes());
    push_tlv(&mut transcript, FIELD_GID, &snapshot.stat.gid.to_be_bytes());
    push_tlv(
        &mut transcript,
        FIELD_RDEV_MAJOR,
        &snapshot.stat.rdev_major.to_be_bytes(),
    );
    push_tlv(
        &mut transcript,
        FIELD_RDEV_MINOR,
        &snapshot.stat.rdev_minor.to_be_bytes(),
    );
    push_tlv(
        &mut transcript,
        FIELD_BLOCK_SIZE,
        &snapshot.stat.block_size.to_be_bytes(),
    );
    push_tlv(
        &mut transcript,
        FIELD_ATTRIBUTES,
        &snapshot.stat.attributes.to_be_bytes(),
    );
    push_tlv(&mut transcript, FIELD_SEALS, &snapshot.seals.to_be_bytes());
    push_tlv(
        &mut transcript,
        FIELD_SIZE,
        &snapshot.stat.size.to_be_bytes(),
    );
    push_tlv(&mut transcript, FIELD_CONTENT, content);
    transcript
}

fn mount_mode_from_snapshot(snapshot: &CarrierSnapshot) -> MountIdMode {
    match (
        snapshot.stat.statx_unique_mount_id,
        snapshot.handle.unique_mount_id,
    ) {
        (None, None) => MountIdMode::Legacy,
        (Some(_), None) => MountIdMode::StatxUnique,
        (Some(_), Some(_)) => MountIdMode::FullUnique,
        (None, Some(_)) => unreachable!("unique handle ID requires unique statx ID"),
    }
}

fn push_tlv(output: &mut Vec<u8>, field: u16, value: &[u8]) {
    output.extend_from_slice(&field.to_be_bytes());
    output.extend_from_slice(&(value.len() as u64).to_be_bytes());
    output.extend_from_slice(value);
}

fn rejection_sample(key: &[u8; 32], transcript: &[u8]) -> u128 {
    rejection_sample_with(|counter| hmac_sample(key, transcript, counter))
}

fn hmac_sample(key: &[u8; 32], transcript: &[u8], counter: u64) -> u128 {
    let mut mac = HmacSha256::new_from_slice(key).expect("HMAC accepts a 256-bit key");
    mac.update(transcript);
    let mut counter_tlv = Vec::with_capacity(18);
    push_tlv(
        &mut counter_tlv,
        FIELD_SAMPLE_COUNTER,
        &counter.to_be_bytes(),
    );
    mac.update(&counter_tlv);
    let digest = mac.finalize().into_bytes();
    u128::from_be_bytes(digest[..16].try_into().expect("SHA-256 prefix is 128 bits"))
}

fn rejection_sample_with(mut sample: impl FnMut(u64) -> u128) -> u128 {
    let mut counter = 0_u64;
    loop {
        let candidate = sample(counter);
        if candidate >= TAG_REJECTION_FLOOR {
            return candidate % TAG_MODULUS;
        }
        counter = counter
            .checked_add(1)
            .expect("HMAC rejection counter exhausted without fallback");
    }
}

fn constant_time_tag_eq(left: u128, right: u128) -> bool {
    let mut left_output = hmac::digest::Output::<HmacSha256>::default();
    let mut right_output = hmac::digest::Output::<HmacSha256>::default();
    left_output[..16].copy_from_slice(&left.to_be_bytes());
    right_output[..16].copy_from_slice(&right.to_be_bytes());
    hmac::digest::CtOutput::<HmacSha256>::new(left_output)
        == hmac::digest::CtOutput::<HmacSha256>::new(right_output)
}

fn snapshot_matches_except_tag(left: &CarrierSnapshot, right: &CarrierSnapshot) -> bool {
    left.name == right.name
        && left.parsed == right.parsed
        && left.handle == right.handle
        && left.stat == right.stat
        && left.seals == right.seals
}

fn read_exact_content(file: &File, size: usize) -> Result<Vec<u8>, libc::c_int> {
    let mut content = vec![0_u8; size];
    let mut offset = 0usize;
    while offset < size {
        let count = file
            .read_at(&mut content[offset..], offset as u64)
            .map_err(io_errno)?;
        if count == 0 {
            return Err(libc::EBADMSG);
        }
        offset += count;
    }
    let mut extra = [0_u8; 1];
    if file.read_at(&mut extra, size as u64).map_err(io_errno)? != 0 {
        return Err(libc::EBADMSG);
    }
    Ok(content)
}

fn last_errno() -> libc::c_int {
    io::Error::last_os_error()
        .raw_os_error()
        .unwrap_or(libc::EIO)
}

fn io_errno(error: io::Error) -> libc::c_int {
    error.raw_os_error().unwrap_or(libc::EIO)
}

struct Hex<'a>(&'a [u8]);

impl fmt::Debug for Hex<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        for byte in self.0 {
            write!(formatter, "{byte:02x}")?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::io::Read;
    use std::io::Seek;
    use std::io::SeekFrom;

    use super::*;

    fn live_authority() -> Arc<ProcCarrierAuthority> {
        ProcCarrierAuthority::new_for_tests().expect("host supports authenticated carriers")
    }

    fn test_memfd(name: &[u8], content: &[u8]) -> File {
        let name = CString::new(name).unwrap();
        // SAFETY: name is NUL-terminated and both flags are valid memfd flags.
        let raw = unsafe {
            libc::memfd_create(
                name.as_ptr(),
                (libc::MFD_CLOEXEC | libc::MFD_ALLOW_SEALING) as libc::c_uint,
            )
        };
        assert!(raw >= 0, "memfd_create: {}", io::Error::last_os_error());
        // SAFETY: memfd_create returned a new owned descriptor.
        let mut file = unsafe { File::from_raw_fd(raw) };
        file.write_all(content).unwrap();
        file
    }

    fn test_snapshot(mode: MountIdMode) -> CarrierSnapshot {
        let (statx_unique_mount_id, unique_mount_id) = match mode {
            MountIdMode::Legacy => (None, None),
            MountIdMode::StatxUnique => (Some(23), None),
            MountIdMode::FullUnique => (Some(23), Some(23)),
        };
        let authority_id = [0x11; 16];
        let canonical_path = b"/proc/uptime".to_vec();
        let name = encode_carrier_name(authority_id, &canonical_path, true).unwrap();
        CarrierSnapshot {
            name,
            parsed: ParsedCarrierName {
                authority_id,
                canonical_path,
                virtual_nofollow: true,
            },
            handle: HandleIdentity {
                opaque: OpaqueHandle {
                    kind: 7,
                    bytes: vec![1, 2, 3, 4, 5],
                },
                legacy_mount_id: 0,
                unique_mount_id,
            },
            stat: StableStat {
                filesystem_type: TMPFS_MAGIC,
                device_major: 0,
                device_minor: 42,
                inode: 99,
                mode: libc::S_IFREG as u16 | 0o444,
                nlink: 0,
                uid: 1000,
                gid: 1001,
                rdev_major: 0,
                rdev_minor: 0,
                block_size: 4096,
                attributes: 0x20,
                size: 3,
                statx_mount_id: 0,
                statx_unique_mount_id,
            },
            seals: REQUIRED_SEALS,
            tag: 123,
        }
    }

    #[test]
    fn reserved_name_round_trip_and_prefix_are_exact() {
        let authority = [0xab; 16];
        for nofollow in [false, true] {
            let name = encode_carrier_name(authority, b"/proc/self/status", nofollow).unwrap();
            assert!(reserved_guest_name(&name));
            let mut target = MEMFD_LINK_PREFIX.to_vec();
            target.extend_from_slice(&name);
            target.extend_from_slice(MEMFD_LINK_SUFFIX);
            assert_eq!(
                parse_carrier_target(&target).unwrap(),
                ParsedCarrierName {
                    authority_id: authority,
                    canonical_path: b"/proc/self/status".to_vec(),
                    virtual_nofollow: nofollow,
                }
            );
        }
        assert!(reserved_guest_name(b"reverie-kvm.proc-carrier.v1"));
        assert!(reserved_guest_name(
            b"reverie-kvm.proc-carrier.v1-malformed"
        ));
        assert!(!reserved_guest_name(b"reverie-kvm.proc-carrier.v2"));

        let mut longest_path = b"/proc/".to_vec();
        while encode_carrier_name(authority, &[longest_path.as_slice(), b"a"].concat(), false)
            .is_ok()
        {
            longest_path.push(b'a');
        }
        let longest_name = encode_carrier_name(authority, &longest_path, false).unwrap();
        assert!(longest_name.len() <= 249);
        assert_eq!(
            encode_carrier_name(authority, &[longest_path.as_slice(), b"a"].concat(), false),
            Err(libc::ENAMETOOLONG)
        );
    }

    #[test]
    fn transcript_is_canonical_and_binds_every_included_field() {
        let base = test_snapshot(MountIdMode::FullUnique);
        let content = b"abc";
        let bytes = transcript(&base, content);
        assert_eq!(
            hex_bytes(&bytes),
            "726576657269652d6b766d2e70726f632d636172726965720000000100010000000000000010111111111111111111111111111111110002000000000000005e726576657269652d6b766d2e70726f632d636172726965722e76312e31313131313131313131313131313131313131313131313131313131313131312e70726f632e6e66312e3266373037323666363332663735373037343639366436350003000000000000000470726f630004000000000000000c2f70726f632f757074696d65000500000000000000040002000000060000000000000001020007000000000000000400000000000800000000000000080000000000000000000900000000000000080000000000000017000a00000000000000080000000000000017000b000000000000000400000005000c000000000000000400000007000d00000000000000050102030405000e00000000000000080000000001021994000f000000000000000400000000001000000000000000040000002a001100000000000000080000000000000063001200000000000000028124001300000000000000040000000000140000000000000004000003e800150000000000000004000003e9001600000000000000040000000000170000000000000004000000000018000000000000000400001000001900000000000000080000000000000020001a00000000000000040000000f001b00000000000000080000000000000003001c0000000000000003616263"
        );
        use sha2::Digest as _;
        assert_eq!(
            hex_bytes(&sha2::Sha256::digest(&bytes)),
            "3b1c0a80390a7f6bbea3b1f7ad68524c1a6296fd3624afcb742aa758331945fd"
        );

        let mutate = |mut snapshot: CarrierSnapshot, f: fn(&mut CarrierSnapshot)| {
            f(&mut snapshot);
            assert_ne!(transcript(&snapshot, content), bytes);
        };
        mutate(base.clone(), |value| value.parsed.authority_id[0] ^= 1);
        mutate(base.clone(), |value| value.name[0] ^= 1);
        mutate(base.clone(), |value| value.parsed.canonical_path.push(b'x'));
        mutate(base.clone(), |value| value.parsed.virtual_nofollow = false);
        mutate(base.clone(), |value| value.handle.legacy_mount_id = 1);
        mutate(base.clone(), |value| value.stat.statx_mount_id = 1);
        mutate(base.clone(), |value| {
            value.stat.statx_unique_mount_id = Some(24)
        });
        mutate(base.clone(), |value| {
            value.handle.unique_mount_id = Some(24)
        });
        mutate(base.clone(), |value| value.handle.opaque.kind += 1);
        mutate(base.clone(), |value| value.handle.opaque.bytes.push(6));
        mutate(base.clone(), |value| value.stat.filesystem_type += 1);
        mutate(base.clone(), |value| value.stat.device_major += 1);
        mutate(base.clone(), |value| value.stat.device_minor += 1);
        mutate(base.clone(), |value| value.stat.inode += 1);
        mutate(base.clone(), |value| value.stat.mode ^= 1);
        mutate(base.clone(), |value| value.stat.nlink += 1);
        mutate(base.clone(), |value| value.stat.uid += 1);
        mutate(base.clone(), |value| value.stat.gid += 1);
        mutate(base.clone(), |value| value.stat.rdev_major += 1);
        mutate(base.clone(), |value| value.stat.rdev_minor += 1);
        mutate(base.clone(), |value| value.stat.block_size += 1);
        mutate(base.clone(), |value| value.stat.attributes ^= 1);
        mutate(base.clone(), |value| value.seals ^= libc::F_SEAL_GROW);
        mutate(base.clone(), |value| value.stat.size += 1);
        assert_ne!(transcript(&base, b"abd"), bytes);

        let mut excluded = base.clone();
        excluded.tag += 1;
        assert_eq!(transcript(&excluded, content), bytes);
    }

    #[test]
    fn transcript_tlvs_have_unambiguous_big_endian_splits() {
        let bytes = transcript(&test_snapshot(MountIdMode::FullUnique), b"abc");
        let header_len = TRANSCRIPT_DOMAIN.len() + std::mem::size_of::<u32>();
        assert_eq!(&bytes[..TRANSCRIPT_DOMAIN.len()], TRANSCRIPT_DOMAIN);
        assert_eq!(
            u32::from_be_bytes(
                bytes[TRANSCRIPT_DOMAIN.len()..header_len]
                    .try_into()
                    .unwrap()
            ),
            TRANSCRIPT_VERSION
        );

        let mut cursor = header_len;
        let mut fields = Vec::new();
        while cursor < bytes.len() {
            let field = u16::from_be_bytes(bytes[cursor..cursor + 2].try_into().unwrap());
            let length = u64::from_be_bytes(bytes[cursor + 2..cursor + 10].try_into().unwrap());
            cursor += 10;
            let length = usize::try_from(length).unwrap();
            assert!(cursor + length <= bytes.len());
            fields.push(field);
            cursor += length;
        }
        assert_eq!(cursor, bytes.len());
        assert_eq!(
            fields,
            (FIELD_AUTHORITY..=FIELD_CONTENT).collect::<Vec<_>>()
        );

        let mut left = Vec::new();
        push_tlv(&mut left, 1, b"a");
        push_tlv(&mut left, 2, b"bc");
        let mut right = Vec::new();
        push_tlv(&mut right, 1, b"ab");
        push_tlv(&mut right, 2, b"c");
        assert_ne!(left, right);
        assert_eq!(&left[2..10], &1_u64.to_be_bytes());
    }

    #[test]
    fn statx_masks_padding_blocks_and_timestamps_are_excluded() {
        // Zero initialization makes all libc-owned padding deterministic; the
        // conversion below deliberately selects fields rather than copying the
        // ABI struct into the transcript.
        let mut first = unsafe { std::mem::zeroed::<libc::statx>() };
        first.stx_blksize = 4096;
        first.stx_attributes = 0x20;
        first.stx_nlink = 1;
        first.stx_uid = 1000;
        first.stx_gid = 1001;
        first.stx_mode = libc::S_IFREG as u16 | 0o444;
        first.stx_ino = 99;
        first.stx_size = 3;
        first.stx_dev_major = 0;
        first.stx_dev_minor = 42;
        first.stx_mnt_id = 7;

        let mut second = first;
        second.stx_mask = u32::MAX;
        second.stx_attributes_mask = u64::MAX;
        second.stx_blocks = u64::MAX;
        second.stx_atime.tv_sec = 1;
        second.stx_atime.tv_nsec = 2;
        second.stx_btime.tv_sec = 3;
        second.stx_btime.tv_nsec = 4;
        second.stx_ctime.tv_sec = 5;
        second.stx_ctime.tv_nsec = 6;
        second.stx_mtime.tv_sec = 7;
        second.stx_mtime.tv_nsec = 8;

        let first = stable_stat_from_statx(&first, TMPFS_MAGIC, Some(11));
        let second = stable_stat_from_statx(&second, TMPFS_MAGIC, Some(11));
        assert_eq!(first, second);
        let mut first_snapshot = test_snapshot(MountIdMode::FullUnique);
        first_snapshot.stat = first;
        let mut second_snapshot = first_snapshot.clone();
        second_snapshot.stat = second;
        assert_eq!(
            transcript(&first_snapshot, b"abc"),
            transcript(&second_snapshot, b"abc")
        );
    }

    #[test]
    fn rejection_sampling_uses_lower_rejection_floor_and_big_endian_counter() {
        let mut counters = Vec::new();
        let selected = rejection_sample_with(|counter| {
            counters.push(counter);
            if counter == 0 {
                TAG_REJECTION_FLOOR - 1
            } else {
                TAG_REJECTION_FLOOR
            }
        });
        assert_eq!(counters, [0, 1]);
        assert_eq!(selected, TAG_REJECTION_FLOOR % TAG_MODULUS);

        let transcript = transcript(&test_snapshot(MountIdMode::FullUnique), b"abc");
        let key = [0x5a; 32];
        let first = rejection_sample(&key, &transcript);
        let second = rejection_sample(&key, &transcript);
        assert_eq!(first, second);
        assert_eq!(first, 1_518_385_793_902_382_431_255_540_264);
        assert_eq!(
            hmac_sample(&key, &transcript, 0),
            16_866_835_130_687_520_824_129_042_134_255_540_264
        );
        assert_eq!(
            hmac_sample(&key, &transcript, 1),
            309_147_490_797_340_504_009_401_793_948_076_478_171
        );
        assert_eq!(
            u128::MAX % TAG_MODULUS + 1,
            TAG_REJECTION_FLOOR,
            "R must equal 2^128 modulo N"
        );
    }

    #[test]
    fn timestamp_encoding_covers_the_exact_range() {
        assert_eq!(tag_to_timestamp(0), Some((0, 0)));
        assert_eq!(
            tag_to_timestamp(TAG_MODULUS - 1),
            Some((i64::MAX - 1, 999_999_999))
        );
        assert_eq!(tag_to_timestamp(TAG_MODULUS), None);
        assert_eq!(timestamp_to_tag(0, 0), Ok(0));
        assert_eq!(
            timestamp_to_tag(i64::MAX - 1, 999_999_999),
            Ok(TAG_MODULUS - 1)
        );
        assert_eq!(timestamp_to_tag(-1, 0), Err(libc::EBADMSG));
        assert_eq!(timestamp_to_tag(i64::MAX, 0), Err(libc::EBADMSG));
        assert_eq!(timestamp_to_tag(0, 1_000_000_000), Err(libc::EBADMSG));
        assert_eq!(UTIME_OMIT_RAW, 1_073_741_822);
    }

    #[test]
    fn handle_sizes_are_exact_bounded_aligned_and_four_byte_multiples() {
        assert_eq!(MAX_HANDLE_SZ, 128);
        assert_eq!(HANDLE_ATTEMPTS, 4);
        assert!(validate_requested_handle_size(0, 4).is_ok());
        assert!(validate_requested_handle_size(4, 12).is_ok());
        assert!(validate_handle_size(12, 12).is_ok());
        assert!(validate_requested_handle_size(0, 0).is_err());
        assert!(validate_requested_handle_size(8, 8).is_err());
        assert!(validate_requested_handle_size(0, 1).is_err());
        assert!(validate_requested_handle_size(4, 13).is_err());
        assert!(validate_requested_handle_size(0, MAX_HANDLE_SZ + 4).is_err());
        assert!(validate_handle_size(16, 0).is_err());
        assert!(validate_handle_size(16, 13).is_err());
        assert!(validate_handle_size(12, 16).is_err());
        assert!(validate_handle_size(MAX_HANDLE_SZ + 4, MAX_HANDLE_SZ + 4).is_err());

        let storage = [0_u64; 4];
        assert_eq!(
            storage.as_ptr() as usize % std::mem::align_of::<libc::file_handle>(),
            0
        );
        assert_eq!(storage.as_ptr() as usize % std::mem::align_of::<u64>(), 0);
    }

    #[test]
    fn injected_mount_modes_are_monotonic_and_never_mixed() {
        assert_eq!(select_mount_mode(None, None).unwrap(), MountIdMode::Legacy);
        assert_eq!(
            select_mount_mode(Some(0), None).unwrap(),
            MountIdMode::StatxUnique
        );
        assert_eq!(
            select_mount_mode(Some(0), Some(0)).unwrap(),
            MountIdMode::FullUnique
        );
        assert!(select_mount_mode(None, Some(0)).is_err());
        assert_eq!(
            consistent_optional_ids(&[None, None], "test").unwrap(),
            None
        );
        assert_eq!(
            consistent_optional_ids(&[Some(7), Some(7)], "test").unwrap(),
            Some(7)
        );
        assert!(consistent_optional_ids(&[Some(7), None], "test").is_err());

        let basics = vec![
            BasicObservation {
                handle: test_snapshot(MountIdMode::Legacy).handle,
                stat: test_snapshot(MountIdMode::Legacy).stat,
                mtime_seconds: 0,
                mtime_nanoseconds: 0,
            };
            2
        ];
        assert_eq!(
            validate_unique_handles(&basics, &[None, None], None).unwrap(),
            None
        );
        let opaque = basics[0].handle.opaque.clone();
        assert_eq!(
            validate_unique_handles(
                &basics,
                &[Some((opaque.clone(), 9)), Some((opaque.clone(), 9))],
                Some(9),
            )
            .unwrap(),
            Some(9)
        );
        assert!(validate_unique_handles(&basics, &[Some((opaque, 9)), None], Some(9),).is_err());
        assert!(validate_unique_handles(&basics, &[None, None], Some(9)).is_ok());
    }

    #[test]
    fn candidate_classification_is_fail_closed_on_inspection_errors() {
        assert_eq!(
            candidate_kind_from_link(Ok(b"/memfd:ordinary (deleted)".to_vec())),
            Ok(ProcCarrierCandidate::Ordinary)
        );
        assert_eq!(
            candidate_kind_from_link(Ok(
                b"/memfd:reverie-kvm.proc-carrier.v1.malformed (deleted)".to_vec()
            )),
            Ok(ProcCarrierCandidate::Reserved)
        );
        assert_eq!(candidate_kind_from_link(Err(libc::EIO)), Err(libc::EIO));
        assert_eq!(
            candidate_kind_from_link(Err(libc::EOVERFLOW)),
            Err(libc::EOVERFLOW)
        );

        let link = [b'x'; LINK_TARGET_CAPACITY];
        assert_eq!(
            complete_link_target(&link, LINK_TARGET_CAPACITY - 1)
                .unwrap()
                .len(),
            266
        );
        assert_eq!(
            complete_link_target(&link, LINK_TARGET_CAPACITY)
                .unwrap_err()
                .raw_os_error(),
            Some(libc::EOVERFLOW)
        );
        assert_eq!(LINK_TARGET_CAPACITY, 267);
    }

    #[test]
    fn seal_profiles_are_exact() {
        assert!(accepted_seal_profile(REQUIRED_SEALS));
        assert!(accepted_seal_profile(ACCEPTED_SEALS_WITH_EXEC));
        for seals in [
            REQUIRED_SEALS & !libc::F_SEAL_WRITE,
            REQUIRED_SEALS | F_SEAL_FUTURE_WRITE,
            REQUIRED_SEALS | 0x40,
        ] {
            assert!(
                !accepted_seal_profile(seals),
                "unexpected seal mask {seals:#x}"
            );
        }
    }

    #[test]
    fn live_mint_authenticate_preserves_readable_and_opath_offsets() {
        let authority = live_authority();
        let mut readable = authority
            .mint(b"/proc/uptime", b"0.00 0.00\n", false, false, 0)
            .unwrap();
        let mut prefix = [0_u8; 2];
        readable.read_exact(&mut prefix).unwrap();
        assert_eq!(&prefix, b"0.");
        let before = readable.stream_position().unwrap();
        let inspection = open_inspection_alias(&readable).unwrap();
        let mut budget = CarrierAuthBudget::new();
        let mut cache = CarrierAuthCache::new();
        let authenticated = authority
            .authenticate_reserved(&readable, &inspection, &mut budget, &mut cache)
            .unwrap();
        assert_eq!(authenticated.canonical_path, b"/proc/uptime");
        assert_eq!(authenticated.content.as_ref(), b"0.00 0.00\n");
        assert_eq!(readable.stream_position().unwrap(), before);

        let path_only = authority
            .mint(b"/proc/uptime", b"0.00 0.00\n", true, true, 0)
            .unwrap();
        let inspection = open_inspection_alias(&path_only).unwrap();
        let authenticated = authority
            .authenticate_reserved(&path_only, &inspection, &mut budget, &mut cache)
            .unwrap();
        assert!(authenticated.virtual_nofollow);
    }

    #[test]
    fn live_fresh_objects_replay_and_new_authority_rejection() {
        let authority = live_authority();
        let other = live_authority();
        assert_ne!(authority.public_id(), other.public_id());
        let first = authority
            .mint(b"/proc/uptime", b"same", false, false, 0)
            .unwrap();
        let second = authority
            .mint(b"/proc/uptime", b"same", false, false, 0)
            .unwrap();
        let first_handle = file_handle(&first, false).unwrap().0;
        let second_handle = file_handle(&second, false).unwrap().0;
        assert_ne!(first_handle, second_handle);

        let wrong_inspection = open_inspection_alias(&second).unwrap();
        assert_eq!(
            authority.authenticate_reserved(
                &first,
                &wrong_inspection,
                &mut CarrierAuthBudget::new(),
                &mut CarrierAuthCache::new(),
            ),
            Err(libc::EBADMSG)
        );

        let mut budget = CarrierAuthBudget::new();
        let mut cache = CarrierAuthCache::new();
        for source in [&first, &first] {
            let inspection = open_inspection_alias(source).unwrap();
            assert!(
                authority
                    .authenticate_reserved(source, &inspection, &mut budget, &mut cache)
                    .is_ok()
            );
        }
        let inspection = open_inspection_alias(&first).unwrap();
        assert_eq!(
            other.authenticate_reserved(&first, &inspection, &mut budget, &mut cache),
            Err(libc::EBADMSG)
        );
    }

    #[test]
    fn nonreserved_same_content_is_ordinary_and_reserved_tamper_fails() {
        let authority = live_authority();
        let carrier = authority
            .mint(b"/proc/uptime", b"payload", false, false, 0)
            .unwrap();
        assert_eq!(candidate_kind(&carrier), Ok(ProcCarrierCandidate::Reserved));

        let ordinary = test_memfd(b"ordinary", b"payload");
        // SAFETY: ordinary owns a live descriptor and the mode has permission bits only.
        assert_eq!(unsafe { libc::fchmod(ordinary.as_raw_fd(), 0o444) }, 0);
        // SAFETY: ordinary is a sealable memfd and the exact required mask is valid.
        assert_eq!(
            unsafe { libc::fcntl(ordinary.as_raw_fd(), libc::F_ADD_SEALS, REQUIRED_SEALS) },
            0
        );
        assert_eq!(get_seals(&ordinary), Ok(authority.selected_seals()));
        assert_eq!(
            candidate_kind(&ordinary),
            Ok(ProcCarrierCandidate::Ordinary)
        );

        let times = [
            libc::timespec {
                tv_sec: 0,
                tv_nsec: UTIME_OMIT_RAW,
            },
            libc::timespec {
                tv_sec: 1,
                tv_nsec: 2,
            },
        ];
        assert_eq!(
            unsafe { libc::futimens(carrier.as_raw_fd(), times.as_ptr()) },
            0
        );
        let inspection = open_inspection_alias(&carrier).unwrap();
        assert_eq!(
            authority.authenticate_reserved(
                &carrier,
                &inspection,
                &mut CarrierAuthBudget::new(),
                &mut CarrierAuthCache::new(),
            ),
            Err(libc::EBADMSG)
        );
    }

    #[test]
    fn malformed_reserved_name_and_stable_field_mutation_are_ebadmsg() {
        let authority = live_authority();
        let malformed = test_memfd(
            b"reverie-kvm.proc-carrier.v1.not-a-valid-carrier",
            b"payload",
        );
        assert_eq!(
            candidate_kind(&malformed),
            Ok(ProcCarrierCandidate::Reserved)
        );
        let malformed_inspection = open_inspection_alias(&malformed).unwrap();
        assert_eq!(
            authority.authenticate_reserved(
                &malformed,
                &malformed_inspection,
                &mut CarrierAuthBudget::new(),
                &mut CarrierAuthCache::new(),
            ),
            Err(libc::EBADMSG)
        );

        let carrier = authority
            .mint(b"/proc/uptime", b"payload", false, false, 0)
            .unwrap();
        // SAFETY: carrier is live; seals protect bytes, not inode mode metadata.
        assert_eq!(unsafe { libc::fchmod(carrier.as_raw_fd(), 0o400) }, 0);
        let inspection = open_inspection_alias(&carrier).unwrap();
        assert_eq!(
            authority.authenticate_reserved(
                &carrier,
                &inspection,
                &mut CarrierAuthBudget::new(),
                &mut CarrierAuthCache::new(),
            ),
            Err(libc::EBADMSG)
        );
    }

    #[test]
    fn metadata_race_after_content_read_is_rejected() {
        let authority = live_authority();
        let carrier = authority
            .mint(b"/proc/uptime", b"payload", false, false, 0)
            .unwrap();
        let observed = observe_basic(&carrier).unwrap();
        let old_tag = timestamp_to_tag(observed.mtime_seconds, observed.mtime_nanoseconds).unwrap();
        let replacement = if old_tag + 1 == TAG_MODULUS {
            0
        } else {
            old_tag + 1
        };
        let inspection = open_inspection_alias(&carrier).unwrap();
        let result = authority.authenticate_reserved_with_hook(
            &carrier,
            &inspection,
            &mut CarrierAuthBudget::new(),
            &mut CarrierAuthCache::new(),
            || set_mtime_tag(&carrier, replacement).unwrap(),
        );
        assert_eq!(result, Err(libc::EBADMSG));
    }

    #[test]
    fn minted_bytes_and_seal_set_cannot_be_mutated() {
        let authority = live_authority();
        let carrier = authority
            .mint(b"/proc/uptime", b"payload", false, false, 0)
            .unwrap();
        assert_eq!(get_seals(&carrier), Ok(authority.selected_seals()));
        // Seals do not freeze mode metadata. Restore owner write permission so
        // the test can acquire a writable description and exercise each seal.
        // SAFETY: carrier is live and the mode has permission bits only.
        assert_eq!(unsafe { libc::fchmod(carrier.as_raw_fd(), 0o600) }, 0);
        let writable = open_proc_fd_alias(&carrier, libc::O_RDWR | libc::O_CLOEXEC).unwrap();

        let byte = *b"x";
        // SAFETY: the pointer is valid for one byte and writable owns a live descriptor.
        assert_eq!(
            unsafe { libc::pwrite(writable.as_raw_fd(), byte.as_ptr().cast(), 1, 0) },
            -1
        );
        assert_eq!(last_errno(), libc::EPERM);
        // SAFETY: writable owns a live descriptor.
        assert_eq!(unsafe { libc::ftruncate(writable.as_raw_fd(), 0) }, -1);
        assert_eq!(last_errno(), libc::EPERM);
        // SAFETY: all arguments describe a one-byte shared mapping of a live file.
        let mapping = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                1,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_SHARED,
                writable.as_raw_fd(),
                0,
            )
        };
        assert_eq!(mapping, libc::MAP_FAILED);
        assert_eq!(last_errno(), libc::EPERM);
        // SAFETY: writable is a sealable memfd; F_SEAL_SEAL must reject additions.
        assert_eq!(
            unsafe { libc::fcntl(writable.as_raw_fd(), libc::F_ADD_SEALS, F_SEAL_FUTURE_WRITE,) },
            -1
        );
        assert_eq!(last_errno(), libc::EPERM);
    }

    #[test]
    fn exact_positioned_read_checks_eof_without_moving_offset() {
        let mut file = test_memfd(b"positioned-read", b"abc");
        file.seek(SeekFrom::Start(2)).unwrap();
        assert_eq!(read_exact_content(&file, 3).unwrap(), b"abc");
        assert_eq!(file.stream_position().unwrap(), 2);
        assert_eq!(read_exact_content(&file, 2), Err(libc::EBADMSG));
        assert_eq!(read_exact_content(&file, 4), Err(libc::EBADMSG));
        assert_eq!(file.stream_position().unwrap(), 2);
    }

    #[test]
    fn batch_budget_is_cumulative_but_cache_can_be_reset_per_message() {
        let mut budget = CarrierAuthBudget::with_limit(5);
        assert_eq!(budget.charge(3), Ok(()));
        assert_eq!(budget.remaining, 2);
        assert_eq!(budget.charge(3), Err(libc::EFBIG));
        assert_eq!(budget.remaining, 2);

        let authority = live_authority();
        let carrier = authority
            .mint(b"/proc/uptime", b"abc", false, false, 0)
            .unwrap();
        let inspection = open_inspection_alias(&carrier).unwrap();
        let mut budget = CarrierAuthBudget::with_limit(3);
        let mut cache = CarrierAuthCache::new();
        authority
            .authenticate_reserved(&carrier, &inspection, &mut budget, &mut cache)
            .unwrap();
        assert_eq!(budget.remaining, 0);
        authority
            .authenticate_reserved(&carrier, &inspection, &mut budget, &mut cache)
            .unwrap();
        assert_eq!(budget.remaining, 0, "a cache hit must not charge bytes");
        cache.clear();
        assert_eq!(
            authority.authenticate_reserved(&carrier, &inspection, &mut budget, &mut cache),
            Err(libc::EFBIG)
        );
    }

    #[test]
    fn authority_debug_redacts_secret() {
        let authority = live_authority();
        let output = format!("{authority:?}");
        assert!(output.contains("<redacted>"));
        assert!(!output.contains(&hex_bytes(&authority.key)));
        assert!(accepted_seal_profile(authority.selected_seals()));
    }

    #[test]
    fn startup_failures_remain_typed() {
        match ProbeFailure::message("test phase", "test reason").into_public() {
            crate::Error::ProcCarrierUnsupported { phase, reason } => {
                assert_eq!(phase, "test phase");
                assert_eq!(reason, "test reason");
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    fn hex_bytes(bytes: &[u8]) -> String {
        let mut output = Vec::with_capacity(bytes.len() * 2);
        append_hex(&mut output, bytes);
        String::from_utf8(output).unwrap()
    }
}
