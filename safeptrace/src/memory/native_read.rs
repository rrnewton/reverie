/* Copyright (c) Meta Platforms, Inc. and affiliates. All rights reserved.
 * Licensed under the BSD-style license in the root LICENSE file. */

//! Synchronous stopped-task source access. Scheduling/mapping/data exclusion is
//! a caller prerequisite, not authority supplied by these observations. Only
//! ordinary private anonymous backing is admitted; this does not bound the
//! elapsed time of smaps or remote GUP.

use std::collections::BTreeSet;
use std::ffi::CString;
use std::fs::File;
use std::fs::OpenOptions;
use std::io::Read;
use std::os::fd::AsFd;
use std::os::fd::AsRawFd;
use std::os::fd::BorrowedFd;
use std::os::fd::FromRawFd;
#[cfg(test)]
use std::os::unix::fs::MetadataExt;
use std::os::unix::fs::OpenOptionsExt;

use reverie_memory::NativeUserReadError as Error;
use reverie_memory::NativeUserReadFault as Fault;
use reverie_memory::NativeUserReadRefusal as Refusal;
use syscalls::Errno;

use super::Stopped;
use super::decode_native_pkru_xstate;
use super::native_pkru_layout;

// Separate proc-mem transport. This never replaces the synchronous ordinary
// process_vm reader below. Its only production constructor consumes the actual
// notifier SourceAcquisition; the mechanical helpers grant no task authority.
#[cfg(any(test, feature = "notifier"))]
#[path = "native_mm_read.rs"]
mod mm_bound;

#[cfg(feature = "notifier")]
pub use mm_bound::FollowedSourceReadPlan;
#[cfg(feature = "notifier")]
pub use mm_bound::NativeSourceReadPlan;

const PAGE: usize = 4096;
const MAX_READ: usize = 512;
const MAX_SMAPS: usize = 1024 * 1024;
const MAX_STATUS: usize = 16 * 1024;
const PRSTATUS_BYTES: usize = std::mem::size_of::<libc::user_regs_struct>();
const PRSTATUS_CS: usize = std::mem::offset_of!(libc::user_regs_struct, cs);

fn refused(reason: Refusal) -> Error {
    Error::Refused(reason)
}

fn metadata_error() -> Error {
    refused(Refusal::MappingMetadata)
}

fn proc_error(error: std::io::Error) -> Error {
    refused(Refusal::Procfs(Errno::new(
        error.raw_os_error().unwrap_or(libc::EIO),
    )))
}

fn target_error(error: crate::Error) -> Error {
    refused(Refusal::TargetState(match error {
        crate::Error::Errno(error) => error,
        crate::Error::Died(_) => Errno::ESRCH,
    }))
}

fn checked_native_mode(
    getregset: impl FnOnce(&mut [u8; PRSTATUS_BYTES]) -> Result<usize, Errno>,
) -> Result<(), Error> {
    // GETREGSET selects its ABI from the actual target mode. In particular,
    // compat PRSTATUS can be shorter than the tracer's native register layout.
    // Never construct a typed register value from that partially filled reply.
    let mut bytes = [0u8; PRSTATUS_BYTES];
    let returned = getregset(&mut bytes).map_err(|e| refused(Refusal::TargetState(e)))?;
    if returned != PRSTATUS_BYTES {
        return Err(refused(Refusal::RegisterShape(returned)));
    }
    // Only a complete native shape can make this offset meaningful. Initialized
    // tail bytes alone are not evidence that the kernel returned those fields.
    let cs = u64::from_ne_bytes(bytes[PRSTATUS_CS..PRSTATUS_CS + 8].try_into().unwrap());
    if cs != 0x33 {
        return Err(refused(Refusal::UnsupportedPlatform));
    }
    Ok(())
}

fn validate_native_mode(target: &Stopped) -> Result<(), Error> {
    checked_native_mode(|bytes| {
        let mut iov = libc::iovec {
            iov_base: bytes.as_mut_ptr().cast(),
            iov_len: bytes.len(),
        };
        // Keep the actual TID and calling-ptracer-thread kernel check. This is
        // register observation, never debugger access to the source operand.
        unsafe {
            syscalls::syscall!(
                syscalls::Sysno::ptrace,
                libc::PTRACE_GETREGSET,
                target.pid().as_raw(),
                libc::NT_PRSTATUS,
                &mut iov as *mut _
            )
        }?;
        Ok(iov.iov_len)
    })
}

fn range_end(address: usize, length: usize) -> Result<usize, Error> {
    let end = address
        .checked_add(length)
        .ok_or_else(|| refused(Refusal::UnsupportedRange))?;
    // Deliberately excludes tagged, upper-canonical and five-level-only
    // addresses; no attempt to emulate the target's LAM configuration.
    if length == 0
        || length > MAX_READ
        || end > (1usize << 47)
        || address / PAGE != (end - 1) / PAGE
    {
        return Err(refused(Refusal::UnsupportedRange));
    }
    Ok(end)
}

#[derive(Debug)]
struct Mapping<'a> {
    start: usize,
    end: usize,
    permissions: &'a [u8],
    offset: usize,
    device: (usize, usize),
    inode: usize,
    key: Option<u8>,
    kernel_page: Option<usize>,
    mmu_page: Option<usize>,
    lazy_free: Option<usize>,
    flags: Option<BTreeSet<&'a str>>,
}

fn hex(value: &str) -> Result<usize, Error> {
    if value.is_empty() || !value.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(metadata_error());
    }
    usize::from_str_radix(value, 16).map_err(|_| metadata_error())
}

fn decimal(value: &str) -> Result<usize, Error> {
    if value.is_empty() || !value.bytes().all(|b| b.is_ascii_digit()) {
        return Err(metadata_error());
    }
    value.parse().map_err(|_| metadata_error())
}

fn header(line: &str) -> Result<Mapping<'_>, Error> {
    let mut fields = line.split_ascii_whitespace();
    let (start, end) = fields
        .next()
        .and_then(|s| s.split_once('-'))
        .ok_or_else(metadata_error)?;
    let start = hex(start)?;
    let end = hex(end)?;
    let permissions = fields.next().ok_or_else(metadata_error)?.as_bytes();
    if start >= end
        || start % PAGE != 0
        || end % PAGE != 0
        || permissions.len() != 4
        || !matches!(permissions[0], b'r' | b'-')
        || !matches!(permissions[1], b'w' | b'-')
        || !matches!(permissions[2], b'x' | b'-')
        || !matches!(permissions[3], b'p' | b's')
    {
        return Err(metadata_error());
    }
    let offset = hex(fields.next().ok_or_else(metadata_error)?)?;
    let (major, minor) = fields
        .next()
        .and_then(|s| s.split_once(':'))
        .ok_or_else(metadata_error)?;
    let device = (hex(major)?, hex(minor)?);
    let inode = decimal(fields.next().ok_or_else(metadata_error)?)?;
    // Remaining bytes name the mapping. No pathname grants access authority.
    Ok(Mapping {
        start,
        end,
        permissions,
        offset,
        device,
        inode,
        key: None,
        kernel_page: None,
        mmu_page: None,
        lazy_free: None,
        flags: None,
    })
}

fn page_field(value: &str) -> Result<usize, Error> {
    let mut fields = value.split_ascii_whitespace();
    let size = decimal(fields.next().ok_or_else(metadata_error)?)?;
    if fields.next() != Some("kB") || fields.next().is_some() || size == 0 {
        return Err(metadata_error());
    }
    size.checked_mul(1024).ok_or_else(metadata_error)
}

impl<'a> Mapping<'a> {
    fn field(&mut self, line: &'a str) -> Result<(), Error> {
        // VmFlags is the producer's last line. A following field without a new
        // header, including duplicate VmFlags, is not a complete smaps record.
        if self.flags.is_some() {
            return Err(metadata_error());
        }
        let (name, value) = line.split_once(':').ok_or_else(metadata_error)?;
        match name {
            "ProtectionKey" => {
                let key = decimal(value.trim_ascii())?;
                if key > 15 || self.key.replace(key as u8).is_some() {
                    return Err(metadata_error());
                }
            }
            "KernelPageSize" => {
                if self.kernel_page.replace(page_field(value)?).is_some() {
                    return Err(metadata_error());
                }
            }
            "MMUPageSize" => {
                if self.mmu_page.replace(page_field(value)?).is_some() {
                    return Err(metadata_error());
                }
            }
            "LazyFree" => {
                let bytes = if value.split_ascii_whitespace().eq(["0", "kB"]) {
                    0
                } else {
                    page_field(value)?
                };
                if self.lazy_free.replace(bytes).is_some() {
                    return Err(metadata_error());
                }
            }
            "VmFlags" => {
                let mut flags = BTreeSet::new();
                for flag in value.split_ascii_whitespace() {
                    if flag.len() != 2 || !flags.insert(flag) {
                        return Err(metadata_error());
                    }
                }
                self.flags = Some(flags);
            }
            _ => {
                if name.is_empty()
                    || !name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
                    || value.trim_ascii().is_empty()
                {
                    return Err(metadata_error());
                }
            }
        }
        Ok(())
    }

    fn complete(&self) -> Result<(), Error> {
        let flags = self.flags.as_ref().ok_or_else(metadata_error)?;
        if self.key.is_none() || self.kernel_page.is_none() || self.mmu_page.is_none()
            || self.lazy_free.is_none()
            || flags.contains("rd") != (self.permissions[0] == b'r')
            || flags.contains("wr") != (self.permissions[1] == b'w')
            || flags.contains("ex") != (self.permissions[2] == b'x')
            // Header 's' is VM_MAYSHARE, not VM_SHARED (fs/proc/task_mmu.c).
            || flags.contains("ms") != (self.permissions[3] == b's')
        {
            return Err(metadata_error());
        }
        Ok(())
    }

    fn private_backing(&self) -> Result<(), Error> {
        let flags = self.flags.as_ref().ok_or_else(metadata_error)?;
        // fs/proc/task_mmu.c reports file backing even after private COW.
        // VM_MAYSHARE controls header 's'; independently exclude VM_SHARED.
        // __install_special_mapping adds VM_DONTEXPAND, including the vDSO.
        // Other special/unknown flags are excluded by read_access below.
        //
        // These fields constrain backing, not its global immutability. The
        // caller must still exclude other MM users, remote/kernel writers and
        // mapping changes throughout this read. Neither a pathname nor a
        // metadata lock establishes that history/authority.
        if self.permissions[3] != b'p'
            || self.offset != 0
            || self.device != (0, 0)
            || self.inode != 0
            || flags.contains("sh")
            || flags.contains("ms")
            || flags.contains("de")
            || flags.contains("mg")
            // MADV_FREE permits reclaim to discard anonymous data while the
            // task is stopped. Reject observed debt anywhere in this VMA.
            // Zero does not prove no work remains queued; original discard
            // history and stable-source exclusion remain caller prerequisites.
            || self.lazy_free != Some(0)
        {
            return Err(refused(Refusal::UnsupportedBacking));
        }
        Ok(())
    }

    fn read_access(&self, pkru: u32) -> Result<(), Error> {
        self.complete()?;
        if self.kernel_page != Some(PAGE) || self.mmu_page != Some(PAGE) {
            return Err(refused(Refusal::UnsupportedMapping));
        }
        for flag in self.flags.as_ref().unwrap() {
            // Explicitly exclude io/pf/mm, userfaultfd, hugetlb, shadow stack,
            // guard/drop/arch-special VMAs and unknown flags before any fault
            // classification. A covered ordinary grow-down stack is allowed;
            // an address below its existing VMA is never inferred accessible.
            if !matches!(
                *flag,
                "rd" | "wr"
                    | "ex"
                    | "sh"
                    | "mr"
                    | "mw"
                    | "me"
                    | "ms"
                    | "gd"
                    | "lo"
                    | "lf"
                    | "sr"
                    | "rr"
                    | "dc"
                    | "de"
                    | "ac"
                    | "nr"
                    | "wf"
                    | "dd"
                    | "sd"
                    | "hg"
                    | "nh"
                    | "mg"
                    | "sl"
            ) {
                return Err(refused(Refusal::UnsupportedMapping));
            }
        }
        self.private_backing()?;
        if self.permissions[0] != b'r' {
            if self.permissions[1] == b'w' || self.permissions[2] == b'x' {
                // Hardware can allow these reads while GUP refuses VM_READ.
                // Neither that discrepancy nor execute-only fallback is EFAULT proof.
                return Err(refused(Refusal::UnsupportedMapping));
            }
            return Err(Error::Fault(Fault::NoAccessMapping));
        }
        let key = self.key.unwrap();
        // Linux __pkru_allows_read: AD denies read; WD alone does not.
        if pkru & (1u32 << (2 * key)) != 0 {
            return Err(Error::Fault(Fault::ProtectionKey(key)));
        }
        Ok(())
    }
}

fn mapping(bytes: &[u8], address: usize, end: usize) -> Result<Mapping<'_>, Error> {
    if bytes.len() > MAX_SMAPS {
        return Err(refused(Refusal::MetadataTooLarge));
    }
    if bytes.is_empty() || !bytes.ends_with(b"\n") {
        return Err(metadata_error());
    }
    let text = std::str::from_utf8(bytes).map_err(|_| metadata_error())?;
    let mut current: Option<Mapping<'_>> = None;
    let mut selected = None;
    let mut previous_end = 0;
    for line in text.lines() {
        let first = line
            .split_ascii_whitespace()
            .next()
            .ok_or_else(metadata_error)?;
        if first.contains('-') {
            if let Some(old) = current.take() {
                old.complete()?;
                if old.start <= address && end <= old.end {
                    selected = Some(old);
                }
            }
            let next = header(line)?;
            if next.start < previous_end {
                return Err(metadata_error());
            }
            previous_end = next.end;
            current = Some(next);
        } else {
            current.as_mut().ok_or_else(metadata_error)?.field(line)?;
        }
    }
    if let Some(old) = current {
        old.complete()?;
        if old.start <= address && end <= old.end {
            selected = Some(old);
        }
    }
    selected.ok_or_else(|| refused(Refusal::MappingMissing))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ProcMount {
    device: u64,
    mount_id: u64,
}

fn verify_proc(file: &File, expected: Option<ProcMount>) -> Result<ProcMount, Error> {
    verify_proc_fd(file.as_fd(), expected)
}

// Share the same procfs/mount checks with the MM transport without duplicating
// or reopening the backend's borrowed original task-directory descriptor.
fn verify_proc_fd(file: BorrowedFd<'_>, expected: Option<ProcMount>) -> Result<ProcMount, Error> {
    let mut fs = std::mem::MaybeUninit::<libc::statfs>::uninit();
    Errno::result(unsafe { libc::fstatfs(file.as_raw_fd(), fs.as_mut_ptr()) })
        .map_err(|e| refused(Refusal::Procfs(e)))?;
    let fs = unsafe { fs.assume_init() };
    let mut metadata = std::mem::MaybeUninit::<libc::stat>::uninit();
    Errno::result(unsafe { libc::fstat(file.as_raw_fd(), metadata.as_mut_ptr()) })
        .map_err(|e| refused(Refusal::Procfs(e)))?;
    let device = unsafe { metadata.assume_init() }.st_dev;
    // A same-procfs bind mount of another task's smaps has the same st_dev.
    // Require the held proc root's mount as well, so path components cannot
    // substitute a foreign proc file while still passing the filesystem test.
    let mut stat = std::mem::MaybeUninit::<libc::statx>::uninit();
    Errno::result(unsafe {
        libc::statx(
            file.as_raw_fd(),
            c"".as_ptr(),
            libc::AT_EMPTY_PATH | libc::AT_SYMLINK_NOFOLLOW,
            libc::STATX_MNT_ID,
            stat.as_mut_ptr(),
        )
    })
    .map_err(|e| refused(Refusal::Procfs(e)))?;
    let stat = unsafe { stat.assume_init() };
    let actual = ProcMount {
        device,
        mount_id: stat.stx_mnt_id,
    };
    if fs.f_type != libc::PROC_SUPER_MAGIC as _
        || stat.stx_mask & libc::STATX_MNT_ID == 0
        || actual.mount_id == 0
        || expected.is_some_and(|e| e != actual)
    {
        return Err(refused(Refusal::ProcfsViewMismatch));
    }
    Ok(actual)
}

fn proc_file(root: &File, mount: ProcMount, name: &str, limit: usize) -> Result<Vec<u8>, Error> {
    let name = CString::new(name).map_err(|_| metadata_error())?;
    let fd = Errno::result(unsafe {
        libc::openat(
            root.as_raw_fd(),
            name.as_ptr(),
            libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
        )
    })
    .map_err(|e| refused(Refusal::Procfs(e)))?;
    let file = unsafe { File::from_raw_fd(fd) };
    verify_proc(&file, Some(mount))?;
    let mut bytes = Vec::new();
    file.take((limit + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(proc_error)?;
    if bytes.len() > limit {
        return Err(refused(Refusal::MetadataTooLarge));
    }
    Ok(bytes)
}

fn proc_view(status: &[u8], tid: usize) -> Result<(), Error> {
    let invalid = || refused(Refusal::ProcfsViewMismatch);
    if !status.ends_with(b"\n") {
        return Err(invalid());
    }
    let text = std::str::from_utf8(status).map_err(|_| invalid())?;
    let mut pid = None;
    let mut nspid = None;
    for line in text.lines() {
        for (prefix, slot) in [("Pid:", &mut pid), ("NSpid:", &mut nspid)] {
            if let Some(value) = line.strip_prefix(prefix) {
                // One NSpid entry proves the procfs instance's namespace level
                // equals this actual caller's active PID namespace level.
                let value = decimal(value.trim_ascii()).map_err(|_| invalid())?;
                if slot.replace(value).is_some() {
                    return Err(invalid());
                }
            }
        }
    }
    if pid != Some(tid) || nspid != Some(tid) {
        return Err(invalid());
    }
    Ok(())
}

pub(super) fn read(
    target: &Stopped,
    tid: i32,
    address: usize,
    buf: &mut [u8],
) -> Result<(), Error> {
    let end = range_end(address, buf.len())?;
    if unsafe { libc::sysconf(libc::_SC_PAGESIZE) } != PAGE as libc::c_long {
        return Err(refused(Refusal::UnsupportedPlatform));
    }
    validate_native_mode(target)?;
    let layout = native_pkru_layout()
        .map_err(|_| refused(Refusal::UnsupportedPlatform))?
        .ok_or_else(|| refused(Refusal::UnsupportedPlatform))?;
    #[cfg(test)]
    tests::note_proc_observation();
    let root = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open("/proc")
        .map_err(proc_error)?;
    let mount = verify_proc(&root, None)?;
    let caller_tid = Errno::result(unsafe { libc::syscall(libc::SYS_gettid) })
        .map_err(|e| refused(Refusal::Procfs(e)))? as usize;
    proc_view(
        &proc_file(&root, mount, "thread-self/status", MAX_STATUS)?,
        caller_tid,
    )?;
    let smaps = proc_file(&root, mount, &format!("{tid}/smaps"), MAX_SMAPS)?;
    let mapping = mapping(&smaps, address, end)?;
    // This second ptrace read both obtains actual PKRU and rechecks the stopped
    // task after procfs observation. It does not replace caller exclusion.
    let state = target.getxstate().map_err(target_error)?;
    let pkru = decode_native_pkru_xstate(&state.0, layout)
        .map_err(|_| refused(Refusal::UnsupportedPlatform))?;
    mapping.read_access(pkru)?;

    let mut staged = [0u8; MAX_READ];
    let local = libc::iovec {
        iov_base: staged.as_mut_ptr().cast(),
        iov_len: buf.len(),
    };
    let remote = libc::iovec {
        iov_base: address as *mut libc::c_void,
        iov_len: buf.len(),
    };
    // Exactly one native transfer, including 1..8 byte operands. GUP errors
    // (even EFAULT), authority errors and short counts are backend refusals.
    // No remote Rust reference, ptrace fallback, or retry is permitted.
    #[cfg(test)]
    tests::note_native_transfer();
    let result = Errno::result(unsafe { libc::process_vm_readv(tid, &local, 1, &remote, 1, 0) })
        .map(|count| count as usize);
    publish(result, &staged, buf)
}

fn publish(
    result: Result<usize, Errno>,
    staged: &[u8; MAX_READ],
    buf: &mut [u8],
) -> Result<(), Error> {
    let count = result.map_err(|e| refused(Refusal::NativeTransfer(e)))?;
    if count != buf.len() {
        return Err(refused(Refusal::ShortTransfer(count)));
    }
    buf.copy_from_slice(&staged[..buf.len()]);
    Ok(())
}

#[cfg(test)]
mod tests {
    // Keep the qualified tests/helpers byte-identical and preserve their names.
    include!("native_read_tests.rs");

    mod mm_bound {
        include!("native_mm_read_tests.rs");
    }
}
