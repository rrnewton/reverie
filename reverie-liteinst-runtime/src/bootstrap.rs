//! Shared sealed bootstrap schema and transport for the runtime and coordinator.

use std::ffi::OsString;
use std::io;
use std::mem::MaybeUninit;
use std::os::fd::FromRawFd;
use std::os::fd::OwnedFd;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::ffi::OsStringExt;
use std::path::Path;
use std::path::PathBuf;

/// Environment variable naming the optional, stats-only coordinator socket.
pub const STATS_COORDINATOR_ENV: &str = "REVERIE_LITEINST_STATS_COORDINATOR";

pub const PRELOAD_BOOTSTRAP_MAGIC: &[u8; 16] = b"REVERIE-LI-V1\0\0\0";
pub const LOG_BOOTSTRAP_MAGIC: &[u8; 16] = b"REVERIE-LI-V2\0\0\0";
pub const BUFFERED_LOG_BOOTSTRAP_MAGIC: &[u8; 16] = b"REVERIE-LI-V3\0\0\0";
pub const ORDERED_LOG_BOOTSTRAP_MAGIC: &[u8; 16] = b"REVERIE-LI-V4\0\0\0";
pub const PRELOAD_BOOTSTRAP_HEADER_BYTES: usize = PRELOAD_BOOTSTRAP_MAGIC.len() + 4;
pub const PRELOAD_BOOTSTRAP_MAX_BYTES: usize = 4096;

// TODO-HUMAN-REVIEW(PR-139): Review the public inherited preload bootstrap contract.
/// Coordinator path and tool-specific bytes consumed by a preload constructor.
pub struct PreloadBootstrap {
    /// Unix-domain socket path for the generic tool coordinator.
    pub coordinator: PathBuf,
    /// Opaque bytes supplied by the tool-specific coordinator launcher.
    pub tool_data: Vec<u8>,
    /// Optional protected logging channel authenticated by the sealed bootstrap.
    pub log: Option<crate::GuestLog>,
}

// TODO-HUMAN-REVIEW(PR-139): Review the public inherited preload bootstrap consumer.
/// Consumes the inherited generic-tool bootstrap, if one is present.
///
/// # Safety
///
/// Call only from a preload constructor launched by LiteinstBackend; this scans
/// inherited descriptors and consumes only a sealed, protocol-matching memfd.
pub unsafe fn take_preload_bootstrap() -> io::Result<Option<PreloadBootstrap>> {
    let mut found = Vec::new();
    for entry in std::fs::read_dir("/proc/self/fd")? {
        let entry = entry?;
        let Some(fd) = entry
            .file_name()
            .to_str()
            .and_then(|name| name.parse::<libc::c_int>().ok())
        else {
            continue;
        };
        if fd <= libc::STDERR_FILENO {
            continue;
        }
        match read_preload_bootstrap(fd) {
            Ok(Some(bootstrap)) => {
                found.push((unsafe { OwnedFd::from_raw_fd(fd) }, bootstrap));
            }
            Ok(None) => {}
            Err(error) => {
                let _matching_fd = unsafe { OwnedFd::from_raw_fd(fd) };
                return Err(error);
            }
        }
    }
    match found.len() {
        0 => Ok(None),
        1 => {
            let (_fd, bootstrap) = found.pop().unwrap();
            Ok(Some(bootstrap))
        }
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "multiple LiteInst preload bootstraps",
        )),
    }
}

pub fn read_preload_bootstrap(fd: libc::c_int) -> io::Result<Option<PreloadBootstrap>> {
    let required_seals =
        libc::F_SEAL_SEAL | libc::F_SEAL_SHRINK | libc::F_SEAL_GROW | libc::F_SEAL_WRITE;
    let seals = unsafe { libc::fcntl(fd, libc::F_GET_SEALS) };
    if seals == -1 || seals & required_seals != required_seals {
        return Ok(None);
    }

    let mut stat = MaybeUninit::<libc::stat>::uninit();
    if unsafe { libc::fstat(fd, stat.as_mut_ptr()) } == -1 {
        return Ok(None);
    }
    let size = match usize::try_from(unsafe { stat.assume_init() }.st_size) {
        Ok(size)
            if (PRELOAD_BOOTSTRAP_HEADER_BYTES..=PRELOAD_BOOTSTRAP_MAX_BYTES).contains(&size) =>
        {
            size
        }
        _ => return Ok(None),
    };
    let mut packet = vec![0_u8; size];
    let read = unsafe { libc::pread(fd, packet.as_mut_ptr().cast(), packet.len(), 0) };
    let magic = packet.get(..PRELOAD_BOOTSTRAP_MAGIC.len());
    let ordered = magic == Some(ORDERED_LOG_BOOTSTRAP_MAGIC);
    let buffered = magic == Some(BUFFERED_LOG_BOOTSTRAP_MAGIC) || ordered;
    let logged = magic == Some(LOG_BOOTSTRAP_MAGIC) || buffered;
    if read != size as isize || (!logged && magic != Some(PRELOAD_BOOTSTRAP_MAGIC)) {
        return Ok(None);
    }

    let lengths = &packet[PRELOAD_BOOTSTRAP_MAGIC.len()..PRELOAD_BOOTSTRAP_HEADER_BYTES];
    let path_len = u16::from_le_bytes([lengths[0], lengths[1]]) as usize;
    let data_len = u16::from_le_bytes([lengths[2], lengths[3]]) as usize;
    let log_bytes = if logged {
        crate::guest_log::IDENTITY_BYTES
    } else {
        0
    };
    if packet.len() != PRELOAD_BOOTSTRAP_HEADER_BYTES + path_len + data_len + log_bytes
        || path_len == 0
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid LiteInst preload bootstrap lengths",
        ));
    }
    let path_end = PRELOAD_BOOTSTRAP_HEADER_BYTES + path_len;
    Ok(Some(PreloadBootstrap {
        coordinator: PathBuf::from(OsString::from_vec(
            packet[PRELOAD_BOOTSTRAP_HEADER_BYTES..path_end].to_vec(),
        )),
        tool_data: packet[path_end..path_end + data_len].to_vec(),
        log: if ordered {
            Some(unsafe {
                crate::guest_log::from_ordered_identity(&packet[path_end + data_len..])
            }?)
        } else if buffered {
            Some(unsafe {
                crate::guest_log::from_buffered_identity(&packet[path_end + data_len..])
            }?)
        } else if logged {
            Some(unsafe { crate::guest_log::from_identity(&packet[path_end + data_len..]) }?)
        } else {
            None
        },
    }))
}

pub fn create_preload_bootstrap(coordinator: &Path, tool_data: &[u8]) -> io::Result<OwnedFd> {
    create_logged_preload_bootstrap(coordinator, tool_data, None)
}

pub fn create_logged_preload_bootstrap(
    coordinator: &Path,
    tool_data: &[u8],
    log_fd: Option<i32>,
) -> io::Result<OwnedFd> {
    create_log_bootstrap(coordinator, tool_data, log_fd, false)
}

pub fn create_log_bootstrap(
    coordinator: &Path,
    tool_data: &[u8],
    log_fd: Option<i32>,
    buffered: bool,
) -> io::Result<OwnedFd> {
    create_versioned_log_bootstrap(coordinator, tool_data, log_fd, buffered, false)
}

pub fn create_versioned_log_bootstrap(
    coordinator: &Path,
    tool_data: &[u8],
    log_fd: Option<i32>,
    buffered: bool,
    ordered: bool,
) -> io::Result<OwnedFd> {
    if ordered && (!buffered || log_fd.is_none()) {
        return Err(io::Error::other(
            "ordered capture requires a buffered log endpoint",
        ));
    }
    let path = coordinator.as_os_str().as_bytes();
    let path_len = u16::try_from(path.len()).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "LiteInst coordinator path exceeds the bootstrap limit",
        )
    })?;
    let data_len = u16::try_from(tool_data.len()).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "LiteInst tool bootstrap data exceeds the bootstrap limit",
        )
    })?;
    let mut packet =
        Vec::with_capacity(PRELOAD_BOOTSTRAP_HEADER_BYTES + path.len() + tool_data.len());
    packet.extend_from_slice(if ordered {
        ORDERED_LOG_BOOTSTRAP_MAGIC
    } else if buffered {
        BUFFERED_LOG_BOOTSTRAP_MAGIC
    } else if log_fd.is_some() {
        LOG_BOOTSTRAP_MAGIC
    } else {
        PRELOAD_BOOTSTRAP_MAGIC
    });
    packet.extend_from_slice(&path_len.to_le_bytes());
    packet.extend_from_slice(&data_len.to_le_bytes());
    packet.extend_from_slice(path);
    packet.extend_from_slice(tool_data);
    if let Some(fd) = log_fd {
        packet.extend_from_slice(&crate::guest_log::identity(fd)?);
    }
    if packet.len() > PRELOAD_BOOTSTRAP_MAX_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "LiteInst preload bootstrap exceeds its size limit",
        ));
    }

    reverie::process::sealed::create(
        c"reverie-liteinst-bootstrap",
        &packet,
        if buffered { libc::STDERR_FILENO + 1 } else { 0 },
    )
}
