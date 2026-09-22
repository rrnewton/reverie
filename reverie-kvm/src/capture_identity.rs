use std::os::fd::AsRawFd;
use std::os::fd::FromRawFd;
use std::os::fd::OwnedFd;
use std::os::fd::RawFd;

use super::OutputAlias;

// Per-fd proc/object inodes occupy 0x2000_0000..=0x201f_ffff and dynamically
// allocated file identities start at 0x2100_0000. Keep capture objects in the
// gap so their identity is independent of both the current fd and allocation
// order while remaining collision-free within the synthetic pipe device.
pub(super) const CAPTURE_STDOUT_INODE: libc::ino_t = 0x2020_0000;
pub(super) const CAPTURE_STDERR_INODE: libc::ino_t = 0x2020_0001;
const LAST_FD_DERIVED_INODE: libc::ino_t =
    0x2000_0000 + ((super::GUEST_NOFILE_LIMIT as libc::ino_t - 1) * 2) + 1;
const _: () = assert!(CAPTURE_STDOUT_INODE > LAST_FD_DERIVED_INODE);
const _: () = assert!(CAPTURE_STDERR_INODE < crate::elf::FIRST_GUEST_FILE_IDENTITY_INODE);

#[cfg(test)]
thread_local! {
    static PIPES_PREPARED: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static RELOCATION_FAILURES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
pub(super) fn pipes_prepared() -> usize {
    PIPES_PREPARED.get()
}

#[cfg(test)]
pub(super) fn relocation_failures() -> usize {
    RELOCATION_FAILURES.get()
}

#[cfg(test)]
pub(super) type CaptureDropProbe = Box<dyn FnOnce([std::os::fd::RawFd; 2]) + Send>;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct CaptureObjectIdentity {
    pub(super) inode: libc::ino_t,
}

impl CaptureObjectIdentity {
    pub(super) const fn for_alias(alias: OutputAlias) -> Self {
        Self {
            inode: match alias {
                OutputAlias::Stdout => CAPTURE_STDOUT_INODE,
                OutputAlias::Stderr => CAPTURE_STDERR_INODE,
            },
        }
    }
}

struct CapturePipe {
    // Never installed in a guest file table, used for I/O, or exported. The
    // keeper preserves the existing fallible setup and root-owner lifecycle;
    // guest-visible identity comes exclusively from the fixed metadata above.
    _keeper: OwnedFd,
}

impl CapturePipe {
    fn try_new() -> std::io::Result<Self> {
        let mut descriptors = [-1; 2];
        // SAFETY: descriptors has room for both newly owned endpoints.
        if unsafe { libc::pipe2(descriptors.as_mut_ptr(), libc::O_CLOEXEC) } != 0 {
            return Err(std::io::Error::last_os_error());
        }
        // SAFETY: a successful pipe2 created both descriptors exclusively here.
        let keeper = unsafe { OwnedFd::from_raw_fd(descriptors[0]) };
        let unused = unsafe { OwnedFd::from_raw_fd(descriptors[1]) };
        // A caller may have closed fd 0, 1 or 2. Never retain a private pipe in
        // the executor's implicit host-standard-descriptor namespace. Closing
        // the unused endpoint first bounds this relocation's descriptor peak.
        drop(unused);
        let keeper = if keeper.as_raw_fd() < 3 {
            // SAFETY: keeper is live; the returned descriptor is newly owned.
            let private = unsafe { libc::fcntl(keeper.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 3) };
            if private < 0 {
                let error = std::io::Error::last_os_error();
                #[cfg(test)]
                RELOCATION_FAILURES.set(RELOCATION_FAILURES.get() + 1);
                return Err(error);
            }
            let private = unsafe { OwnedFd::from_raw_fd(private) };
            drop(keeper);
            private
        } else {
            keeper
        };
        let mut stat = std::mem::MaybeUninit::<libc::stat>::zeroed();
        // SAFETY: keeper is live and stat is writable storage.
        if unsafe { libc::fstat(keeper.as_raw_fd(), stat.as_mut_ptr()) } != 0 {
            return Err(std::io::Error::last_os_error());
        }
        // SAFETY: fstat initialized stat on success.
        let stat = unsafe { stat.assume_init() };
        if stat.st_mode & libc::S_IFMT != libc::S_IFIFO {
            return Err(std::io::Error::other("capture identity is not a pipe"));
        }
        #[cfg(test)]
        PIPES_PREPARED.set(PIPES_PREPARED.get() + 1);
        Ok(Self { _keeper: keeper })
    }
}

pub(super) struct CapturedPipeIdentities {
    _stdout: CapturePipe,
    _stderr: CapturePipe,
    #[cfg(test)]
    pub(super) drop_probe: std::sync::Mutex<Option<CaptureDropProbe>>,
}

impl CapturedPipeIdentities {
    pub(super) fn try_new() -> std::io::Result<Self> {
        let stdout = CapturePipe::try_new()?;
        let stderr = CapturePipe::try_new()?;
        Ok(Self {
            _stdout: stdout,
            _stderr: stderr,
            #[cfg(test)]
            drop_probe: std::sync::Mutex::new(None),
        })
    }

    pub(super) fn metadata(&self) -> CaptureMetadata {
        CaptureMetadata {
            // Keep captured-output objects outside the sequential allocator
            // and per-fd proc ranges. Aliases retain these fixed object IDs
            // after the original standard slot is replaced or closed.
            stdout: CaptureObjectIdentity::for_alias(OutputAlias::Stdout),
            stderr: CaptureObjectIdentity::for_alias(OutputAlias::Stderr),
            // These descriptors are private lifetime anchors, not guest file
            // table entries.  Metadata-only syscalls may use them as stable
            // pipefs carriers instead of consulting inherited supervisor
            // stdout or stderr.
            stdout_statfs_carrier: self._stdout._keeper.as_raw_fd(),
            stderr_statfs_carrier: self._stderr._keeper.as_raw_fd(),
        }
    }

    #[cfg(test)]
    pub(super) fn descriptors(&self) -> [std::os::fd::RawFd; 2] {
        [
            self._stdout._keeper.as_raw_fd(),
            self._stderr._keeper.as_raw_fd(),
        ]
    }
}

#[cfg(test)]
impl Drop for CapturedPipeIdentities {
    fn drop(&mut self) {
        let probe = self.drop_probe.get_mut().unwrap().take();
        if let Some(probe) = probe {
            probe(self.descriptors());
        }
        // The real OwnedFd fields close only after this observation.
    }
}

#[derive(Clone, Copy)]
pub(super) struct CaptureMetadata {
    stdout: CaptureObjectIdentity,
    stderr: CaptureObjectIdentity,
    stdout_statfs_carrier: RawFd,
    stderr_statfs_carrier: RawFd,
}

impl CaptureMetadata {
    pub(super) fn identity(self, alias: OutputAlias) -> CaptureObjectIdentity {
        match alias {
            OutputAlias::Stdout => self.stdout,
            OutputAlias::Stderr => self.stderr,
        }
    }

    /// A read-end pipe keeper suitable only for filesystem metadata queries.
    /// It is not a descriptor-creation carrier: duplicating it would give a
    /// captured O_WRONLY guest alias a physically readable host description.
    pub(super) fn statfs_carrier(self, alias: OutputAlias) -> RawFd {
        match alias {
            OutputAlias::Stdout => self.stdout_statfs_carrier,
            OutputAlias::Stderr => self.stderr_statfs_carrier,
        }
    }
}
