/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! An unwired x86-64 Linux ELF loader and the host side of its start protocol.
//!
//! The caller pins a readable ELF file, creates a short symlink to the prepared
//! loader image, and executes the padded symlink with the target's argv/envp.
//! The target and [`PreparedStart::metadata`] are passed on descriptors 100 and
//! 102. The loader reuses the kernel's initial stack, replaces its ELF-specific
//! auxv values, and transfers control to the target's PT_INTERP.
//!
//! This is a layout building block, not an exec authorization implementation.
//! The caller must use `ADDR_NO_RANDOMIZE`, keep the pinned files immutable,
//! preserve descriptor numbers, and arrange the native argv/envp. No Reverie
//! backend calls this crate. See the crate README for the admitted ELF class
//! and the exact differences still visible through procfs.

#[cfg(not(all(target_os = "linux", target_arch = "x86_64")))]
compile_error!("reverie-elf-loader supports x86-64 Linux only");

use std::ffi::CString;
use std::ffi::OsStr;
use std::fmt;
use std::fs::File;
use std::fs::OpenOptions;
use std::fs::Permissions;
use std::io;
use std::os::fd::RawFd;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::FileExt;
use std::os::unix::fs::MetadataExt;
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::path::PathBuf;

/// Linux's path buffer includes its terminating NUL.
pub const PATH_MAX: usize = 4096;
/// The lower address band reserved for the loader and its scratch space.
pub const MIN_PROGRAM_ADDRESS: u64 = 0x400000;
pub const TARGET_FD: RawFd = 100;
pub const METADATA_FD: RawFd = 102;
const PAGE: u64 = 4096;
const ELF_ET_DYN_BASE: u64 = 0x555555554aaa;
const PT_LOAD: u32 = 1;
const PT_INTERP: u32 = 3;
const PT_NOTE: u32 = 4;
const PT_GNU_STACK: u32 = 0x6474e551;
const ET_EXEC: u16 = 2;
const ET_DYN: u16 = 3;
const USER_LIMIT: u64 = 0x800000000000;

/// The five scalar auxv values replaced after loading.
pub const AUXV_PATCH_TYPES: [u64; 5] = [3, 5, 7, 9, 33];

/// Why a start cannot be prepared. These are loader refusals, not guest errno.
#[derive(Debug)]
pub enum Error {
    Io(io::Error),
    InvalidElf(&'static str),
    UnsupportedElf(&'static str),
    PaddedPathTooLong {
        length_with_nul: usize,
        maximum: usize,
    },
    LauncherPathTooLong {
        filename_length: usize,
        launcher_length: usize,
    },
    LowLoadSegment {
        address: u64,
        interpreter: bool,
    },
    MissingInterpreter,
    ExecutableStack,
    InvalidInvocation(&'static str),
    FiniteAddressSpaceLimit {
        limit: u64,
    },
    FiniteDataLimitBelowStartupFootprint {
        limit: u64,
        minimum: u64,
    },
    InvalidLoaderTemplate(&'static str),
    AmbiguousDescriptorComm,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(e) => write!(f, "ELF loader I/O: {e}"),
            Self::InvalidElf(reason) => write!(f, "invalid ELF: {reason}"),
            Self::UnsupportedElf(reason) => {
                write!(f, "ELF loader refuses unsupported ELF: {reason}")
            }
            Self::PaddedPathTooLong {
                length_with_nul,
                maximum,
            } => write!(
                f,
                "ELF loader refuses padded path of {length_with_nul} bytes including NUL; PATH_MAX is {maximum}"
            ),
            Self::LauncherPathTooLong {
                filename_length,
                launcher_length,
            } => write!(
                f,
                "ELF loader refuses filename length {filename_length}; launcher path requires {launcher_length} bytes"
            ),
            Self::LowLoadSegment {
                address,
                interpreter,
            } => write!(
                f,
                "ELF loader refuses {} PT_LOAD at {address:#x}, below {MIN_PROGRAM_ADDRESS:#x}",
                if *interpreter {
                    "interpreter"
                } else {
                    "program"
                }
            ),
            Self::MissingInterpreter => write!(f, "ELF loader refuses program without PT_INTERP"),
            Self::ExecutableStack => write!(
                f,
                "ELF loader refuses executable, missing or repeated PT_GNU_STACK"
            ),
            Self::InvalidInvocation(reason) => write!(f, "ELF loader refuses invocation: {reason}"),
            Self::FiniteAddressSpaceLimit { limit } => write!(
                f,
                "ELF loader refuses finite RLIMIT_AS ({limit} bytes); loader mappings consume additional address space"
            ),
            Self::InvalidLoaderTemplate(reason) => write!(f, "invalid loader template: {reason}"),
            Self::FiniteDataLimitBelowStartupFootprint { limit, minimum } => write!(
                f,
                "ELF loader refuses RLIMIT_DATA {limit} bytes below its {minimum}-byte ELF startup footprint boundary"
            ),
            Self::AmbiguousDescriptorComm => write!(
                f,
                "ELF loader refuses AT_EMPTY_PATH comm: a procfd name ending in ' (deleted)' with live hardlinks is ambiguous"
            ),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            _ => None,
        }
    }
}

impl From<io::Error> for Error {
    fn from(value: io::Error) -> Self {
        Self::Io(value)
    }
}

/// A literal exec call. `dirfd` is the descriptor number used by the new child.
#[derive(Clone, Debug)]
pub struct Invocation {
    dirfd: RawFd,
    path: CString,
    flags: i32,
}

impl Invocation {
    pub fn execve(path: impl AsRef<OsStr>) -> Result<Self, Error> {
        Self::execveat(libc::AT_FDCWD, path, 0)
    }

    pub fn execveat(dirfd: RawFd, path: impl AsRef<OsStr>, flags: i32) -> Result<Self, Error> {
        if flags & !(libc::AT_EMPTY_PATH | libc::AT_SYMLINK_NOFOLLOW) != 0 {
            return Err(Error::InvalidInvocation("unsupported execveat flags"));
        }
        let path = CString::new(path.as_ref().as_bytes())
            .map_err(|_| Error::InvalidInvocation("path contains NUL"))?;
        if path.as_bytes().is_empty() && flags & libc::AT_EMPTY_PATH == 0 {
            return Err(Error::InvalidInvocation("empty path without AT_EMPTY_PATH"));
        }
        Ok(Self { dirfd, path, flags })
    }

    /// The kernel's `bprm->filename`, including execveat's `/dev/fd/` prefix.
    pub fn native_execfn(&self) -> CString {
        if self.dirfd == libc::AT_FDCWD || self.path.as_bytes().starts_with(b"/") {
            return self.path.clone();
        }
        let mut filename = format!("/dev/fd/{}", self.dirfd).into_bytes();
        if !self.path.as_bytes().is_empty() {
            filename.push(b'/');
            filename.extend_from_slice(self.path.as_bytes());
        }
        CString::new(filename).expect("numeric descriptor and validated path have no NUL")
    }

    pub fn dirfd(&self) -> RawFd {
        self.dirfd
    }
    pub fn path(&self) -> &std::ffi::CStr {
        &self.path
    }
    pub fn flags(&self) -> i32 {
        self.flags
    }
}

/// Limits that will be installed in the child before exec.
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    pub data: u64,
    pub address_space: u64,
}

impl Limits {
    pub fn current() -> Result<Self, Error> {
        fn limit(resource: libc::__rlimit_resource_t) -> io::Result<u64> {
            let mut value = libc::rlimit {
                rlim_cur: 0,
                rlim_max: 0,
            };
            // SAFETY: value is writable for the exact structure size.
            if unsafe { libc::getrlimit(resource, &mut value) } != 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(value.rlim_cur)
        }
        Ok(Self {
            data: limit(libc::RLIMIT_DATA)?,
            address_space: limit(libc::RLIMIT_AS)?,
        })
    }
}

/// Native ELF layout with randomization disabled.
#[derive(Clone, Debug)]
pub struct ProgramLayout {
    pub load_bias: u64,
    pub program_base: u64,
    pub entry: u64,
    pub phdr: u64,
    pub phnum: u16,
    pub start_data: u64,
    pub end_data: u64,
    pub start_brk: u64,
    pub interpreter: PathBuf,
    /// Conservative bound covering private writable target/interpreter loads,
    /// including a possible transient first-mapping reservation.
    pub minimum_data_limit: u64,
}

/// Values supplied at preparation or determined by the freestanding loader.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AuxvValue {
    Fixed(u64),
    InterpreterBase,
    RelocatedVdso,
    ExecFnString,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AuxvPatch {
    pub kind: u64,
    pub value: AuxvValue,
}

/// A start plan. It holds no live descriptors and performs no exec itself.
#[derive(Clone, Debug)]
pub struct PreparedStart {
    pub native_execfn: CString,
    pub padded_path: CString,
    pub comm: CString,
    pub layout: ProgramLayout,
    pub auxv_patches: [AuxvPatch; 6],
}

impl PreparedStart {
    /// Bytes for the private, non-CLOEXEC metadata descriptor 102.
    pub fn metadata(&self) -> Vec<u8> {
        let mut bytes = self.native_execfn.as_bytes_with_nul().to_vec();
        bytes.extend_from_slice(self.comm.as_bytes_with_nul());
        bytes.push(0); // Production loader rejects nonzero mutation flags.
        bytes
    }

    /// Generate a sparse per-start loader ELF from the bundled static program.
    pub fn write_image(&self, output: &File) -> Result<(), Error> {
        self.write_image_from_template(output, loader_template())
    }

    /// Generate from another loader built with the same protocol/linker script.
    /// This also lets the integration tests exercise separately compiled mutations.
    pub fn write_image_from_template(&self, output: &File, template: &[u8]) -> Result<(), Error> {
        let elf = Elf::from_bytes(template)
            .map_err(|_| Error::InvalidLoaderTemplate("ELF header or program headers"))?;
        if elf.kind != ET_EXEC {
            return Err(Error::InvalidLoaderTemplate("expected fixed ET_EXEC"));
        }
        let index = elf
            .headers
            .iter()
            .position(|p| p.kind == PT_NOTE)
            .ok_or(Error::InvalidLoaderTemplate("missing replaceable PT_NOTE"))?;
        let mut image = template.to_vec();
        // Match mkshadow2.py: leave a sparse zero tail, with the file offset
        // congruent to start_data. Its R-only file pages do not add data_vm.
        let offset = page_start(checked_add(template.len() as u64, 0x10000)?)
            + self.layout.start_data % PAGE;
        let filesize = self
            .layout
            .end_data
            .checked_sub(self.layout.start_data)
            .ok_or(Error::InvalidElf("end_data precedes start_data"))?;
        let memsize = self
            .layout
            .start_brk
            .checked_sub(self.layout.start_data)
            .ok_or(Error::InvalidElf("start_brk precedes start_data"))?;
        if memsize < filesize {
            return Err(Error::InvalidElf("shadow file size exceeds memory size"));
        }
        let shadow = ProgramHeader {
            kind: PT_LOAD,
            flags: 4,
            offset,
            vaddr: self.layout.start_data,
            paddr: self.layout.start_data,
            filesz: filesize,
            memsz: memsize,
            align: PAGE,
        };
        let header_offset = elf.phoff as usize + index * 56;
        shadow.write(&mut image[header_offset..header_offset + 56]);
        output.set_len(0)?;
        output.write_all_at(&image, 0)?;
        output.set_len(checked_add(offset, filesize)?)?;
        output.set_permissions(Permissions::from_mode(0o755))?;
        Ok(())
    }
}

/// The build.rs-generated static loader. This byte slice contains no libc.
pub fn loader_template() -> &'static [u8] {
    include_bytes!(env!("ELF_LOADER_TEMPLATE"))
}

pub fn prepare_start(
    target: &File,
    invocation: &Invocation,
    launcher_link: &Path,
) -> Result<PreparedStart, Error> {
    prepare_start_with_limits(target, invocation, launcher_link, Limits::current()?)
}

/// Prepare with the limits that the caller plans to install in the child.
/// Finite DATA at or above the ELF startup footprint is admitted: scratch is
/// shared, and shadow and data segments replace each other. Smaller DATA and
/// all finite AS limits are refused explicitly.
pub fn prepare_start_with_limits(
    target: &File,
    invocation: &Invocation,
    launcher_link: &Path,
    limits: Limits,
) -> Result<PreparedStart, Error> {
    let native_execfn = invocation.native_execfn();
    let padded_path = pad_launcher_path(launcher_link, native_execfn.as_bytes().len())?;
    if limits.address_space != libc::RLIM_INFINITY {
        return Err(Error::FiniteAddressSpaceLimit {
            limit: limits.address_space,
        });
    }
    let elf = Elf::read(target)?;
    let mut layout = elf.layout()?;
    // Classify without running a device open method or waiting on a FIFO,
    // then read the pinned object rather than repeating its path lookup.
    let interp_path = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_PATH)
        .open(&layout.interpreter)?;
    if !interp_path.metadata()?.is_file() {
        return Err(Error::UnsupportedElf("interpreter is not a regular file"));
    }
    let interp = File::open(format!(
        "/proc/self/fd/{}",
        std::os::fd::AsRawFd::as_raw_fd(&interp_path)
    ))?;
    let interp_elf = Elf::read(&interp)?;
    if interp_elf.kind == ET_EXEC {
        interp_elf.check_low(0, true)?;
    }
    let (target_data, target_peak) = elf.data_footprint(elf.kind == ET_DYN)?;
    let (_, interpreter_peak) = interp_elf.data_footprint(true)?;
    layout.minimum_data_limit = target_peak.max(checked_add(target_data, interpreter_peak)?);
    if limits.data < layout.minimum_data_limit {
        return Err(Error::FiniteDataLimitBelowStartupFootprint {
            limit: limits.data,
            minimum: layout.minimum_data_limit,
        });
    }
    let comm_bytes = if invocation.path.as_bytes().is_empty() {
        // With AT_EMPTY_PATH Linux uses the executable dentry, not /dev/fd/N.
        // An unlinked file with no surviving hardlinks has an unambiguous
        // kernel-added suffix; the remaining ambiguous class is refused.
        let link = std::fs::read_link(format!(
            "/proc/self/fd/{}",
            std::os::fd::AsRawFd::as_raw_fd(target)
        ))?;
        let bytes = link.as_os_str().as_bytes();
        let bytes = if let Some(original) = bytes.strip_suffix(b" (deleted)") {
            if target.metadata()?.nlink() != 0 {
                return Err(Error::AmbiguousDescriptorComm);
            }
            original
        } else {
            bytes
        };
        bytes
            .rsplit(|b| *b == b'/')
            .next()
            .unwrap_or(bytes)
            .to_vec()
    } else {
        native_execfn
            .as_bytes()
            .rsplit(|b| *b == b'/')
            .next()
            .unwrap_or_default()
            .to_vec()
    };
    let comm = CString::new(&comm_bytes[..comm_bytes.len().min(15)])
        .map_err(|_| Error::InvalidInvocation("comm contains NUL"))?;
    let auxv_patches = [
        AuxvPatch {
            kind: 3,
            value: AuxvValue::Fixed(layout.phdr),
        },
        AuxvPatch {
            kind: 5,
            value: AuxvValue::Fixed(u64::from(layout.phnum)),
        },
        AuxvPatch {
            kind: 7,
            value: AuxvValue::InterpreterBase,
        },
        AuxvPatch {
            kind: 9,
            value: AuxvValue::Fixed(layout.entry),
        },
        AuxvPatch {
            kind: 33,
            value: AuxvValue::RelocatedVdso,
        },
        AuxvPatch {
            kind: 31,
            value: AuxvValue::ExecFnString,
        },
    ];
    Ok(PreparedStart {
        native_execfn,
        padded_path,
        comm,
        layout,
        auxv_patches,
    })
}

/// Pad redundant separators, preserving lookup and the exact filename length.
pub fn pad_launcher_path(link: &Path, filename_length: usize) -> Result<CString, Error> {
    let length_with_nul = filename_length.saturating_add(1);
    if length_with_nul > PATH_MAX {
        return Err(Error::PaddedPathTooLong {
            length_with_nul,
            maximum: PATH_MAX,
        });
    }
    let mut path = link.as_os_str().as_bytes().to_vec();
    if path.contains(&0) || path.is_empty() {
        return Err(Error::InvalidInvocation(
            "launcher link is empty or contains NUL",
        ));
    }
    if path.len() < filename_length && !path.contains(&b'/') {
        let mut relative = b"./".to_vec();
        relative.extend_from_slice(&path);
        path = relative;
    }
    if path.len() > filename_length {
        return Err(Error::LauncherPathTooLong {
            filename_length,
            launcher_length: path.len(),
        });
    }
    if let Some(position) = path.iter().position(|b| *b == b'/') {
        let padding = filename_length - path.len();
        path.splice(position..position, std::iter::repeat_n(b'/', padding));
    }
    CString::new(path).map_err(|_| Error::InvalidInvocation("launcher link contains NUL"))
}

#[derive(Clone, Debug)]
struct ProgramHeader {
    kind: u32,
    flags: u32,
    offset: u64,
    vaddr: u64,
    paddr: u64,
    filesz: u64,
    memsz: u64,
    align: u64,
}

impl ProgramHeader {
    fn read(bytes: &[u8]) -> Self {
        Self {
            kind: u32_at(bytes, 0),
            flags: u32_at(bytes, 4),
            offset: u64_at(bytes, 8),
            vaddr: u64_at(bytes, 16),
            paddr: u64_at(bytes, 24),
            filesz: u64_at(bytes, 32),
            memsz: u64_at(bytes, 40),
            align: u64_at(bytes, 48),
        }
    }
    fn write(&self, bytes: &mut [u8]) {
        bytes[..4].copy_from_slice(&self.kind.to_le_bytes());
        bytes[4..8].copy_from_slice(&self.flags.to_le_bytes());
        for (index, value) in [
            self.offset,
            self.vaddr,
            self.paddr,
            self.filesz,
            self.memsz,
            self.align,
        ]
        .iter()
        .enumerate()
        {
            bytes[8 + index * 8..16 + index * 8].copy_from_slice(&value.to_le_bytes());
        }
    }
}

struct Elf {
    kind: u16,
    entry: u64,
    phoff: u64,
    phnum: u16,
    headers: Vec<ProgramHeader>,
    interpreter: Option<PathBuf>,
}

impl Elf {
    fn header(bytes: &[u8]) -> Result<(u16, u64, u64, u16), Error> {
        if bytes.len() < 64 || bytes[..4] != *b"\x7fELF" {
            return Err(Error::InvalidElf("truncated or missing ELF header"));
        }
        if bytes[4..7] != [2, 1, 1] || u16_at(bytes, 18) != 62 || u32_at(bytes, 20) != 1 {
            return Err(Error::UnsupportedElf("expected little-endian ELF64 x86-64"));
        }
        let kind = u16_at(bytes, 16);
        if kind != ET_EXEC && kind != ET_DYN {
            return Err(Error::UnsupportedElf("expected ET_EXEC or ET_DYN"));
        }
        let phnum = u16_at(bytes, 56);
        if u16_at(bytes, 52) != 64 || u16_at(bytes, 54) != 56 || phnum == 0 || phnum > 128 {
            return Err(Error::UnsupportedElf(
                "program-header size or count (maximum 128)",
            ));
        }
        Ok((kind, u64_at(bytes, 24), u64_at(bytes, 32), phnum))
    }

    fn from_bytes(bytes: &[u8]) -> Result<Self, Error> {
        let (kind, entry, phoff, phnum) = Self::header(bytes)?;
        let end = checked_add(phoff, u64::from(phnum) * 56)?;
        let header_bytes = bytes
            .get(
                phoff as usize
                    ..usize::try_from(end)
                        .map_err(|_| Error::InvalidElf("program-header offset overflow"))?,
            )
            .ok_or(Error::InvalidElf("truncated program headers"))?;
        Ok(Self {
            kind,
            entry,
            phoff,
            phnum,
            headers: header_bytes
                .as_chunks::<56>()
                .0
                .iter()
                .map(|bytes| ProgramHeader::read(bytes))
                .collect(),
            interpreter: None,
        })
    }

    fn read(file: &File) -> Result<Self, Error> {
        let metadata = file.metadata()?;
        if !metadata.is_file() {
            return Err(Error::UnsupportedElf("ELF is not a regular file"));
        }
        let mut header = [0; 64];
        file.read_exact_at(&mut header, 0)?;
        let (kind, entry, phoff, phnum) = Self::header(&header)?;
        let mut bytes = vec![0; usize::from(phnum) * 56];
        file.read_exact_at(&mut bytes, phoff)?;
        let headers: Vec<_> = bytes
            .as_chunks::<56>()
            .0
            .iter()
            .map(|bytes| ProgramHeader::read(bytes))
            .collect();
        let file_length = metadata.len();
        let mut interpreter = None;
        let mut last_load = None;
        for p in &headers {
            if p.kind == PT_LOAD {
                if p.filesz > p.memsz || checked_add(p.offset, p.filesz)? > file_length {
                    return Err(Error::InvalidElf("PT_LOAD file size"));
                }
                let memory_end = checked_add(p.vaddr, p.memsz)?;
                if p.vaddr >= USER_LIMIT || memory_end > USER_LIMIT {
                    return Err(Error::UnsupportedElf(
                        "PT_LOAD exceeds x86-64 default user address range",
                    ));
                }
                if p.offset % PAGE != p.vaddr % PAGE || (p.align > 1 && !p.align.is_power_of_two())
                {
                    return Err(Error::UnsupportedElf("PT_LOAD alignment"));
                }
                if last_load.is_some_and(|last| p.vaddr < last) {
                    return Err(Error::UnsupportedElf("unordered PT_LOAD headers"));
                }
                last_load = Some(p.vaddr);
            }
            if p.kind == PT_INTERP {
                if interpreter.is_some() || p.filesz < 2 || p.filesz > PATH_MAX as u64 {
                    return Err(Error::InvalidElf("PT_INTERP count or length"));
                }
                let mut path = vec![0; p.filesz as usize];
                file.read_exact_at(&mut path, p.offset)?;
                if path.last() != Some(&0) || path[..path.len() - 1].contains(&0) {
                    return Err(Error::InvalidElf(
                        "PT_INTERP is not a single terminated string",
                    ));
                }
                path.pop();
                interpreter = Some(PathBuf::from(OsStr::from_bytes(&path)));
            }
        }
        if last_load.is_none() {
            return Err(Error::InvalidElf("no PT_LOAD"));
        }
        Ok(Self {
            kind,
            entry,
            phoff,
            phnum,
            headers,
            interpreter,
        })
    }

    fn check_low(&self, bias: u64, interpreter: bool) -> Result<(), Error> {
        for p in self.headers.iter().filter(|p| p.kind == PT_LOAD) {
            let address = checked_add(bias, page_start(p.vaddr))?;
            if address < MIN_PROGRAM_ADDRESS {
                return Err(Error::LowLoadSegment {
                    address,
                    interpreter,
                });
            }
            let address = checked_add(bias, p.vaddr)?;
            if address >= USER_LIMIT || checked_add(address, p.memsz)? > USER_LIMIT {
                return Err(Error::UnsupportedElf(
                    "biased PT_LOAD exceeds x86-64 default user address range",
                ));
            }
        }
        Ok(())
    }

    fn layout(&self) -> Result<ProgramLayout, Error> {
        let interpreter = self.interpreter.clone().ok_or(Error::MissingInterpreter)?;
        let stacks: Vec<_> = self
            .headers
            .iter()
            .filter(|p| p.kind == PT_GNU_STACK)
            .collect();
        if stacks.len() != 1 || stacks[0].flags & 1 != 0 {
            return Err(Error::ExecutableStack);
        }
        let loads: Vec<_> = self.headers.iter().filter(|p| p.kind == PT_LOAD).collect();
        let first = loads[0];
        let mut load_bias = 0;
        if self.kind == ET_DYN {
            let alignment = loads
                .iter()
                .map(|p| p.align)
                .max()
                .unwrap_or(PAGE)
                .max(PAGE);
            load_bias = page_start(
                (ELF_ET_DYN_BASE & !(alignment - 1))
                    .checked_sub(first.vaddr)
                    .ok_or(Error::UnsupportedElf(
                        "PIE first segment exceeds native load base",
                    ))?,
            );
        }
        self.check_low(load_bias, false)?;
        let start_data = checked_add(loads.iter().map(|p| p.vaddr).max().unwrap(), load_bias)?;
        let end_data = checked_add(
            loads
                .iter()
                .map(|p| checked_add(p.vaddr, p.filesz))
                .collect::<Result<Vec<_>, _>>()?
                .into_iter()
                .max()
                .unwrap(),
            load_bias,
        )?;
        let memory_end = checked_add(
            loads
                .iter()
                .map(|p| checked_add(p.vaddr, p.memsz))
                .collect::<Result<Vec<_>, _>>()?
                .into_iter()
                .max()
                .unwrap(),
            load_bias,
        )?;
        if end_data < start_data {
            return Err(Error::UnsupportedElf("end_data precedes start_data"));
        }
        let phdr = loads
            .iter()
            .find(|p| p.offset <= self.phoff && self.phoff < p.offset + p.filesz)
            .ok_or(Error::UnsupportedElf("program headers are not mapped"))?;
        Ok(ProgramLayout {
            load_bias,
            program_base: checked_add(page_start(first.vaddr), load_bias)?,
            entry: checked_add(self.entry, load_bias)?,
            phdr: checked_add(
                checked_add(phdr.vaddr, self.phoff - phdr.offset)?,
                load_bias,
            )?,
            phnum: self.phnum,
            start_data,
            end_data,
            start_brk: page_align(memory_end)?,
            interpreter,
            minimum_data_limit: 0, // Filled after the pinned interpreter is read.
        })
    }

    fn data_footprint(&self, reserves_span: bool) -> Result<(u64, u64), Error> {
        let loads: Vec<_> = self.headers.iter().filter(|p| p.kind == PT_LOAD).collect();
        let mut data = 0;
        let mut peak = 0;
        for (index, p) in loads.iter().enumerate() {
            let file_pages = if p.filesz == 0 {
                0
            } else {
                page_align(checked_add(p.filesz, p.vaddr % PAGE)?)?
            };
            if p.flags & 2 != 0 {
                let span = if index == 0 && reserves_span && p.filesz != 0 {
                    let low = loads.iter().map(|p| page_start(p.vaddr)).min().unwrap();
                    let high = loads
                        .iter()
                        .map(|p| checked_add(p.vaddr, p.memsz))
                        .collect::<Result<Vec<_>, _>>()?
                        .into_iter()
                        .max()
                        .unwrap();
                    page_align(high - low)?
                } else {
                    file_pages
                };
                peak = peak.max(checked_add(data, span)?);
                data = checked_add(data, file_pages)?;
            }
            if p.memsz > p.filesz {
                // binfmt_elf supplies RW anonymous BSS even for an R-only
                // segment. A zero-file-size load includes its first partial
                // page, whereas file-backed BSS starts at the next page.
                let zero_start = if p.filesz == 0 {
                    page_start(p.vaddr)
                } else {
                    page_align(checked_add(p.vaddr, p.filesz)?)?
                };
                let zero_end = page_align(checked_add(p.vaddr, p.memsz)?)?;
                data = checked_add(data, zero_end - zero_start)?;
            }
            peak = peak.max(data);
        }
        Ok((data, peak))
    }
}

fn u16_at(bytes: &[u8], index: usize) -> u16 {
    u16::from_le_bytes(bytes[index..index + 2].try_into().unwrap())
}
fn u32_at(bytes: &[u8], index: usize) -> u32 {
    u32::from_le_bytes(bytes[index..index + 4].try_into().unwrap())
}
fn u64_at(bytes: &[u8], index: usize) -> u64 {
    u64::from_le_bytes(bytes[index..index + 8].try_into().unwrap())
}
fn checked_add(a: u64, b: u64) -> Result<u64, Error> {
    a.checked_add(b)
        .ok_or(Error::InvalidElf("address or file-offset overflow"))
}
fn page_start(value: u64) -> u64 {
    value & !(PAGE - 1)
}
fn page_align(value: u64) -> Result<u64, Error> {
    Ok(page_start(checked_add(value, PAGE - 1)?))
}
