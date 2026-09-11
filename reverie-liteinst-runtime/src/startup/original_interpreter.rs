use std::fs::File;
use std::io;
use std::mem::ManuallyDrop;
use std::ops::Range;
use std::os::fd::AsRawFd;
use std::os::fd::BorrowedFd;
use std::os::fd::FromRawFd;
use std::os::unix::fs::FileExt;
use std::os::unix::fs::MetadataExt;

use super::AuxvSnapshot;
use super::InterpreterImage;
use super::InterpreterPlan;
use super::PlanError;

pub const IMAGE_FD_ENV: &str = "HERMIT_LITEINST_ORIGINAL_INTERPRETER_FD";
const MAX_BYTES: usize = 512 * 1024 * 1024;

pub struct OriginalInterpreter {
    bytes: Box<[u8]>,
    device: u64,
    inode: u64,
}

impl OriginalInterpreter {
    pub fn acquire(descriptor: BorrowedFd<'_>) -> io::Result<Self> {
        if descriptor.as_raw_fd() < 3 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "original interpreter descriptor overlaps stdio",
            ));
        }
        let seals = unsafe { libc::fcntl(descriptor.as_raw_fd(), libc::F_GET_SEALS) };
        if seals < 0 {
            return Err(io::Error::last_os_error());
        }
        let required =
            libc::F_SEAL_SEAL | libc::F_SEAL_SHRINK | libc::F_SEAL_GROW | libc::F_SEAL_WRITE;
        if seals & required != required {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "original interpreter is not fully sealed",
            ));
        }
        let file = ManuallyDrop::new(unsafe { File::from_raw_fd(descriptor.as_raw_fd()) });
        let metadata = file.metadata()?;
        let size = usize::try_from(metadata.len())
            .ok()
            .filter(|size| (64..=MAX_BYTES).contains(size));
        let size = size.filter(|_| metadata.is_file()).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "original interpreter file is not a bounded regular image",
            )
        })?;
        let mut bytes = Vec::new();
        bytes.try_reserve_exact(size).map_err(io::Error::other)?;
        bytes.resize(size, 0);
        file.read_exact_at(&mut bytes, 0)?;
        InterpreterImage::parse(&bytes).map_err(io::Error::other)?;
        Ok(Self {
            bytes: bytes.into_boxed_slice(),
            device: metadata.dev(),
            inode: metadata.ino(),
        })
    }

    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    pub fn file_identity(&self) -> (u64, u64) {
        (self.device, self.inode)
    }

    pub fn plan<'image>(
        &'image self,
        initial: &AuxvSnapshot,
        reservation: Range<u64>,
        other_mappings: &[Range<u64>],
    ) -> Result<InterpreterPlan<'image>, PlanError> {
        InterpreterImage::parse(&self.bytes)?.plan_at_original_base(
            initial,
            reservation,
            other_mappings,
        )
    }
}
