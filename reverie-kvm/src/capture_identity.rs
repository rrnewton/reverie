// Private identities for the in-memory capture streams. An owned anonymous
// pipe endpoint reserves each native inode for the whole capture lifetime.
use std::os::fd::AsRawFd;
use std::os::fd::FromRawFd;
use std::os::fd::OwnedFd;
use std::sync::Arc;

use super::OutputAlias;

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
pub(super) type CaptureDropProbe = Box<dyn FnOnce([std::os::fd::RawFd; 4]) + Send>;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct CaptureObjectIdentity {
    pub(super) device: libc::dev_t,
    pub(super) inode: libc::ino_t,
}

struct CapturePipe {
    identity: CaptureObjectIdentity,
    // Private read endpoint keeps writable aliases ready. Neither endpoint is
    // supervisor stdio; capture writes still go exclusively to the memory sink.
    _keeper: OwnedFd,
    writer: Arc<std::fs::File>,
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
        let writer = unsafe { OwnedFd::from_raw_fd(descriptors[1]) };
        let relocate = |descriptor: OwnedFd| -> std::io::Result<OwnedFd> {
            if descriptor.as_raw_fd() >= 3 {
                return Ok(descriptor);
            }
            // SAFETY: the owned source is live; success creates a private fd.
            let private = unsafe { libc::fcntl(descriptor.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 3) };
            if private < 0 {
                #[cfg(test)]
                RELOCATION_FAILURES.set(RELOCATION_FAILURES.get() + 1);
                return Err(std::io::Error::last_os_error());
            }
            Ok(unsafe { OwnedFd::from_raw_fd(private) })
        };
        let keeper = relocate(keeper)?;
        let writer = relocate(writer)?;
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
        let mut writer_stat = std::mem::MaybeUninit::<libc::stat>::zeroed();
        if unsafe { libc::fstat(writer.as_raw_fd(), writer_stat.as_mut_ptr()) } != 0 {
            return Err(std::io::Error::last_os_error());
        }
        let writer_stat = unsafe { writer_stat.assume_init() };
        if (stat.st_dev, stat.st_ino) != (writer_stat.st_dev, writer_stat.st_ino)
            || unsafe { libc::fcntl(writer.as_raw_fd(), libc::F_GETFL) } & libc::O_ACCMODE
                != libc::O_WRONLY
        {
            return Err(std::io::Error::other("invalid private capture writer"));
        }
        // Neither endpoint carries captured payload. The reader pins identity
        // and prevents a missing-peer error on private writable aliases.
        #[cfg(test)]
        PIPES_PREPARED.set(PIPES_PREPARED.get() + 1);
        Ok(Self {
            identity: CaptureObjectIdentity {
                device: stat.st_dev,
                inode: stat.st_ino,
            },
            _keeper: keeper,
            writer: Arc::new(writer.into()),
        })
    }
}

pub(crate) struct CapturedPipeIdentities {
    stdout: CapturePipe,
    stderr: CapturePipe,
    #[cfg(test)]
    pub(super) drop_probe: std::sync::Mutex<Option<CaptureDropProbe>>,
}

impl CapturedPipeIdentities {
    pub(super) fn try_new() -> std::io::Result<Self> {
        let stdout = CapturePipe::try_new()?;
        let stderr = CapturePipe::try_new()?;
        Ok(Self {
            stdout,
            stderr,
            #[cfg(test)]
            drop_probe: std::sync::Mutex::new(None),
        })
    }

    pub(super) fn metadata(&self) -> CaptureMetadata {
        CaptureMetadata {
            stdout: self.stdout.identity,
            stderr: self.stderr.identity,
        }
    }

    pub(super) fn writer(&self, alias: OutputAlias) -> Arc<std::fs::File> {
        match alias {
            OutputAlias::Stdout => self.stdout.writer.clone(),
            OutputAlias::Stderr => self.stderr.writer.clone(),
        }
    }

    #[cfg(test)]
    pub(super) fn metadata_descriptors(&self) -> [std::os::fd::RawFd; 2] {
        [
            self.stdout._keeper.as_raw_fd(),
            self.stderr._keeper.as_raw_fd(),
        ]
    }

    #[cfg(test)]
    pub(super) fn descriptors(&self) -> [std::os::fd::RawFd; 4] {
        [
            self.stdout._keeper.as_raw_fd(),
            self.stdout.writer.as_raw_fd(),
            self.stderr._keeper.as_raw_fd(),
            self.stderr.writer.as_raw_fd(),
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
}

impl CaptureMetadata {
    pub(super) fn identity(self, alias: OutputAlias) -> CaptureObjectIdentity {
        match alias {
            OutputAlias::Stdout => self.stdout,
            OutputAlias::Stderr => self.stderr,
        }
    }
}
