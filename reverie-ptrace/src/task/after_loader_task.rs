//! Real stopped-task adapter for the explicitly selected one-task experiment.
use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::ffi::CString;
use std::io::Read;
use std::io::Seek;
use std::io::SeekFrom;
use std::io::{self};
use std::os::fd::AsRawFd;
use std::os::fd::FromRawFd;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::ffi::OsStringExt;
use std::os::unix::fs::FileExt;
use std::os::unix::fs::MetadataExt;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;

use goblin::elf::Elf;
use goblin::elf::header;
use goblin::elf::program_header as ph;
use goblin::elf::sym;
use iced_x86::Decoder;
use iced_x86::DecoderOptions;
use iced_x86::FlowControl;
use iced_x86::OpKind;
use sha2::Digest;
use sha2::Sha256;

use super::*;
use crate::LiteinstAfterLoaderConfig;
use crate::LiteinstCallerImage;
use crate::entry_call::CallCode;
use crate::entry_call::Calls;
use crate::entry_call::ImageIdentity;
use crate::entry_call::Phase as CallsPhase;
use crate::entry_call::TrapObservation;

const STACK_SIZE: u64 = 64 * 1024;
const PAGE: u64 = 4096;
const MAX_STACK_SNAPSHOT: usize = 1024 * 1024;
const MAX_PRIVATE_READ: u64 = 1024 * 1024;
const MAX_PRIVATE_MMAP_EFFECT: u64 = crate::after_loader::MAX_RUNTIME_LOAD_SPAN;
const MAX_PRIVATE_BRK_GROWTH: u64 = 1024 * 1024;
const PROC_FD_GETDENTS_BYTES: u64 = 8192;
const PROC_FD_READLINK_BYTES: u64 = 128;
const PROC_SELF_FD_DIRECTORY: &[u8] = b"/proc/self/fd";
const PROC_SELF_FD_PREFIX: &[u8] = b"/proc/self/fd/";
// Before the v12 handshake is readable, admit only the finite set of exact
// page-rounded usable lengths that its FXSAVE/XSAVE reserve contract can
// produce. The retained mapping is bound byte-for-byte to the handshake after
// Begin/Ready; this range is admission, not tolerance.
const CALLBACK_STACK_MIN_USABLE_BYTES: u64 = LITEINST_CALLBACK_EXECUTION_HEADROOM_BYTES + PAGE;
const CALLBACK_STACK_MAX_USABLE_BYTES: u64 = LITEINST_CALLBACK_EXECUTION_HEADROOM_BYTES
    + LITEINST_CALLBACK_SAVED_XSTATE_RESERVE_CEILING_BYTES
    + PAGE;
const RETURN_MARKER: u64 = 0x4c49_4341_4c4c_0001;
const TRAMPOLINE_ARENA_SIZE: u64 = 128 * PAGE;
const KERNEL_O_LARGEFILE: u64 = 0o100000;
const PRIVATE_SIGNAL_ACTIONS_OFFSET: u64 = 512;
const PRIVATE_SIGNAL_ACTION_BYTES: u64 = 32;
const PRIVATE_SIGNAL_ACTION_COUNT: u64 = 64;
const PRIVATE_SIGNAL_ACTIONS_END_OFFSET: u64 =
    PRIVATE_SIGNAL_ACTIONS_OFFSET + PRIVATE_SIGNAL_ACTION_BYTES * PRIVATE_SIGNAL_ACTION_COUNT;
const PRIVATE_POLICY_SCRATCH_OFFSET: u64 = 2560;
const PRIVATE_POLICY_SCRATCH_BYTES: usize = 8;
const LOADER_CACHE_SCRATCH_OFFSET: u64 =
    PRIVATE_POLICY_SCRATCH_OFFSET + PRIVATE_POLICY_SCRATCH_BYTES as u64;
const PRIVATE_HOST_CONFIG_OFFSET: u64 = 3072;
const LOADER_CACHE_SCRATCH_END_OFFSET: u64 = PRIVATE_HOST_CONFIG_OFFSET;
const LOADER_CACHE_SCRATCH_BYTES: usize =
    (LOADER_CACHE_SCRATCH_END_OFFSET - LOADER_CACHE_SCRATCH_OFFSET) as usize;
const _: () = {
    assert!(PRIVATE_SIGNAL_ACTIONS_END_OFFSET == PRIVATE_POLICY_SCRATCH_OFFSET);
    assert!(
        LOADER_CACHE_SCRATCH_OFFSET
            == PRIVATE_POLICY_SCRATCH_OFFSET + PRIVATE_POLICY_SCRATCH_BYTES as u64
    );
    assert!(LOADER_CACHE_SCRATCH_END_OFFSET == PRIVATE_HOST_CONFIG_OFFSET);
};
const ARCH_SHSTK_STATUS: u64 = 0x5005;
const STATX_OUTPUT_BYTES: usize = std::mem::size_of::<libc::statx>();
const PRIVATE_STATX_MASK: u32 = libc::STATX_BASIC_STATS | libc::STATX_BTIME;
const MFD_ALLOW_SEALING: u64 = 0x0002;
const TRAMPOLINE_SEALS: i32 =
    libc::F_SEAL_FUTURE_WRITE | libc::F_SEAL_SHRINK | libc::F_SEAL_GROW | libc::F_SEAL_SEAL;

// One exact glibc interpreter whose private loader-cache lifecycle is consumed
// below. A digest match is only the first gate: every cache syscall renews the
// complete mapped geometry, backing identity and live nonwritable bytes before
// accepting its exact wrapper site.
const LOADER_CACHE_INTERPRETER_SHA256: [u8; 32] = [
    0x3a, 0x47, 0xec, 0x2c, 0x0e, 0x2b, 0x0f, 0x09, 0x48, 0xea, 0x0c, 0xae, 0xfb, 0x77, 0xd2, 0x98,
    0x67, 0x9d, 0xf3, 0xb8, 0x47, 0x46, 0x31, 0xc7, 0xbe, 0x79, 0x89, 0xf1, 0x1b, 0x76, 0x2c, 0x5a,
];
const LOADER_CACHE_PATH: &[u8] = b"/etc/ld.so.cache";
const LOADER_CACHE_PATH_RVA: u64 = 0x2d266;
const LOADER_CACHE_OPENAT_SYSCALL_RVA: u64 = 0x257b6;
const LOADER_CACHE_FSTAT_SYSCALL_RVA: u64 = 0x25529;
const LOADER_CACHE_MMAP_SYSCALL_RVA: u64 = 0x25964;
const LOADER_CACHE_CLOSE_SYSCALL_RVA: u64 = 0x25679;
const LOADER_CACHE_MUNMAP_SYSCALL_RVA: u64 = 0x259f9;
const LOADER_CACHE_MEMFD_LINK: &[u8] = b"/memfd:reverie-after-loader-immutable (deleted)";
const LOADER_CACHE_ALIAS_READ_SYSCALL_RVA: u64 = 0x25806;
const LOADER_CACHE_ALIAS_RELRO_MPROTECT_SYSCALL_RVA: u64 = 0x25a29;
const LOADER_CACHE_ALIAS_HEADER_BYTES: u64 = 0x340;
const RESOLVE_NO_MAGICLINKS: u64 = 0x02;
const RESOLVE_IN_ROOT: u64 = 0x10;

#[repr(C)]
struct OpenHow {
    flags: u64,
    mode: u64,
    resolve: u64,
}

type StableBackingStamp = (u64, u64, u64, i64, i64, i64, i64);

// One exact glibc provider whose lazy ptmalloc bootstrap performs a raw
// getrandom(tcache_key, 8, GRND_NONBLOCK) while the controller's private
// dlopen is active. The complete provider digest and complete function bytes
// are both required; this is not a structural instruction-pattern admission.
const PTMALLOC_BOOTSTRAP_PROVIDER_SHA256: [u8; 32] = [
    0x1b, 0xb4, 0x75, 0x60, 0x7d, 0xfd, 0xce, 0xc1, 0xcf, 0x8b, 0x1a, 0x88, 0x5e, 0x2c, 0x71, 0xa3,
    0x67, 0x97, 0xe7, 0x86, 0xe4, 0x12, 0x48, 0xde, 0xb5, 0x0a, 0xb5, 0x43, 0x97, 0xd5, 0x6a, 0xac,
];
const PTMALLOC_BOOTSTRAP_FUNCTION_RVA: u64 = 0x985f0;
const PTMALLOC_BOOTSTRAP_SYSCALL_RVA: u64 = 0x98623;
const PTMALLOC_BRK_SYSCALL_RVA: u64 = 0x104f59;
const PTMALLOC_BOOTSTRAP_TCACHE_KEY_RVA: u64 = 0x2034f8;
const PTMALLOC_BOOTSTRAP_INITIALIZED_RVA: u64 = 0x203508;
const PTMALLOC_BOOTSTRAP_FUNCTION: &[u8] = &[
    0x55, 0xba, 0x01, 0x00, 0x00, 0x00, 0xbe, 0x08, 0x00, 0x00, 0x00, 0x48, 0x8d, 0x3d, 0xf6, 0xae,
    0x16, 0x00, 0x53, 0x48, 0x83, 0xec, 0x28, 0x64, 0x48, 0x8b, 0x04, 0x25, 0x28, 0x00, 0x00, 0x00,
    0x48, 0x89, 0x44, 0x24, 0x18, 0x31, 0xc0, 0xc6, 0x05, 0xea, 0xae, 0x16, 0x00, 0x01, 0xb8, 0x3e,
    0x01, 0x00, 0x00, 0x0f, 0x05, 0x48, 0x89, 0xe5, 0x83, 0xf8, 0x08, 0x74, 0x4d, 0x48, 0x89, 0xee,
    0xbf, 0x01, 0x00, 0x00, 0x00, 0xe8, 0x26, 0xd4, 0x03, 0x00, 0x48, 0x8b, 0x1c, 0x24, 0x33, 0x5c,
    0x24, 0x08, 0x48, 0x89, 0xee, 0x89, 0xd8, 0xbf, 0x01, 0x00, 0x00, 0x00, 0xc1, 0xc8, 0x08, 0x31,
    0xc3, 0x48, 0x89, 0x1d, 0xa0, 0xae, 0x16, 0x00, 0x48, 0xc1, 0xe3, 0x20, 0xe8, 0xff, 0xd3, 0x03,
    0x00, 0x48, 0x8b, 0x04, 0x24, 0x33, 0x44, 0x24, 0x08, 0x89, 0xc2, 0xc1, 0xca, 0x08, 0x31, 0xd0,
    0x48, 0x09, 0xc3, 0x48, 0x89, 0x1d, 0x7e, 0xae, 0x16, 0x00, 0x80, 0x3d, 0x4d, 0x29, 0x17, 0x00,
    0x00, 0x75, 0x07, 0xc6, 0x05, 0x9e, 0xae, 0x16, 0x00, 0x01, 0x48, 0x8b, 0x05, 0xff, 0x36, 0x16,
    0x00, 0x48, 0x8d, 0x0d, 0x08, 0x46, 0x16, 0x00, 0x64, 0x48, 0x89, 0x08, 0x48, 0x83, 0xc1, 0x60,
    0x48, 0x89, 0xc8, 0x48, 0x8d, 0x91, 0xf0, 0x07, 0x00, 0x00, 0x66, 0x0f, 0x1f, 0x44, 0x00, 0x00,
    0x48, 0x89, 0x40, 0x18, 0x48, 0x89, 0x40, 0x10, 0x48, 0x83, 0xc0, 0x10, 0x48, 0x39, 0xc2, 0x75,
    0xef, 0x48, 0x8d, 0x15, 0x08, 0xef, 0xff, 0xff, 0x48, 0x89, 0xee, 0xbf, 0x0d, 0x00, 0x00, 0x00,
    0x48, 0xc7, 0x05, 0x45, 0xae, 0x16, 0x00, 0x80, 0x00, 0x00, 0x00, 0xc7, 0x05, 0xc3, 0x45, 0x16,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x48, 0x89, 0x0d, 0x14, 0x46, 0x16, 0x00, 0xe8, 0xbf, 0x0f, 0xf9,
    0xff, 0x48, 0x8d, 0x15, 0x48, 0xef, 0xff, 0xff, 0x48, 0x89, 0xee, 0xbf, 0x03, 0x00, 0x00, 0x00,
    0xe8, 0xab, 0x0f, 0xf9, 0xff, 0x48, 0x8d, 0x15, 0xe4, 0xee, 0xff, 0xff, 0x48, 0x89, 0xee, 0xbf,
    0x1a, 0x00, 0x00, 0x00, 0xe8, 0x97, 0x0f, 0xf9, 0xff, 0x48, 0x8d, 0x15, 0x90, 0xee, 0xff, 0xff,
    0x48, 0x89, 0xee, 0xbf, 0x02, 0x00, 0x00, 0x00, 0xe8, 0x83, 0x0f, 0xf9, 0xff, 0x48, 0x8d, 0x15,
    0xec, 0xee, 0xff, 0xff, 0x48, 0x89, 0xee, 0xbf, 0x13, 0x00, 0x00, 0x00, 0xe8, 0x6f, 0x0f, 0xf9,
    0xff, 0x48, 0x8d, 0x15, 0x68, 0xec, 0xff, 0xff, 0x48, 0x89, 0xee, 0xbf, 0x19, 0x00, 0x00, 0x00,
    0xe8, 0x5b, 0x0f, 0xf9, 0xff, 0x48, 0x8d, 0x15, 0x64, 0xec, 0xff, 0xff, 0x48, 0x89, 0xee, 0xbf,
    0x1d, 0x00, 0x00, 0x00, 0xe8, 0x47, 0x0f, 0xf9, 0xff, 0x48, 0x8d, 0x15, 0x60, 0xec, 0xff, 0xff,
    0x48, 0x89, 0xee, 0xbf, 0x20, 0x00, 0x00, 0x00, 0xe8, 0x33, 0x0f, 0xf9, 0xff, 0x48, 0x8d, 0x15,
    0x8c, 0xec, 0xff, 0xff, 0x48, 0x89, 0xee, 0xbf, 0x1c, 0x00, 0x00, 0x00, 0xe8, 0x1f, 0x0f, 0xf9,
    0xff, 0x48, 0x8d, 0x15, 0x98, 0xec, 0xff, 0xff, 0x48, 0x89, 0xee, 0xbf, 0x15, 0x00, 0x00, 0x00,
    0xe8, 0x0b, 0x0f, 0xf9, 0xff, 0x48, 0x8d, 0x15, 0xd4, 0xed, 0xff, 0xff, 0x48, 0x89, 0xee, 0xbf,
    0x0a, 0x00, 0x00, 0x00, 0xe8, 0xf7, 0x0e, 0xf9, 0xff, 0x48, 0x8b, 0x44, 0x24, 0x18, 0x64, 0x48,
    0x2b, 0x04, 0x25, 0x28, 0x00, 0x00, 0x00, 0x75, 0x07, 0x48, 0x83, 0xc4, 0x28, 0x5b, 0x5d, 0xc3,
    0xe8, 0xdb, 0x62, 0x08, 0x00,
];

fn sha256_hex(bytes: &[u8]) -> String {
    let digest: [u8; 32] = Sha256::digest(bytes).into();
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn classify_procfs_absence<T>(observed: io::Result<T>) -> io::Result<bool> {
    match observed {
        Ok(_) => Ok(false),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(true),
        Err(error) => Err(error),
    }
}

fn procfs_path_is_absent(path: impl AsRef<Path>) -> io::Result<bool> {
    classify_procfs_absence(std::fs::symlink_metadata(path))
}

#[derive(Clone, Copy, Eq, PartialEq)]
struct PtmallocBootstrapObservation {
    initialized: u8,
    tcache_key: [u8; 8],
}

impl fmt::Debug for PtmallocBootstrapObservation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PtmallocBootstrapObservation")
            .field("initialized", &self.initialized)
            .field("tcache_key_sha256", &sha256_hex(&self.tcache_key))
            .finish()
    }
}

#[derive(Clone, Copy, Eq, PartialEq)]
struct PtmallocBootstrapState {
    load_bias: u64,
    entry: PtmallocBootstrapObservation,
    pre_dlopen_verified: bool,
    entropy_consumed: bool,
    injected_key: Option<[u8; 8]>,
}

impl fmt::Debug for PtmallocBootstrapState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PtmallocBootstrapState")
            .field("load_bias", &format_args!("{:#x}", self.load_bias))
            .field("entry", &self.entry)
            .field("pre_dlopen_verified", &self.pre_dlopen_verified)
            .field("entropy_consumed", &self.entropy_consumed)
            .field(
                "injected_key_sha256",
                &self.injected_key.map(|key| sha256_hex(&key)),
            )
            .finish()
    }
}

fn ptmalloc_bootstrap_profile_matches(provider: &[u8]) -> Result<bool, &'static str> {
    let digest: [u8; 32] = Sha256::digest(provider).into();
    if digest != PTMALLOC_BOOTSTRAP_PROVIDER_SHA256 {
        return Ok(false);
    }
    let elf = Elf::parse(provider).map_err(|_| "profiled ptmalloc provider is not ELF")?;
    if !elf.is_64
        || !elf.little_endian
        || elf.header.e_machine != header::EM_X86_64
        || elf.header.e_type != header::ET_DYN
    {
        return Err("profiled ptmalloc provider has a different ELF identity");
    }
    let function_end = PTMALLOC_BOOTSTRAP_FUNCTION_RVA
        .checked_add(PTMALLOC_BOOTSTRAP_FUNCTION.len() as u64)
        .ok_or("profiled ptmalloc function range overflowed")?;
    let executable = elf
        .program_headers
        .iter()
        .filter(|load| {
            load.p_type == ph::PT_LOAD
                && load.p_flags == (ph::PF_R | ph::PF_X)
                && load.p_vaddr <= PTMALLOC_BOOTSTRAP_FUNCTION_RVA
                && load
                    .p_vaddr
                    .checked_add(load.p_filesz)
                    .is_some_and(|end| function_end <= end)
        })
        .collect::<Vec<_>>();
    let [executable] = executable.as_slice() else {
        return Err("profiled ptmalloc function lacks one exact executable PT_LOAD");
    };
    let file_offset = executable
        .p_offset
        .checked_add(
            PTMALLOC_BOOTSTRAP_FUNCTION_RVA
                .checked_sub(executable.p_vaddr)
                .ok_or("profiled ptmalloc function precedes its PT_LOAD")?,
        )
        .ok_or("profiled ptmalloc function file offset overflowed")?;
    let start = usize::try_from(file_offset)
        .map_err(|_| "profiled ptmalloc function file offset is too large")?;
    let end = start
        .checked_add(PTMALLOC_BOOTSTRAP_FUNCTION.len())
        .ok_or("profiled ptmalloc function file range overflowed")?;
    if provider.get(start..end) != Some(PTMALLOC_BOOTSTRAP_FUNCTION) {
        return Err("profiled ptmalloc function bytes differ");
    }
    let syscall_offset = usize::try_from(
        PTMALLOC_BOOTSTRAP_SYSCALL_RVA
            .checked_sub(PTMALLOC_BOOTSTRAP_FUNCTION_RVA)
            .ok_or("profiled ptmalloc syscall precedes its function")?,
    )
    .map_err(|_| "profiled ptmalloc syscall offset is too large")?;
    if PTMALLOC_BOOTSTRAP_FUNCTION.get(syscall_offset..syscall_offset + 2) != Some(&[0x0f, 0x05]) {
        return Err("profiled ptmalloc syscall bytes differ");
    }
    let tcache_end = PTMALLOC_BOOTSTRAP_TCACHE_KEY_RVA
        .checked_add(8)
        .ok_or("profiled tcache-key range overflowed")?;
    let initialized_end = PTMALLOC_BOOTSTRAP_INITIALIZED_RVA
        .checked_add(1)
        .ok_or("profiled ptmalloc-initialized range overflowed")?;
    let writable_bss = elf
        .program_headers
        .iter()
        .filter(|load| {
            let Some(file_end) = load.p_vaddr.checked_add(load.p_filesz) else {
                return false;
            };
            let Some(memory_end) = load.p_vaddr.checked_add(load.p_memsz) else {
                return false;
            };
            load.p_type == ph::PT_LOAD
                && load.p_flags == (ph::PF_R | ph::PF_W)
                && file_end <= PTMALLOC_BOOTSTRAP_TCACHE_KEY_RVA
                && tcache_end <= memory_end
                && file_end <= PTMALLOC_BOOTSTRAP_INITIALIZED_RVA
                && initialized_end <= memory_end
        })
        .count();
    if writable_bss != 1 {
        return Err("profiled ptmalloc state lacks one exact writable BSS PT_LOAD");
    }
    Ok(true)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ProcFdAuditProfile {
    function_start: u64,
    function_end: u64,
    raw_syscall_start: u64,
    raw_syscall_end: u64,
    audit_call_returns: [u64; 6],
    raw_call_return: u64,
    trusted_gate: u64,
    trusted_gate_end: u64,
    trusted_syscall: u64,
}

fn elf_rx_file_bytes<'a>(
    elf: &Elf<'_>,
    bytes: &'a [u8],
    address: u64,
    length: u64,
) -> Option<&'a [u8]> {
    let end = address.checked_add(length)?;
    let load = elf.program_headers.iter().find(|load| {
        load.p_type == ph::PT_LOAD
            && load.p_flags == (ph::PF_R | ph::PF_X)
            && load.p_vaddr <= address
            && end <= load.p_vaddr.checked_add(load.p_filesz).unwrap_or(0)
    })?;
    let offset = load
        .p_offset
        .checked_add(address.checked_sub(load.p_vaddr)?)?;
    let file_end = offset.checked_add(length)?;
    bytes.get(usize::try_from(offset).ok()?..usize::try_from(file_end).ok()?)
}

fn proc_fd_audit_profile(runtime: &[u8]) -> Result<ProcFdAuditProfile, &'static str> {
    const AUDIT: &str = "reverie_liteinst_proc_fd_audit";
    const TRUSTED_GATE: &str = "reverie_preload_trusted_syscall";
    const TRUSTED_SYSCALL: &str = "reverie_preload_trusted_syscall_ip";
    const TRUSTED_GATE_BYTES: &[u8; 26] = &[
        0x48, 0x89, 0xf8, 0x48, 0x89, 0xf7, 0x48, 0x89, 0xd6, 0x48, 0x89, 0xca, 0x4d, 0x89, 0xc2,
        0x4d, 0x89, 0xc8, 0x4c, 0x8b, 0x4c, 0x24, 0x08, 0x0f, 0x05, 0xc3,
    ];
    let elf = Elf::parse(runtime).map_err(|_| "proc-fd runtime is not ELF")?;
    if !elf.is_64
        || !elf.little_endian
        || elf.header.e_machine != header::EM_X86_64
        || elf.header.e_type != header::ET_DYN
        || elf.syms.len() > 1_000_000
    {
        return Err("proc-fd runtime ELF identity differs");
    }
    if elf.dynsyms.iter().any(|symbol| {
        elf.dynstrtab
            .get_at(symbol.st_name)
            .is_some_and(|name| name == AUDIT || name == "reverie_preload_raw_syscall6")
    }) {
        return Err("proc-fd private control symbol is dynamically exported");
    }
    let mut audit = None;
    let mut trusted_gate = None;
    let mut trusted = None;
    for symbol in &elf.syms {
        let Some(name) = elf.strtab.get_at(symbol.st_name) else {
            continue;
        };
        if name == AUDIT {
            if symbol.st_type() != sym::STT_FUNC
                || symbol.st_bind() != sym::STB_LOCAL
                || symbol.st_visibility() != sym::STV_HIDDEN
                || symbol.st_shndx == 0
                || symbol.st_size == 0
                || symbol.st_size > 1024 * 1024
                || elf_rx_file_bytes(&elf, runtime, symbol.st_value, symbol.st_size).is_none()
                || audit.replace((symbol.st_value, symbol.st_size)).is_some()
            {
                return Err("proc-fd audit function symbol differs");
            }
        } else if name == TRUSTED_GATE {
            if symbol.st_type() != sym::STT_FUNC
                || symbol.st_bind() != sym::STB_LOCAL
                || symbol.st_visibility() != sym::STV_HIDDEN
                || symbol.st_shndx == 0
                || symbol.st_size != TRUSTED_GATE_BYTES.len() as u64
                || elf_rx_file_bytes(&elf, runtime, symbol.st_value, symbol.st_size)
                    != Some(TRUSTED_GATE_BYTES)
                || trusted_gate
                    .replace((symbol.st_value, symbol.st_size))
                    .is_some()
            {
                return Err("proc-fd trusted-gate function symbol differs");
            }
        } else if name == TRUSTED_SYSCALL {
            if symbol.st_type() != sym::STT_NOTYPE
                || symbol.st_bind() != sym::STB_LOCAL
                || symbol.st_visibility() != sym::STV_HIDDEN
                || symbol.st_shndx == 0
                || elf_rx_file_bytes(&elf, runtime, symbol.st_value, 2) != Some(&[0x0f, 0x05])
                || trusted.replace(symbol.st_value).is_some()
            {
                return Err("proc-fd trusted-syscall symbol differs");
            }
        }
    }
    let (function_start, function_size) = audit.ok_or("proc-fd audit function symbol is absent")?;
    let (trusted_gate_start, trusted_gate_size) =
        trusted_gate.ok_or("proc-fd trusted-gate function symbol is absent")?;
    let trusted_syscall = trusted.ok_or("proc-fd trusted-syscall symbol is absent")?;
    let trusted_gate_end = trusted_gate_start
        .checked_add(trusted_gate_size)
        .ok_or("proc-fd trusted-gate function range overflowed")?;
    if trusted_syscall
        != trusted_gate_start
            .checked_add(23)
            .ok_or("proc-fd trusted syscall offset overflowed")?
        || trusted_gate_end
            != trusted_gate_start
                .checked_add(26)
                .ok_or("proc-fd trusted gate range overflowed")?
    {
        return Err("proc-fd trusted syscall is outside its exact gate");
    }
    let function_end = function_start
        .checked_add(function_size)
        .ok_or("proc-fd audit function range overflowed")?;
    let function = elf_rx_file_bytes(&elf, runtime, function_start, function_size)
        .ok_or("proc-fd audit function bytes are absent")?;
    let mut decoder = Decoder::with_ip(64, function, function_start, DecoderOptions::NONE);
    let mut direct_calls = Vec::new();
    while decoder.can_decode() {
        let instruction = decoder.decode();
        if instruction.is_invalid() || instruction.next_ip() > function_end {
            return Err("proc-fd audit function does not decode exactly");
        }
        if instruction.flow_control() == FlowControl::Call
            && matches!(
                instruction.op0_kind(),
                OpKind::NearBranch16 | OpKind::NearBranch32 | OpKind::NearBranch64
            )
        {
            if instruction.len() != 5 {
                return Err("proc-fd audit contains a non-rel32 direct call");
            }
            direct_calls.push((instruction.next_ip(), instruction.near_branch_target()));
        }
    }
    let shim_matches = |target: u64| {
        let Some(bytes) = elf_rx_file_bytes(&elf, runtime, target, 14) else {
            return false;
        };
        if bytes[..4] != [0xff, 0x74, 0x24, 0x08]
            || bytes[4] != 0xe8
            || bytes[9..] != [0x48, 0x83, 0xc4, 0x08, 0xc3]
        {
            return false;
        }
        let displacement = i32::from_le_bytes(bytes[5..9].try_into().unwrap());
        target
            .checked_add(9)
            .and_then(|return_address| return_address.checked_add_signed(i64::from(displacement)))
            == Some(trusted_gate_start)
    };
    let shim_calls = direct_calls
        .into_iter()
        .filter(|(_, target)| shim_matches(*target))
        .collect::<Vec<_>>();
    let [first, second, third, fourth, fifth, sixth] = shim_calls.as_slice() else {
        return Err("proc-fd audit does not contain exactly six direct raw-syscall calls");
    };
    if !shim_calls.iter().all(|(_, target)| *target == first.1) {
        return Err("proc-fd audit direct calls do not share one exact raw-syscall shim");
    }
    let raw_syscall_start = first.1;
    let raw_symbols = elf
        .syms
        .iter()
        .filter(|symbol| {
            symbol.st_value == raw_syscall_start
                && symbol.st_type() == sym::STT_FUNC
                && symbol.st_shndx != 0
        })
        .collect::<Vec<_>>();
    let [raw_symbol] = raw_symbols.as_slice() else {
        return Err("proc-fd raw-syscall shim symbol is not unique");
    };
    if raw_symbol.st_size != 14
        || raw_symbol.st_bind() != sym::STB_LOCAL
        || raw_symbol.st_visibility() != sym::STV_HIDDEN
    {
        return Err("proc-fd raw-syscall shim symbol provenance differs");
    }
    Ok(ProcFdAuditProfile {
        function_start,
        function_end,
        raw_syscall_start,
        raw_syscall_end: raw_syscall_start
            .checked_add(14)
            .ok_or("proc-fd raw-syscall function range overflowed")?,
        audit_call_returns: [first.0, second.0, third.0, fourth.0, fifth.0, sixth.0],
        raw_call_return: raw_syscall_start
            .checked_add(9)
            .ok_or("proc-fd raw-syscall return overflowed")?,
        trusted_gate: trusted_gate_start,
        trusted_gate_end,
        trusted_syscall,
    })
}

fn rel32_call_target(return_address: u64, instruction: [u8; 5]) -> Option<u64> {
    if instruction[0] != 0xe8 {
        return None;
    }
    let displacement = i32::from_le_bytes(instruction[1..].try_into().ok()?);
    return_address.checked_add_signed(i64::from(displacement))
}

fn proc_fd_audit_site_is_exact(
    profile: ProcFdAuditProfile,
    load_bias: u64,
    permit: &AfterLoaderSyscallPermit,
    registers: &libc::user_regs_struct,
    controller_stack: GuestRange,
    controller_return_slot: u64,
    controller_return_address: u64,
    expected_controller_return: u64,
    return_address: u64,
    call_instruction: [u8; 5],
    raw_return_address: u64,
    raw_call_instruction: [u8; 5],
    copied_arg5: u64,
    original_arg5: u64,
    data_spans: &[(u64, u64)],
) -> bool {
    let Some(syscall) = load_bias.checked_add(profile.trusted_syscall) else {
        return false;
    };
    let Some(function_start) = load_bias.checked_add(profile.function_start) else {
        return false;
    };
    let Some(function_end) = load_bias.checked_add(profile.function_end) else {
        return false;
    };
    let Some(raw_start) = load_bias.checked_add(profile.raw_syscall_start) else {
        return false;
    };
    let Some(raw_end) = load_bias.checked_add(profile.raw_syscall_end) else {
        return false;
    };
    let Some(trusted_gate) = load_bias.checked_add(profile.trusted_gate) else {
        return false;
    };
    let Some(trusted_gate_end) = load_bias.checked_add(profile.trusted_gate_end) else {
        return false;
    };
    let Some(resume) = syscall.checked_add(2) else {
        return false;
    };
    let Some(call_chain_end) = registers.rsp.checked_add(32) else {
        return false;
    };
    let Some(call_start) = return_address.checked_sub(5) else {
        return false;
    };
    let Some(raw_call_start) = raw_return_address.checked_sub(5) else {
        return false;
    };
    let Some(controller_return_end) = controller_return_slot.checked_add(8) else {
        return false;
    };
    let call_chain = (registers.rsp, call_chain_end);
    let controller_return = (controller_return_slot, controller_return_end);
    permit.instruction_pointer == syscall
        && permit.resume_pointer == resume
        && permit.instruction_length == 2
        && permit.instruction[..2] == [0x0f, 0x05]
        && after_loader_syscall_registers_match(permit.number, permit.args, registers)
        && registers.rsp & 15 == 8
        && controller_stack.start <= call_chain.0
        && call_chain.1 <= controller_stack.end
        && controller_stack.start <= controller_return.0
        && controller_return.1 == controller_stack.end
        && controller_return_address == expected_controller_return
        && function_start <= call_start
        && return_address < function_end
        && profile
            .audit_call_returns
            .iter()
            .any(|candidate| load_bias.checked_add(*candidate) == Some(return_address))
        && raw_start <= raw_call_start
        && raw_return_address < raw_end
        && load_bias.checked_add(profile.raw_call_return) == Some(raw_return_address)
        && trusted_gate < syscall
        && syscall < trusted_gate_end
        && rel32_call_target(return_address, call_instruction) == Some(raw_start)
        && rel32_call_target(raw_return_address, raw_call_instruction) == Some(trusted_gate)
        && copied_arg5 == permit.args[5]
        && original_arg5 == permit.args[5]
        && data_spans.iter().copied().enumerate().all(|(index, span)| {
            span.0 < span.1
                && controller_stack.start <= span.0
                && span.1 <= controller_stack.end
                && !ranges_overlap(span, call_chain)
                && !ranges_overlap(span, controller_return)
                && data_spans[..index]
                    .iter()
                    .copied()
                    .all(|prior| !ranges_overlap(prior, span))
        })
}

/// Decode the two canonical register representations of a C `int` argument.
///
/// Bound libc code can materialize a negative `int` by either writing a
/// 32-bit register (zero extension) or by sign-extending it to the syscall
/// register width. Refuse every other high word so private-syscall admission
/// remains stricter than the kernel's truncating `int` conversion.
fn canonical_c_int_argument(raw: u64) -> Option<i32> {
    let low = raw as u32;
    let zero_extended = u64::from(low);
    let sign_extended = i64::from(low as i32) as u64;
    (raw == zero_extended || raw == sign_extended).then_some(low as i32)
}

fn canonical_c_int_argument_is(raw: u64, expected: i32) -> bool {
    canonical_c_int_argument(raw) == Some(expected)
}

fn classify_trace_only_syscall_number(raw: u64) -> Result<(i64, Option<Sysno>), Errno> {
    let semantic = canonical_c_int_argument(raw).ok_or(Errno::ENOSYS)?;
    if semantic >= 0 && semantic as u64 & X32_SYSCALL_BIT != 0 {
        return Err(Errno::ENOSYS);
    }
    let known = usize::try_from(semantic).ok().and_then(Sysno::new);
    Ok((raw as i64, known))
}

fn controller_semantic_arguments_match(number: i64, args: [u64; 6]) -> bool {
    match number {
        libc::SYS_arch_prctl => args[0] == ARCH_SHSTK_STATUS,
        libc::SYS_sigaltstack => args[0] == 0,
        libc::SYS_rt_sigaction => (1..=64).contains(&args[0]) && args[1] == 0 && args[3] == 8,
        libc::SYS_rt_sigprocmask => {
            args[0] == libc::SIG_SETMASK as u64 && args[1] == 0 && args[3] == 8
        }
        libc::SYS_brk => args[0] == 0,
        libc::SYS_fcntl => canonical_c_int_argument_is(args[1], libc::F_GET_SEALS) && args[2] == 0,
        libc::SYS_close => true,
        _ => false,
    }
}

fn initializer_futex_arguments_match(args: [u64; 6]) -> bool {
    args[1] == (libc::FUTEX_WAKE | libc::FUTEX_PRIVATE_FLAG) as u64
        && args[2] == 1
        && args[3..] == [0, 0, 0]
}

fn initializer_fcntl_add_seals_arguments_match(args: [u64; 6]) -> bool {
    canonical_c_int_argument_is(args[1], libc::F_ADD_SEALS) && args[2] == TRAMPOLINE_SEALS as u64
}

fn after_loader_syscall_registers_match(
    number: i64,
    args: [u64; 6],
    registers: &libc::user_regs_struct,
) -> bool {
    registers.orig_rax as i64 == number
        && [
            registers.rdi,
            registers.rsi,
            registers.rdx,
            registers.r10,
            registers.r8,
            registers.r9,
        ] == args
}

fn ptmalloc_bootstrap_completion_registers_match(
    admission: &libc::user_regs_struct,
    completed: &libc::user_regs_struct,
) -> bool {
    let mut expected = register_words(admission);
    expected[10] = 8;
    register_words(completed) == expected
}

fn loader_cache_redirect_register_words_match(
    admission: [u64; 27],
    executed_path: u64,
    observed: [u64; 27],
) -> bool {
    let mut expected = admission;
    expected[13] = executed_path;
    observed == expected
}

fn loader_cache_completion_register_words_match(
    admission: [u64; 27],
    path: u64,
    raw_result: i64,
    observed: [u64; 27],
) -> bool {
    let mut expected = admission;
    expected[10] = raw_result as u64;
    expected[13] = path;
    observed == expected
}

fn loader_cache_admission_register_words_match(
    number: i64,
    args: [u64; 6],
    resume_pointer: u64,
    admission: [u64; 27],
) -> bool {
    admission[15] as i64 == number
        && [
            admission[14],
            admission[13],
            admission[12],
            admission[7],
            admission[9],
            admission[8],
        ] == args
        && admission[16] == resume_pointer
}

fn exact_readonly_openat_arguments(args: &[u64; 6]) -> bool {
    canonical_c_int_argument_is(args[0], libc::AT_FDCWD)
        && canonical_c_int_argument(args[2])
            == Some(libc::O_RDONLY | libc::O_CLOEXEC | libc::O_LARGEFILE)
        && args[3] == 0
}

fn exact_proc_fd_directory_openat_arguments(args: &[u64; 6]) -> bool {
    canonical_c_int_argument_is(args[0], libc::AT_FDCWD)
        && args[1] != 0
        && canonical_c_int_argument(args[2])
            == Some(libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC)
        && args[3..] == [0, 0, 0]
}

fn exact_proc_fd_getdents_arguments(args: &[u64; 6]) -> bool {
    args[1] != 0 && args[2] == PROC_FD_GETDENTS_BYTES && args[3..] == [0, 0, 0]
}

fn exact_proc_fd_readlink_arguments(args: &[u64; 6]) -> bool {
    canonical_c_int_argument_is(args[0], libc::AT_FDCWD)
        && args[1] != 0
        && args[2] != 0
        && args[3] == PROC_FD_READLINK_BYTES
        && args[4..] == [0, 0]
}

fn proc_self_fd_number(path: &[u8]) -> Option<u64> {
    let digits = path.strip_prefix(PROC_SELF_FD_PREFIX)?;
    if digits.is_empty()
        || digits.len() > 10
        || digits.len() > 1 && digits[0] == b'0'
        || !digits.iter().all(u8::is_ascii_digit)
    {
        return None;
    }
    let descriptor = std::str::from_utf8(digits).ok()?.parse::<u64>().ok()?;
    (descriptor <= libc::c_int::MAX as u64).then_some(descriptor)
}

fn proc_fd_dirent_descriptors(bytes: &[u8]) -> Result<BTreeSet<u64>, &'static str> {
    let mut cursor = 0;
    let mut descriptors = BTreeSet::new();
    while cursor < bytes.len() {
        if bytes.len() - cursor < 24 {
            return Err("truncated proc-fd directory record");
        }
        let record_length = u16::from_ne_bytes(
            bytes[cursor + 16..cursor + 18]
                .try_into()
                .map_err(|_| "truncated proc-fd directory record length")?,
        ) as usize;
        let end = cursor
            .checked_add(record_length)
            .ok_or("proc-fd directory record length overflow")?;
        if record_length < 24 || !record_length.is_multiple_of(8) || end > bytes.len() {
            return Err("malformed proc-fd directory record length");
        }
        let kind = bytes[cursor + 18];
        let name_field = &bytes[cursor + 19..end];
        let name_end = name_field
            .iter()
            .position(|byte| *byte == 0)
            .ok_or("unterminated proc-fd directory name")?;
        let name = &name_field[..name_end];
        if matches!(name, b"." | b"..") {
            if kind != libc::DT_DIR {
                return Err("proc-fd dot entry is not a directory");
            }
        } else {
            if kind != libc::DT_LNK
                || name.is_empty()
                || name.len() > 10
                || name.len() > 1 && name[0] == b'0'
                || !name.iter().all(u8::is_ascii_digit)
            {
                return Err("proc-fd entry is not one canonical descriptor symlink");
            }
            let descriptor = std::str::from_utf8(name)
                .map_err(|_| "nontext proc-fd descriptor")?
                .parse::<u64>()
                .map_err(|_| "invalid proc-fd descriptor")?;
            if descriptor > libc::c_int::MAX as u64 || !descriptors.insert(descriptor) {
                return Err("duplicate or out-of-range proc-fd descriptor");
            }
        }
        cursor = end;
    }
    Ok(descriptors)
}

fn private_brk_growth_range(previous: u64, requested: u64) -> Result<Option<(u64, u64)>, Errno> {
    let growth = requested.checked_sub(previous).ok_or(Errno::EINVAL)?;
    if growth == 0 || growth > MAX_PRIVATE_BRK_GROWTH {
        return Err(Errno::EINVAL);
    }
    let start = page_up(previous)?;
    let end = page_up(requested)?;
    Ok((start < end).then_some((start, end)))
}

fn exact_private_heap_mapping(mapping: &GuestMap) -> bool {
    mapping.start < mapping.end
        && mapping.offset == 0
        && mapping.device_major == 0
        && mapping.device_minor == 0
        && mapping.readable
        && mapping.writable
        && !mapping.executable
        && !mapping.shared
        && mapping.inode == 0
        && mapping
            .path
            .as_ref()
            .is_some_and(|path| path.as_os_str().as_encoded_bytes() == b"[heap]")
}

fn controller_call_stack_maps_are_exact(maps: &[GuestMap], usable: GuestRange) -> bool {
    let Some(allocation_start) = usable.start.checked_sub(PAGE) else {
        return false;
    };
    let Some(allocation_end) = usable.end.checked_add(PAGE) else {
        return false;
    };
    if usable.end.checked_sub(usable.start) != Some(STACK_SIZE) {
        return false;
    }
    let matching = maps
        .iter()
        .filter(|mapping| {
            ranges_overlap(
                (mapping.start, mapping.end),
                (allocation_start, allocation_end),
            )
        })
        .collect::<Vec<_>>();
    let [lower, middle, upper] = matching.as_slice() else {
        return false;
    };
    let anonymous_private = |mapping: &GuestMap| {
        !mapping.shared
            && mapping.offset == 0
            && mapping.device_major == 0
            && mapping.device_minor == 0
            && mapping.inode == 0
            && mapping.path.is_none()
    };
    anonymous_private(lower)
        && anonymous_private(middle)
        && anonymous_private(upper)
        && lower.start == allocation_start
        && lower.end == usable.start
        && !lower.readable
        && !lower.writable
        && !lower.executable
        && middle.start == usable.start
        && middle.end == usable.end
        && middle.readable
        && middle.writable
        && !middle.executable
        && upper.start == usable.end
        && upper.end == allocation_end
        && !upper.readable
        && !upper.writable
        && !upper.executable
}

fn private_brk_maps_advance_exactly(
    before: &[GuestMap],
    after: &[GuestMap],
    previous: u64,
    requested: u64,
) -> bool {
    let (Ok(previous_end), Ok(requested_end)) = (page_up(previous), page_up(requested)) else {
        return false;
    };
    if previous_end == requested_end {
        return before == after;
    }
    let before_heaps = before
        .iter()
        .enumerate()
        .filter(|(_, mapping)| exact_private_heap_mapping(mapping))
        .collect::<Vec<_>>();
    let after_heaps = after
        .iter()
        .enumerate()
        .filter(|(_, mapping)| exact_private_heap_mapping(mapping))
        .collect::<Vec<_>>();
    let [(after_index, after_heap)] = after_heaps.as_slice() else {
        return false;
    };
    if after_heap.end != requested_end {
        return false;
    }
    let before_index = match before_heaps.as_slice() {
        [] if after_heap.start == previous_end => None,
        [(index, before_heap)]
            if before_heap.end == previous_end
                && after_heap
                    == &&GuestMap {
                        end: requested_end,
                        ..(*before_heap).clone()
                    } =>
        {
            Some(*index)
        }
        _ => return false,
    };
    before
        .iter()
        .enumerate()
        .filter(|(index, _)| Some(*index) != before_index)
        .map(|(_, mapping)| mapping)
        .eq(after
            .iter()
            .enumerate()
            .filter(|(index, _)| *index != *after_index)
            .map(|(_, mapping)| mapping))
}

fn complete_private_brk_growth_state(
    current: &mut Option<u64>,
    consumed: &mut bool,
    previous: u64,
    requested: u64,
    raw_result: i64,
) -> bool {
    if raw_result < 0 || raw_result as u64 != requested || *current != Some(previous) || *consumed {
        return false;
    }
    *current = Some(requested);
    *consumed = true;
    true
}

fn exact_loader_cache_openat_arguments(args: &[u64; 6], logical_path: u64) -> bool {
    let flags = (libc::O_RDONLY | libc::O_CLOEXEC) as u64;
    args[0] == u64::from(libc::AT_FDCWD as u32)
        && args[1] == logical_path
        && args[2] == flags
        && args[3] == 0
        && args[4] == flags
        && args[5] == logical_path
}

fn loader_cache_mmap_arguments_match(args: [u64; 6], descriptor: u64, length: u64) -> bool {
    args == [
        0,
        length,
        libc::PROT_READ as u64,
        libc::MAP_PRIVATE as u64,
        descriptor,
        0,
    ]
}

fn loader_cache_redirect_arguments(
    logical: [u64; 6],
    redirect: &LoaderCacheRedirect,
) -> Option<[u64; 6]> {
    let mut executed = logical;
    executed[1] = redirect.scratch.0;
    (redirect.scratch.1.checked_sub(redirect.scratch.0) == Some(LOADER_CACHE_SCRATCH_BYTES as u64)
        && redirect.preimage.len() == LOADER_CACHE_SCRATCH_BYTES
        && !redirect.path.is_empty()
        && redirect.path.last() == Some(&0)
        && redirect.path.len() <= redirect.preimage.len())
    .then_some(executed)
}

fn after_loader_permit_argument_binding_is_exact(permit: &AfterLoaderSyscallPermit) -> bool {
    match &permit.effect {
        AfterLoaderSyscallEffect::OpenLoaderCache { redirect, .. } => {
            loader_cache_admission_register_words_match(
                permit.number,
                permit.args,
                permit.resume_pointer,
                redirect.admission_registers,
            ) && loader_cache_redirect_arguments(permit.args, redirect)
                == Some(permit.executed_args)
        }
        _ => permit.args == permit.executed_args,
    }
}

fn canonical_anonymous_mmap_descriptor(raw: u64) -> bool {
    canonical_c_int_argument_is(raw, -1)
}

fn exact_read_prefix_length(raw_result: i64, expected: &[u8]) -> Option<usize> {
    let amount = usize::try_from(raw_result).ok()?;
    (amount <= expected.len() && (amount != 0 || expected.is_empty())).then_some(amount)
}

fn exact_read_window_end(start: usize, input_len: usize, count: usize) -> Option<usize> {
    if start > input_len || (count == 0 && start != input_len) {
        return None;
    }
    start.checked_add(count).map(|end| end.min(input_len))
}

fn exact_owned_descriptor_statx_arguments(args: &[u64; 6]) -> bool {
    canonical_c_int_argument_is(args[2], libc::AT_EMPTY_PATH)
        && canonical_c_int_argument(args[3]) == Some(PRIVATE_STATX_MASK as i32)
}

fn fd_statx_bytes(descriptor: libc::c_int) -> io::Result<[u8; STATX_OUTPUT_BYTES]> {
    let mut value = std::mem::MaybeUninit::<libc::statx>::zeroed();
    let result = unsafe {
        libc::statx(
            descriptor,
            b"\0".as_ptr().cast(),
            libc::AT_EMPTY_PATH,
            PRIVATE_STATX_MASK,
            value.as_mut_ptr(),
        )
    };
    if result != 0 {
        return Err(io::Error::last_os_error());
    }
    let value = unsafe { value.assume_init() };
    let mut bytes = [0_u8; STATX_OUTPUT_BYTES];
    unsafe {
        std::ptr::copy_nonoverlapping(
            (&raw const value).cast::<u8>(),
            bytes.as_mut_ptr(),
            bytes.len(),
        );
    }
    Ok(bytes)
}

fn descriptor_statx_bytes(tid: Pid, descriptor: u64) -> io::Result<[u8; STATX_OUTPUT_BYTES]> {
    let file = std::fs::File::open(format!("/proc/{tid}/fd/{descriptor}"))?;
    fd_statx_bytes(file.as_raw_fd())
}

#[derive(Clone)]
pub(super) struct AfterLoaderToolCallbackContext {
    diagnostics: crate::LiteinstCallerDiagnostics,
    raw_clock: Option<u64>,
    tid: Pid,
    generation: u64,
    phase: LiteinstRuntimePhase,
    physical_status: Option<safeptrace::PhysicalStatusId>,
}

impl AfterLoaderToolCallbackContext {
    pub(super) fn record(&self, callback: &str) -> Result<(), Error> {
        self.diagnostics
            .record(
                format!("Tool callback: {callback}"),
                self.raw_clock,
                format!(
                    "tid={} generation={} phase={:?} physical_status={:?}",
                    self.tid, self.generation, self.phase, self.physical_status,
                ),
            )
            .map_err(|error| {
                Error::runtime(
                    self.tid,
                    "record LiteInst after-loader Tool callback",
                    error.to_string(),
                )
            })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CallerFunction {
    ErrnoLocation,
    Dlopen,
    Initializer,
}

/// A controller-bound image name scoped to one exec generation.
///
/// The contained identity is only a file-domain key. It is never compared to
/// the device/inode tuple observed in `/proc/<pid>/maps`.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct AfterLoaderImageId {
    generation: u64,
    file: crate::after_loader::FileIdentity,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct AfterLoaderTrampolineId {
    generation: u64,
    serial: u64,
    file: crate::after_loader::FileIdentity,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ResolvedImageGeometry {
    image: AfterLoaderImageId,
    mapping: MappingIdentity,
    load_bias: u64,
    span: (u64, u64),
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct AfterLoaderHelperIsolation {
    image: AfterLoaderImageId,
    range: GuestRange,
    original_mapping: GuestMap,
    mapping: MappingIdentity,
    load_bias: u64,
    file_offset: u64,
    expected_bytes: Vec<u8>,
}

impl AfterLoaderHelperIsolation {
    fn applies_to(&self, identity: MappingIdentity, load_bias: u64) -> bool {
        self.mapping == identity && self.load_bias == load_bias
    }

    fn validates_mapping(&self, task: &Stopped, mapping: &GuestMap) -> bool {
        mapping.start == self.range.start
            && mapping.end == self.range.end
            && !mapping.readable
            && !mapping.writable
            && !mapping.executable
            && !mapping.shared
            && mapping.mapping_identity() == self.mapping
            && mapping.path == self.original_mapping.path
            && mapping.offset == self.file_offset
            && guest_hook_mapping_attributes(task.pid(), mapping)
                .is_some_and(|attributes| attributes.fork_safe && attributes.protection_key == 0)
    }

    fn validates_live_page(&self, task: &Stopped) -> bool {
        guest_maps(task.pid()).is_some_and(|maps| {
            maps.iter()
                .any(|mapping| self.validates_mapping(task, mapping))
        }) && {
            let Ok(address) = usize::try_from(self.range.start) else {
                return false;
            };
            let mut bytes = vec![0_u8; self.expected_bytes.len()];
            read_stopped_ptrace_words(task, address, &mut bytes) && bytes == self.expected_bytes
        }
    }

    fn target_loader_projection(&self) -> Option<crate::target_loader::TargetIsolatedRxPage> {
        crate::target_loader::TargetIsolatedRxPage::new(
            self.range.start,
            self.range.end,
            self.file_offset,
            self.mapping.as_target_loader(),
        )
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct SharedReservationIdentity {
    mapping: MappingIdentity,
    path: Option<PathBuf>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TrampolineCloseShape {
    Complete,
    Abandoned,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ImageGeometryMismatch {
    MappingOutsideImageSpan,
    MissingPage,
    SharedPage,
    LoadPermissions,
    LoadIdentity,
    LoadOffset,
    AnonymousBssBacking,
    HolePermissions,
    HoleIdentity,
    HoleOffset,
    HelperIsolation,
    NonwritableBytes { file_offset: u64 },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ImageRelroState {
    WritableBeforeProtection,
    Protected,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum AfterLoaderOwnedDescriptor {
    SealedRuntime {
        bytes: Arc<[u8]>,
        position: u64,
    },
    BoundImage {
        image: AfterLoaderImageId,
        bytes: Arc<[u8]>,
        position: u64,
    },
    LoaderCacheAlias {
        image: AfterLoaderImageId,
        soname: String,
        raw_path: PathBuf,
        canonical_path: PathBuf,
        file: crate::after_loader::FileIdentity,
        bytes: Arc<[u8]>,
        stamp: StableBackingStamp,
        position: u64,
    },
    ProcMaps {
        device: u64,
        inode: u64,
        bytes: Arc<[u8]>,
        position: u64,
    },
    ProcFdDirectory {
        expected: BTreeMap<u64, Vec<u8>>,
        seen: BTreeSet<u64>,
        linked: BTreeSet<u64>,
        eof: bool,
    },
    LoaderCache {
        file: crate::after_loader::FileIdentity,
        mapping: MappingIdentity,
        length: u64,
        position: u64,
    },
    Trampoline {
        id: Option<AfterLoaderTrampolineId>,
        size: Option<u64>,
    },
}

impl AfterLoaderOwnedDescriptor {
    fn reopened(&self) -> Option<Self> {
        match self {
            Self::SealedRuntime { bytes, .. } => Some(Self::SealedRuntime {
                bytes: bytes.clone(),
                position: 0,
            }),
            Self::BoundImage { image, bytes, .. } => Some(Self::BoundImage {
                image: *image,
                bytes: bytes.clone(),
                position: 0,
            }),
            Self::LoaderCacheAlias { .. }
            | Self::ProcMaps { .. }
            | Self::ProcFdDirectory { .. }
            | Self::LoaderCache { .. }
            | Self::Trampoline { .. } => None,
        }
    }

    fn bytes_and_position(&self) -> Option<(&[u8], u64)> {
        match self {
            Self::SealedRuntime { bytes, position }
            | Self::BoundImage {
                bytes, position, ..
            }
            | Self::LoaderCacheAlias {
                bytes, position, ..
            }
            | Self::ProcMaps {
                bytes, position, ..
            } => Some((bytes, *position)),
            Self::ProcFdDirectory { .. } | Self::LoaderCache { .. } | Self::Trampoline { .. } => {
                None
            }
        }
    }

    fn set_position(&mut self, position: u64) -> bool {
        match self {
            Self::SealedRuntime {
                position: current, ..
            }
            | Self::BoundImage {
                position: current, ..
            }
            | Self::LoaderCacheAlias {
                position: current, ..
            }
            | Self::ProcMaps {
                position: current, ..
            } => {
                *current = position;
                true
            }
            Self::ProcFdDirectory { .. } | Self::LoaderCache { .. } | Self::Trampoline { .. } => {
                false
            }
        }
    }

    fn proc_fd_scan_is_complete(&self) -> bool {
        matches!(
            self,
            Self::ProcFdDirectory {
                expected,
                seen,
                linked,
                eof: true,
            } if seen.len() == expected.len()
                && expected.keys().all(|descriptor| seen.contains(descriptor))
                && linked == seen
        )
    }

    fn record_proc_fd_dirents(&mut self, descriptors: BTreeSet<u64>, eof_observed: bool) -> bool {
        let Self::ProcFdDirectory {
            expected,
            seen,
            linked,
            eof,
        } = self
        else {
            return false;
        };
        if *eof
            || linked != seen
            || descriptors
                .iter()
                .any(|descriptor| !expected.contains_key(descriptor) || seen.contains(descriptor))
        {
            return false;
        }
        if eof_observed {
            if !descriptors.is_empty()
                || seen.len() != expected.len()
                || !expected.keys().all(|descriptor| seen.contains(descriptor))
            {
                return false;
            }
            *eof = true;
        } else {
            seen.extend(descriptors);
        }
        true
    }

    fn record_proc_fd_readlink(&mut self, descriptor: u64) -> bool {
        let Self::ProcFdDirectory {
            expected,
            seen,
            linked,
            eof,
        } = self
        else {
            return false;
        };
        !*eof
            && expected.contains_key(&descriptor)
            && seen.contains(&descriptor)
            && linked.insert(descriptor)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct LoaderCacheMapping {
    start: u64,
    raw_length: u64,
    end: u64,
    identity: MappingIdentity,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum LoaderCacheLifecycle {
    AwaitingOpen,
    Open {
        descriptor: u64,
    },
    Statted {
        descriptor: u64,
    },
    MappedOpen {
        descriptor: u64,
        mapping: LoaderCacheMapping,
    },
    MappedClosed {
        descriptor: u64,
        mapping: LoaderCacheMapping,
    },
    Retired {
        descriptor: u64,
        mapping: LoaderCacheMapping,
    },
    ScratchReleaseArmed {
        descriptor: u64,
        mapping: LoaderCacheMapping,
    },
    Released {
        descriptor: u64,
        mapping: LoaderCacheMapping,
    },
}

impl LoaderCacheLifecycle {
    fn complete_open(&mut self, descriptor: u64) -> bool {
        if *self != Self::AwaitingOpen {
            return false;
        }
        *self = Self::Open { descriptor };
        true
    }

    fn complete_stat(&mut self, descriptor: u64) -> bool {
        if *self != (Self::Open { descriptor }) {
            return false;
        }
        *self = Self::Statted { descriptor };
        true
    }

    fn complete_map(
        &mut self,
        descriptor: u64,
        mapping: LoaderCacheMapping,
        expected_raw_length: u64,
        expected_identity: MappingIdentity,
    ) -> bool {
        if *self != (Self::Statted { descriptor })
            || mapping.raw_length != expected_raw_length
            || mapping.identity != expected_identity
        {
            return false;
        }
        *self = Self::MappedOpen {
            descriptor,
            mapping,
        };
        true
    }

    fn complete_close(&mut self, descriptor: u64, mapping: LoaderCacheMapping) -> bool {
        if *self
            != (Self::MappedOpen {
                descriptor,
                mapping,
            })
        {
            return false;
        }
        *self = Self::MappedClosed {
            descriptor,
            mapping,
        };
        true
    }

    fn complete_retire(&mut self, descriptor: u64, mapping: LoaderCacheMapping) -> bool {
        if *self
            != (Self::MappedClosed {
                descriptor,
                mapping,
            })
        {
            return false;
        }
        *self = Self::Retired {
            descriptor,
            mapping,
        };
        true
    }

    fn arm_scratch_release(&mut self, descriptor: u64, mapping: LoaderCacheMapping) -> bool {
        if *self
            != (Self::Retired {
                descriptor,
                mapping,
            })
        {
            return false;
        }
        *self = Self::ScratchReleaseArmed {
            descriptor,
            mapping,
        };
        true
    }

    fn complete_scratch_release(&mut self, descriptor: u64, mapping: LoaderCacheMapping) -> bool {
        if *self
            != (Self::ScratchReleaseArmed {
                descriptor,
                mapping,
            })
        {
            return false;
        }
        *self = Self::Released {
            descriptor,
            mapping,
        };
        true
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct LoaderCacheState {
    scratch_reservation: (u64, u64),
    scratch: (u64, u64),
    scratch_owner: AfterLoaderOwnedMapping,
    scratch_preimage: Vec<u8>,
    lifecycle: LoaderCacheLifecycle,
    aliases: BTreeMap<PathBuf, LoaderCacheAliasLifecycle>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct LoaderCacheAliasMapping {
    owned: AfterLoaderOwnedMapping,
    identity: MappingIdentity,
    raw_length: u64,
    fixed: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct LoaderCacheAliasFileMapStep {
    relative_start: u64,
    raw_length: u64,
    protection: i32,
    offset: u64,
    fixed: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct LoaderCacheAliasZeroFillStep {
    relative_start: u64,
    raw_length: u64,
    protection: i32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct LoaderCacheAliasMmapPlan {
    file_maps: Vec<LoaderCacheAliasFileMapStep>,
    zero_fill: LoaderCacheAliasZeroFillStep,
}

fn loader_cache_alias_load_protection(flags: u32) -> Option<i32> {
    (flags & !(ph::PF_R | ph::PF_W | ph::PF_X) == 0
        && flags & (ph::PF_W | ph::PF_X) != (ph::PF_W | ph::PF_X))
        .then_some(
            (if flags & ph::PF_R != 0 {
                libc::PROT_READ
            } else {
                0
            }) | (if flags & ph::PF_W != 0 {
                libc::PROT_WRITE
            } else {
                0
            }) | (if flags & ph::PF_X != 0 {
                libc::PROT_EXEC
            } else {
                0
            }),
        )
}

fn loader_cache_alias_mmap_plan(bytes: &[u8]) -> Result<LoaderCacheAliasMmapPlan, &'static str> {
    let elf = Elf::parse(bytes).map_err(|_| "profiled alias ELF parse failed")?;
    if !elf.is_64
        || !elf.little_endian
        || elf.header.e_machine != header::EM_X86_64
        || elf.header.e_type != header::ET_DYN
        || elf.program_headers.len() > 128
    {
        return Err("profiled alias ELF header differs");
    }
    let loads = elf
        .program_headers
        .iter()
        .filter(|program| program.p_type == ph::PT_LOAD)
        .collect::<Vec<_>>();
    if loads.len() < 2 {
        return Err("profiled alias lacks multiple PT_LOAD mappings");
    }
    if loads
        .windows(2)
        .any(|pair| page_down(pair[0].p_vaddr) >= page_down(pair[1].p_vaddr))
    {
        return Err("profiled alias PT_LOAD headers are out of order");
    }
    let file_length = bytes.len() as u64;
    for (index, load) in loads.iter().enumerate() {
        let data_end = load
            .p_vaddr
            .checked_add(load.p_filesz)
            .ok_or("profiled alias PT_LOAD data range overflow")?;
        let allocation_end = load
            .p_vaddr
            .checked_add(load.p_memsz)
            .ok_or("profiled alias PT_LOAD allocation range overflow")?;
        let file_end = load
            .p_offset
            .checked_add(load.p_filesz)
            .ok_or("profiled alias PT_LOAD file range overflow")?;
        let map_start = page_down(load.p_vaddr);
        let map_end = page_up(data_end).map_err(|_| "profiled alias PT_LOAD map overflow")?;
        if load.p_filesz == 0
            || load.p_filesz > load.p_memsz
            || file_end > file_length
            || load.p_vaddr % PAGE != load.p_offset % PAGE
            || load.p_align > 1
                && (!load.p_align.is_power_of_two()
                    || load.p_vaddr % load.p_align != load.p_offset % load.p_align)
            || loader_cache_alias_load_protection(load.p_flags).is_none()
            || map_start >= map_end
            || index + 1 < loads.len() && load.p_memsz != load.p_filesz
        {
            return Err("profiled alias PT_LOAD geometry differs");
        }
        if let Some(prior) = index.checked_sub(1).and_then(|prior| loads.get(prior)) {
            let prior_data_end = prior
                .p_vaddr
                .checked_add(prior.p_filesz)
                .ok_or("profiled alias prior PT_LOAD data range overflow")?;
            let prior_map_end =
                page_up(prior_data_end).map_err(|_| "profiled alias prior map overflow")?;
            let prior_allocation_end = prior
                .p_vaddr
                .checked_add(prior.p_memsz)
                .ok_or("profiled alias prior allocation range overflow")?;
            if prior_map_end != map_start
                || prior_allocation_end > load.p_vaddr
                || page_up(prior_data_end).map_err(|_| "profiled alias prior BSS overflow")?
                    < prior_allocation_end
            {
                return Err("profiled alias PT_LOAD sequence has a hole, overlap or early BSS map");
            }
        }
        if allocation_end < data_end {
            return Err("profiled alias PT_LOAD allocation precedes its file data");
        }
    }

    let first = loads[0];
    let last = *loads.last().ok_or("profiled alias lacks a final PT_LOAD")?;
    let first_start = page_down(first.p_vaddr);
    let last_allocation_end = last
        .p_vaddr
        .checked_add(last.p_memsz)
        .ok_or("profiled alias final allocation range overflow")?;
    let initial_length = last_allocation_end
        .checked_sub(first_start)
        .ok_or("profiled alias initial reservation underflow")?;
    let mut file_maps = vec![LoaderCacheAliasFileMapStep {
        relative_start: 0,
        raw_length: initial_length,
        protection: loader_cache_alias_load_protection(first.p_flags)
            .ok_or("profiled alias first PT_LOAD protection differs")?,
        offset: page_down(first.p_offset),
        fixed: false,
    }];
    for load in loads.iter().skip(1) {
        let start = page_down(load.p_vaddr);
        let data_end = load
            .p_vaddr
            .checked_add(load.p_filesz)
            .ok_or("profiled alias PT_LOAD data range overflow")?;
        let end = page_up(data_end).map_err(|_| "profiled alias PT_LOAD map overflow")?;
        file_maps.push(LoaderCacheAliasFileMapStep {
            relative_start: start
                .checked_sub(first_start)
                .ok_or("profiled alias fixed map precedes its reservation")?,
            raw_length: end
                .checked_sub(start)
                .ok_or("profiled alias fixed map length underflow")?,
            protection: loader_cache_alias_load_protection(load.p_flags)
                .ok_or("profiled alias PT_LOAD protection differs")?,
            offset: page_down(load.p_offset),
            fixed: true,
        });
    }
    let final_data_end = last
        .p_vaddr
        .checked_add(last.p_filesz)
        .ok_or("profiled alias final data range overflow")?;
    if final_data_end % PAGE != 0 {
        return Err("profiled alias final file data requires unrepresented partial-page zeroing");
    }
    let zero_start = page_up(final_data_end).map_err(|_| "profiled alias BSS start overflow")?;
    let zero_length = last_allocation_end
        .checked_sub(zero_start)
        .ok_or("profiled alias final BSS does not require one anonymous map")?;
    if zero_length == 0 {
        return Err("profiled alias final BSS does not require one anonymous map");
    }
    Ok(LoaderCacheAliasMmapPlan {
        file_maps,
        zero_fill: LoaderCacheAliasZeroFillStep {
            relative_start: zero_start
                .checked_sub(first_start)
                .ok_or("profiled alias BSS precedes its reservation")?,
            raw_length: zero_length,
            protection: loader_cache_alias_load_protection(last.p_flags)
                .ok_or("profiled alias BSS protection differs")?,
        },
    })
}

impl LoaderCacheAliasMmapPlan {
    fn file_request_is_exact(
        &self,
        index: usize,
        base: Option<u64>,
        descriptor: u64,
        args: [u64; 6],
    ) -> bool {
        let Some(step) = self.file_maps.get(index) else {
            return false;
        };
        let address = if step.fixed {
            let Some(address) = base.and_then(|base| base.checked_add(step.relative_start)) else {
                return false;
            };
            address
        } else {
            if index != 0 || base.is_some() {
                return false;
            }
            0
        };
        let flags =
            libc::MAP_PRIVATE | libc::MAP_DENYWRITE | if step.fixed { libc::MAP_FIXED } else { 0 };
        args[0] == address
            && args[1] == step.raw_length
            && canonical_c_int_argument_is(args[2], step.protection)
            && canonical_c_int_argument_is(args[3], flags)
            && args[4] == descriptor
            && args[5] == step.offset
    }

    fn zero_fill_request_is_exact(&self, base: Option<u64>, args: [u64; 6]) -> bool {
        let Some(address) = base.and_then(|base| base.checked_add(self.zero_fill.relative_start))
        else {
            return false;
        };
        args[0] == address
            && args[1] == self.zero_fill.raw_length
            && canonical_c_int_argument_is(args[2], self.zero_fill.protection)
            && canonical_c_int_argument_is(
                args[3],
                libc::MAP_PRIVATE | libc::MAP_ANONYMOUS | libc::MAP_FIXED,
            )
            && args[4] == u64::from(u32::MAX)
            && args[5] == 0
    }

    fn file_completion_is_exact(
        &self,
        index: usize,
        descriptor: u64,
        image: AfterLoaderImageId,
        prior: &[LoaderCacheAliasMapping],
        mapping: LoaderCacheAliasMapping,
    ) -> bool {
        let Some(step) = self.file_maps.get(index) else {
            return false;
        };
        let start = if step.fixed {
            let Some(start) = prior
                .first()
                .and_then(|first| first.owned.start.checked_add(step.relative_start))
            else {
                return false;
            };
            start
        } else {
            if index != 0 || !prior.is_empty() {
                return false;
            }
            mapping.owned.start
        };
        let Some((expected_start, expected_end)) =
            checked_page_effect_range(start, step.raw_length).ok()
        else {
            return false;
        };
        mapping.raw_length == step.raw_length
            && mapping.fixed == step.fixed
            && mapping.owned.start == expected_start
            && mapping.owned.end == expected_end
            && mapping.owned.readable == (step.protection & libc::PROT_READ != 0)
            && mapping.owned.writable == (step.protection & libc::PROT_WRITE != 0)
            && mapping.owned.executable == (step.protection & libc::PROT_EXEC != 0)
            && !mapping.owned.shared
            && mapping.owned.descriptor == Some(descriptor)
            && mapping.owned.offset == step.offset
            && mapping.owned.purpose == (AfterLoaderMappingPurpose::Image { image })
            && prior
                .first()
                .is_none_or(|first| first.identity == mapping.identity)
    }

    fn zero_fill_completion_is_exact(
        &self,
        image: AfterLoaderImageId,
        file_maps: &[LoaderCacheAliasMapping],
        raw_length: u64,
        mapping: AfterLoaderOwnedMapping,
    ) -> bool {
        let Some(start) = file_maps
            .first()
            .and_then(|first| first.owned.start.checked_add(self.zero_fill.relative_start))
        else {
            return false;
        };
        let Some((expected_start, expected_end)) =
            checked_page_effect_range(start, self.zero_fill.raw_length).ok()
        else {
            return false;
        };
        file_maps.len() == self.file_maps.len()
            && raw_length == self.zero_fill.raw_length
            && mapping.start == expected_start
            && mapping.end == expected_end
            && mapping.readable == (self.zero_fill.protection & libc::PROT_READ != 0)
            && mapping.writable == (self.zero_fill.protection & libc::PROT_WRITE != 0)
            && mapping.executable == (self.zero_fill.protection & libc::PROT_EXEC != 0)
            && !mapping.shared
            && mapping.descriptor.is_none()
            && mapping.offset == 0
            && mapping.purpose == (AfterLoaderMappingPurpose::ImageZeroFill { image })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum LoaderCacheAliasLifecycle {
    AwaitingOpen {
        image: AfterLoaderImageId,
    },
    Open {
        descriptor: u64,
        image: AfterLoaderImageId,
        read_end: u64,
    },
    Statted {
        descriptor: u64,
        image: AfterLoaderImageId,
    },
    Mapped {
        descriptor: u64,
        image: AfterLoaderImageId,
        mappings: Vec<LoaderCacheAliasMapping>,
        zero_fills: Vec<AfterLoaderOwnedMapping>,
    },
    MappedClosed {
        image: AfterLoaderImageId,
        mappings: Vec<LoaderCacheAliasMapping>,
        zero_fills: Vec<AfterLoaderOwnedMapping>,
        geometry: ResolvedImageGeometry,
    },
    Consumed {
        image: AfterLoaderImageId,
        mappings: Vec<LoaderCacheAliasMapping>,
        zero_fills: Vec<AfterLoaderOwnedMapping>,
        geometry: ResolvedImageGeometry,
    },
}

impl LoaderCacheAliasLifecycle {
    fn complete_open(&mut self, descriptor: u64, image: AfterLoaderImageId) -> bool {
        if *self != (Self::AwaitingOpen { image }) {
            return false;
        }
        *self = Self::Open {
            descriptor,
            image,
            read_end: 0,
        };
        true
    }

    fn complete_read(&mut self, descriptor: u64, start: u64, end: u64) -> bool {
        let Self::Open {
            descriptor: expected_descriptor,
            read_end,
            ..
        } = self
        else {
            return false;
        };
        if *expected_descriptor != descriptor
            || *read_end != 0
            || start != 0
            || end != LOADER_CACHE_ALIAS_HEADER_BYTES
        {
            return false;
        }
        *read_end = end;
        true
    }

    fn complete_stat(&mut self, descriptor: u64) -> bool {
        let Self::Open {
            descriptor: expected_descriptor,
            image,
            read_end,
        } = self
        else {
            return false;
        };
        if *expected_descriptor != descriptor || *read_end != LOADER_CACHE_ALIAS_HEADER_BYTES {
            return false;
        }
        *self = Self::Statted {
            descriptor,
            image: *image,
        };
        true
    }

    fn complete_map(
        &mut self,
        plan: &LoaderCacheAliasMmapPlan,
        descriptor: u64,
        image: AfterLoaderImageId,
        mapping: LoaderCacheAliasMapping,
    ) -> bool {
        match self {
            Self::Statted {
                descriptor: expected_descriptor,
                image: expected_image,
            } if *expected_descriptor == descriptor
                && *expected_image == image
                && plan.file_completion_is_exact(0, descriptor, image, &[], mapping) =>
            {
                *self = Self::Mapped {
                    descriptor,
                    image,
                    mappings: vec![mapping],
                    zero_fills: Vec::new(),
                };
                true
            }
            Self::Mapped {
                descriptor: expected_descriptor,
                image: expected_image,
                mappings,
                ..
            } if *expected_descriptor == descriptor
                && *expected_image == image
                && plan.file_completion_is_exact(
                    mappings.len(),
                    descriptor,
                    image,
                    mappings,
                    mapping,
                ) =>
            {
                mappings.push(mapping);
                true
            }
            _ => false,
        }
    }

    fn complete_zero_fill(
        &mut self,
        plan: &LoaderCacheAliasMmapPlan,
        image: AfterLoaderImageId,
        raw_length: u64,
        mapping: AfterLoaderOwnedMapping,
    ) -> bool {
        let Self::Mapped {
            image: expected_image,
            mappings,
            zero_fills,
            ..
        } = self
        else {
            return false;
        };
        if *expected_image != image
            || !zero_fills.is_empty()
            || !plan.zero_fill_completion_is_exact(image, mappings, raw_length, mapping)
        {
            return false;
        }
        zero_fills.push(mapping);
        true
    }

    fn complete_close(
        &mut self,
        plan: &LoaderCacheAliasMmapPlan,
        descriptor: u64,
        image: AfterLoaderImageId,
        geometry: ResolvedImageGeometry,
    ) -> bool {
        let Self::Mapped {
            descriptor: expected_descriptor,
            image: expected_image,
            mappings,
            zero_fills,
        } = self
        else {
            return false;
        };
        if *expected_descriptor != descriptor
            || *expected_image != image
            || geometry.image != image
            || mappings.len() != plan.file_maps.len()
            || zero_fills.len() != 1
            || mappings
                .iter()
                .any(|mapping| mapping.identity != geometry.mapping)
        {
            return false;
        }
        *self = Self::MappedClosed {
            image,
            mappings: mappings.clone(),
            zero_fills: zero_fills.clone(),
            geometry,
        };
        true
    }

    fn complete_relro(
        &mut self,
        image: AfterLoaderImageId,
        geometry: ResolvedImageGeometry,
    ) -> bool {
        let Self::MappedClosed {
            image: expected_image,
            mappings,
            zero_fills,
            geometry: pre_relro_geometry,
        } = self
        else {
            return false;
        };
        if *expected_image != image
            || geometry.image != image
            || *pre_relro_geometry != geometry
            || mappings.is_empty()
            || mappings
                .iter()
                .any(|mapping| mapping.identity != geometry.mapping)
        {
            return false;
        }
        *self = Self::Consumed {
            image,
            mappings: mappings.clone(),
            zero_fills: zero_fills.clone(),
            geometry,
        };
        true
    }

    fn is_consumed(&self) -> bool {
        matches!(self, Self::Consumed { .. })
    }
}

fn loader_cache_alias_next_step_is_exact(
    plan: &LoaderCacheAliasMmapPlan,
    lifecycle: &LoaderCacheAliasLifecycle,
    number: i64,
    args: [u64; 6],
) -> bool {
    match lifecycle {
        LoaderCacheAliasLifecycle::AwaitingOpen { .. }
        | LoaderCacheAliasLifecycle::Consumed { .. } => true,
        LoaderCacheAliasLifecycle::Open {
            descriptor,
            read_end: 0,
            ..
        } => number == libc::SYS_read && args[0] == *descriptor,
        LoaderCacheAliasLifecycle::Open {
            descriptor,
            read_end: LOADER_CACHE_ALIAS_HEADER_BYTES,
            ..
        } => number == libc::SYS_fstat && args[0] == *descriptor,
        LoaderCacheAliasLifecycle::Open { .. } => false,
        LoaderCacheAliasLifecycle::Statted { descriptor, .. } => {
            number == libc::SYS_mmap && plan.file_request_is_exact(0, None, *descriptor, args)
        }
        LoaderCacheAliasLifecycle::Mapped {
            descriptor,
            mappings,
            zero_fills,
            ..
        } if mappings.len() < plan.file_maps.len() => {
            number == libc::SYS_mmap
                && zero_fills.is_empty()
                && plan.file_request_is_exact(
                    mappings.len(),
                    mappings.first().map(|mapping| mapping.owned.start),
                    *descriptor,
                    args,
                )
        }
        LoaderCacheAliasLifecycle::Mapped {
            descriptor: _,
            mappings,
            zero_fills,
            ..
        } if mappings.len() == plan.file_maps.len() && zero_fills.is_empty() => {
            number == libc::SYS_mmap
                && plan.zero_fill_request_is_exact(
                    mappings.first().map(|mapping| mapping.owned.start),
                    args,
                )
        }
        LoaderCacheAliasLifecycle::Mapped {
            descriptor,
            mappings,
            zero_fills,
            ..
        } => {
            number == libc::SYS_close
                && args[0] == *descriptor
                && mappings.len() == plan.file_maps.len()
                && zero_fills.len() == 1
        }
        LoaderCacheAliasLifecycle::MappedClosed { .. } => number == libc::SYS_mprotect,
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct LoaderCacheRedirect {
    scratch: (u64, u64),
    preimage: Vec<u8>,
    path: Vec<u8>,
    admission_registers: [u64; 27],
    admission_xstate: safeptrace::X86ExtendedState,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum AfterLoaderMappingPurpose {
    Controller,
    CallbackStack,
    Image { image: AfterLoaderImageId },
    ImageZeroFill { image: AfterLoaderImageId },
    SharedReservation { trampoline: AfterLoaderTrampolineId },
    Trampoline { trampoline: AfterLoaderTrampolineId },
    LoaderCache,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct AfterLoaderOwnedMapping {
    start: u64,
    end: u64,
    readable: bool,
    writable: bool,
    executable: bool,
    shared: bool,
    descriptor: Option<u64>,
    offset: u64,
    purpose: AfterLoaderMappingPurpose,
}

fn loader_cache_scratch_owner_shape_is_exact(
    scratch: (u64, u64),
    owner: AfterLoaderOwnedMapping,
) -> bool {
    scratch.0 < scratch.1
        && owner.start % PAGE == 0
        && owner.end.checked_sub(owner.start) == Some(PAGE)
        && owner.start <= scratch.0
        && scratch.1 <= owner.end
        && scratch.1.checked_sub(scratch.0) == Some(LOADER_CACHE_SCRATCH_BYTES as u64)
        && owner.readable
        && owner.writable
        && !owner.executable
        && !owner.shared
        && owner.descriptor.is_none()
        && owner.offset == PAGE
        && owner.purpose == AfterLoaderMappingPurpose::Controller
}

fn loader_cache_scratch_metadata_is_exact(
    reservation: (u64, u64),
    scratch: (u64, u64),
    owner: AfterLoaderOwnedMapping,
) -> bool {
    let Some(lower_end) = reservation.0.checked_add(PAGE) else {
        return false;
    };
    let Some(middle_end) = lower_end.checked_add(PAGE) else {
        return false;
    };
    let Some(expected_end) = middle_end.checked_add(PAGE) else {
        return false;
    };
    let expected_scratch = (
        owner.start.checked_add(LOADER_CACHE_SCRATCH_OFFSET),
        owner.start.checked_add(LOADER_CACHE_SCRATCH_END_OFFSET),
    );
    reservation.1 == expected_end
        && owner.start == lower_end
        && owner.end == middle_end
        && expected_scratch == (Some(scratch.0), Some(scratch.1))
        && loader_cache_scratch_owner_shape_is_exact(scratch, owner)
}

fn loader_cache_scratch_logical_reservation_is_exact(
    reservation: (u64, u64),
    scratch: (u64, u64),
    owner: AfterLoaderOwnedMapping,
    mappings: &[AfterLoaderOwnedMapping],
) -> bool {
    if !loader_cache_scratch_metadata_is_exact(reservation, scratch, owner) {
        return false;
    }
    let lower_end = owner.start;
    let middle_end = owner.end;
    let lower = AfterLoaderOwnedMapping {
        start: reservation.0,
        end: lower_end,
        readable: false,
        writable: false,
        executable: false,
        shared: false,
        descriptor: None,
        offset: 0,
        purpose: AfterLoaderMappingPurpose::Controller,
    };
    let upper = AfterLoaderOwnedMapping {
        start: middle_end,
        end: reservation.1,
        offset: 2 * PAGE,
        ..lower
    };
    let mut overlapping = mappings
        .iter()
        .filter(|mapping| ranges_overlap((mapping.start, mapping.end), reservation))
        .copied()
        .collect::<Vec<_>>();
    overlapping.sort_by_key(|mapping| mapping.start);
    overlapping.as_slice() == &[lower, owner, upper]
}

fn loader_cache_scratch_physical_map_is_exact(
    owner: AfterLoaderOwnedMapping,
    mapping: &GuestMap,
    attributes: Option<GuestHookMappingAttributes>,
) -> bool {
    mapping.start == owner.start
        && mapping.end == owner.end
        && mapping.readable
        && mapping.writable
        && !mapping.executable
        && !mapping.shared
        && mapping.offset == 0
        && mapping.device_major == 0
        && mapping.device_minor == 0
        && mapping.inode == 0
        && mapping.path.is_none()
        && attributes
            .is_some_and(|attributes| attributes.fork_safe && attributes.protection_key == 0)
}

fn loader_cache_scratch_physical_guard_is_exact(
    guard: (u64, u64),
    lower: bool,
    mapping: &GuestMap,
    attributes: Option<GuestHookMappingAttributes>,
) -> bool {
    (if lower {
        mapping.start <= guard.0 && mapping.end == guard.1
    } else {
        mapping.start == guard.0 && guard.1 <= mapping.end
    }) && !mapping.readable
        && !mapping.writable
        && !mapping.executable
        && !mapping.shared
        && mapping.offset == 0
        && mapping.device_major == 0
        && mapping.device_minor == 0
        && mapping.inode == 0
        && mapping.path.is_none()
        && attributes
            .is_some_and(|attributes| attributes.fork_safe && attributes.protection_key == 0)
}

fn loader_cache_scratch_physical_reservation_is_exact(
    pid: Pid,
    reservation: (u64, u64),
    owner: AfterLoaderOwnedMapping,
    maps: &[GuestMap],
) -> bool {
    let lower = (reservation.0, owner.start);
    let upper = (owner.end, reservation.1);
    let covering = |range: (u64, u64)| {
        maps.iter()
            .filter(|mapping| mapping.start <= range.0 && range.1 <= mapping.end)
            .collect::<Vec<_>>()
    };
    let lower_maps = covering(lower);
    let middle_maps = covering((owner.start, owner.end));
    let upper_maps = covering(upper);
    lower_maps.len() == 1
        && middle_maps.len() == 1
        && upper_maps.len() == 1
        && loader_cache_scratch_physical_guard_is_exact(
            lower,
            true,
            lower_maps[0],
            guest_hook_mapping_attributes(pid, lower_maps[0]),
        )
        && loader_cache_scratch_physical_map_is_exact(
            owner,
            middle_maps[0],
            guest_hook_mapping_attributes(pid, middle_maps[0]),
        )
        && loader_cache_scratch_physical_guard_is_exact(
            upper,
            false,
            upper_maps[0],
            guest_hook_mapping_attributes(pid, upper_maps[0]),
        )
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum AfterLoaderSyscallEffect {
    None,
    PtmallocBootstrapEntropy {
        destination: u64,
        before: [u8; 8],
    },
    CetStatus {
        destination: u64,
        before: [u8; 8],
    },
    Open(AfterLoaderOwnedDescriptor),
    OpenLoaderCacheAlias(AfterLoaderOwnedDescriptor),
    OpenLoaderCache {
        descriptor: AfterLoaderOwnedDescriptor,
        redirect: LoaderCacheRedirect,
    },
    OpenProcFdDirectory {
        expected: BTreeMap<u64, Vec<u8>>,
    },
    ReadProcFdDirectory {
        descriptor: u64,
        destination: u64,
        capacity: u64,
    },
    ReadProcFdLink {
        directory: u64,
        descriptor: u64,
        destination: u64,
        expected: Vec<u8>,
    },
    CloseProcFdDirectory {
        descriptor: u64,
        remaining: BTreeMap<u64, Vec<u8>>,
    },
    StatLoaderCache {
        descriptor: u64,
        destination: u64,
        fields: AfterLoaderStatFields,
    },
    MapLoaderCache {
        descriptor: u64,
        raw_length: u64,
        identity: MappingIdentity,
    },
    CloseLoaderCache {
        descriptor: u64,
        mapping: LoaderCacheMapping,
    },
    RetireLoaderCache {
        descriptor: u64,
        mapping: LoaderCacheMapping,
    },
    Close(u64),
    Map {
        requested: u64,
        raw_length: u64,
        protection: i32,
        flags: i32,
        descriptor: Option<u64>,
        offset: u64,
        purpose: AfterLoaderMappingPurpose,
    },
    Protect {
        start: u64,
        raw_length: u64,
        protection: i32,
        loader_cache_alias: Option<(PathBuf, AfterLoaderImageId, ResolvedImageGeometry)>,
    },
    Remove {
        start: u64,
        raw_length: u64,
    },
    Resize {
        descriptor: u64,
        length: u64,
    },
    AddTrampolineSeals {
        descriptor: u64,
    },
    VerifyTrampolineSeals {
        descriptor: u64,
        trampoline: AfterLoaderTrampolineId,
    },
    Identity(Pid),
    Read {
        descriptor: u64,
        destination: u64,
        offset: u64,
        expected: Vec<u8>,
        advances: bool,
    },
    RecordBreak,
    CheckBreak(u64),
    AdvanceBreak {
        previous: u64,
        requested: u64,
        before_maps: Vec<GuestMap>,
    },
    ExpectedResult(i64),
    FutexWake {
        address: u64,
        word: [u8; 4],
    },
    Stat {
        descriptor: u64,
        destination: u64,
        fields: AfterLoaderStatFields,
    },
    StatLoaderCacheAlias {
        descriptor: u64,
        destination: u64,
        fields: AfterLoaderStatFields,
    },
    Statx {
        descriptor: u64,
        destination: u64,
        expected: [u8; STATX_OUTPUT_BYTES],
    },
}

fn private_memory_effect_result_is_exact(effect: &AfterLoaderSyscallEffect, result: i64) -> bool {
    !matches!(
        effect,
        AfterLoaderSyscallEffect::Protect { .. } | AfterLoaderSyscallEffect::Remove { .. }
    ) || result == 0
}

fn private_close_result_is_exact(result: i64) -> bool {
    result == 0
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum AfterLoaderSyscallCompletion {
    KernelResult(i64),
    UnsupportedCetStatus,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum AfterLoaderEmulatedCompletion {
    PtmallocBootstrapEntropy,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CetStatusCompletionError {
    OutputChanged,
    SyscallFailed(i64),
    NonzeroSuccess(i64),
}

fn complete_cet_status_query(
    effect: &AfterLoaderSyscallEffect,
    raw_result: i64,
    after: [u8; 8],
) -> Option<Result<AfterLoaderSyscallCompletion, CetStatusCompletionError>> {
    let AfterLoaderSyscallEffect::CetStatus { before, .. } = effect else {
        return None;
    };
    Some(if raw_result == -(libc::EINVAL as i64) {
        if after == *before {
            Ok(AfterLoaderSyscallCompletion::UnsupportedCetStatus)
        } else {
            Err(CetStatusCompletionError::OutputChanged)
        }
    } else if raw_result < 0 {
        Err(CetStatusCompletionError::SyscallFailed(raw_result))
    } else if raw_result == 0 {
        Ok(AfterLoaderSyscallCompletion::KernelResult(raw_result))
    } else {
        Err(CetStatusCompletionError::NonzeroSuccess(raw_result))
    })
}

fn caller_private_syscall_result_is_accepted(completion: AfterLoaderSyscallCompletion) -> bool {
    match completion {
        AfterLoaderSyscallCompletion::KernelResult(raw_result) => {
            raw_result < -4095 || raw_result >= 0
        }
        AfterLoaderSyscallCompletion::UnsupportedCetStatus => true,
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct AfterLoaderStatFields {
    device: u64,
    inode: u64,
    mode: u32,
    links: u64,
    uid: u32,
    gid: u32,
    rdev: u64,
    size: i64,
    block_size: i64,
    blocks: i64,
    access_seconds: i64,
    access_nanoseconds: i64,
    modify_seconds: i64,
    modify_nanoseconds: i64,
    change_seconds: i64,
    change_nanoseconds: i64,
}

impl AfterLoaderStatFields {
    fn from_metadata(metadata: &std::fs::Metadata) -> Self {
        Self {
            device: metadata.dev(),
            inode: metadata.ino(),
            mode: metadata.mode(),
            links: metadata.nlink(),
            uid: metadata.uid(),
            gid: metadata.gid(),
            rdev: metadata.rdev(),
            size: metadata.size() as i64,
            block_size: metadata.blksize() as i64,
            blocks: metadata.blocks() as i64,
            access_seconds: metadata.atime(),
            access_nanoseconds: metadata.atime_nsec(),
            modify_seconds: metadata.mtime(),
            modify_nanoseconds: metadata.mtime_nsec(),
            change_seconds: metadata.ctime(),
            change_nanoseconds: metadata.ctime_nsec(),
        }
    }

    fn deterministic_loader_cache_alias(metadata: &std::fs::Metadata) -> Self {
        Self::from_metadata(metadata).with_deterministic_access_time()
    }

    /// The profiled real-file alias has an explicit private-loader ABI: its
    /// `fstat` reports `st_atime == st_mtime`. The backing mount is `relatime`
    /// and the mandatory header read can otherwise inject wall time into the
    /// loader's private result without changing the authenticated stamp. This
    /// is deterministic syscall virtualization, not a claim that ambient Linux
    /// metadata is byte-identical or a relaxation of later guest comparators.
    fn with_deterministic_access_time(mut self) -> Self {
        self.access_seconds = self.modify_seconds;
        self.access_nanoseconds = self.modify_nanoseconds;
        self
    }

    fn exact_x86_64_output(self) -> [u8; 144] {
        let mut bytes = [0_u8; 144];
        bytes[0..8].copy_from_slice(&self.device.to_ne_bytes());
        bytes[8..16].copy_from_slice(&self.inode.to_ne_bytes());
        bytes[16..24].copy_from_slice(&self.links.to_ne_bytes());
        bytes[24..28].copy_from_slice(&self.mode.to_ne_bytes());
        bytes[28..32].copy_from_slice(&self.uid.to_ne_bytes());
        bytes[32..36].copy_from_slice(&self.gid.to_ne_bytes());
        bytes[40..48].copy_from_slice(&self.rdev.to_ne_bytes());
        bytes[48..56].copy_from_slice(&self.size.to_ne_bytes());
        bytes[56..64].copy_from_slice(&self.block_size.to_ne_bytes());
        bytes[64..72].copy_from_slice(&self.blocks.to_ne_bytes());
        bytes[72..80].copy_from_slice(&self.access_seconds.to_ne_bytes());
        bytes[80..88].copy_from_slice(&self.access_nanoseconds.to_ne_bytes());
        bytes[88..96].copy_from_slice(&self.modify_seconds.to_ne_bytes());
        bytes[96..104].copy_from_slice(&self.modify_nanoseconds.to_ne_bytes());
        bytes[104..112].copy_from_slice(&self.change_seconds.to_ne_bytes());
        bytes[112..120].copy_from_slice(&self.change_nanoseconds.to_ne_bytes());
        bytes
    }
}

fn stat_output_matches(fields: AfterLoaderStatFields, bytes: &[u8; 144]) -> bool {
    *bytes == fields.exact_x86_64_output()
}

#[derive(Debug)]
pub(super) struct AfterLoaderPrivateState {
    image: ImageIdentity,
    original_mappings: Vec<(u64, u64)>,
    original_descriptors: BTreeSet<u64>,
    owned_descriptors: BTreeMap<u64, AfterLoaderOwnedDescriptor>,
    proc_fd_audit: ProcFdAuditLifecycle,
    owned_mappings: Vec<AfterLoaderOwnedMapping>,
    current_break: Option<u64>,
    private_brk_growth_consumed: bool,
    shared_reservations: BTreeMap<AfterLoaderTrampolineId, ((u64, u64), SharedReservationIdentity)>,
    protected_ranges: Vec<(u64, u64)>,
    image_mappings: BTreeMap<AfterLoaderImageId, MappingIdentity>,
    image_geometries: BTreeMap<AfterLoaderImageId, ResolvedImageGeometry>,
    trampoline_mappings: BTreeMap<AfterLoaderTrampolineId, MappingIdentity>,
    trampoline_seals_added: BTreeSet<u64>,
    sealed_trampolines: BTreeSet<AfterLoaderTrampolineId>,
    next_trampoline_serial: u64,
    ptmalloc_bootstrap: Option<PtmallocBootstrapState>,
    loader_cache: Option<LoaderCacheState>,
    timer_suspension: Option<PrivateExecutionTimerSuspension>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ProcFdAuditLifecycle {
    AwaitingOpen,
    Scanning { descriptor: u64 },
    Complete,
}

#[derive(Debug, Eq, PartialEq)]
enum PrivateTimerRestoreOwnershipError<E> {
    Restore(E),
    MissingToken,
}

/// Applies the timer API's ownership transition without interpreting any
/// later validation result. A failed restore retains terminal-retirement
/// authority; a successful restore consumes it before any subsequent fallible
/// observation can return.
fn settle_private_timer_restore<T, E>(
    token: &mut Option<T>,
    restore: Result<(), E>,
) -> Result<(), PrivateTimerRestoreOwnershipError<E>> {
    match restore {
        Err(error) => Err(PrivateTimerRestoreOwnershipError::Restore(error)),
        Ok(()) => {
            drop(
                token
                    .take()
                    .ok_or(PrivateTimerRestoreOwnershipError::MissingToken)?,
            );
            Ok(())
        }
    }
}

#[cfg(test)]
impl Clone for AfterLoaderPrivateState {
    fn clone(&self) -> Self {
        assert!(
            self.timer_suspension.is_none(),
            "test state clone must not duplicate private timer ownership"
        );
        Self {
            image: self.image,
            original_mappings: self.original_mappings.clone(),
            original_descriptors: self.original_descriptors.clone(),
            owned_descriptors: self.owned_descriptors.clone(),
            proc_fd_audit: self.proc_fd_audit,
            owned_mappings: self.owned_mappings.clone(),
            current_break: self.current_break,
            private_brk_growth_consumed: self.private_brk_growth_consumed,
            shared_reservations: self.shared_reservations.clone(),
            protected_ranges: self.protected_ranges.clone(),
            image_mappings: self.image_mappings.clone(),
            image_geometries: self.image_geometries.clone(),
            trampoline_mappings: self.trampoline_mappings.clone(),
            trampoline_seals_added: self.trampoline_seals_added.clone(),
            sealed_trampolines: self.sealed_trampolines.clone(),
            next_trampoline_serial: self.next_trampoline_serial,
            ptmalloc_bootstrap: self.ptmalloc_bootstrap,
            loader_cache: self.loader_cache.clone(),
            timer_suspension: None,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum AfterLoaderSyscallPurpose {
    TraceePreinit,
    ToolInjection,
    PatchProtection,
    PrivateSetup,
    GuestRtSigreturn,
    OrdinaryForward,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct AfterLoaderSyscallPermit {
    image: Option<ImageIdentity>,
    tid: Pid,
    generation: u64,
    physical_generation: Option<safeptrace::PhysicalEventGenerationId>,
    origin_status: Option<safeptrace::PhysicalStatusId>,
    admission_status: Option<safeptrace::PhysicalStatusId>,
    pub(super) purpose: AfterLoaderSyscallPurpose,
    pub(super) number: i64,
    /// Exact arguments presented by the profiled guest at seccomp admission.
    pub(super) args: [u64; 6],
    /// Exact arguments presented to the kernel. These differ from `args` only
    /// for the one authenticated loader-cache pathname redirection.
    executed_args: [u64; 6],
    instruction_pointer: u64,
    resume_pointer: u64,
    instruction: [u8; 4],
    instruction_length: u8,
    output_spans: Vec<(u64, u64)>,
    effect: AfterLoaderSyscallEffect,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct AfterLoaderPrivateCall {
    image: ImageIdentity,
    function: CallerFunction,
    origin_status: safeptrace::PhysicalStatusId,
    entry: u64,
    return_rip: u64,
    call_stack_top: u64,
    code_bytes: [u8; 30],
    arguments: [u64; 2],
    calls_phase: CallsPhase,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct AfterLoaderTraceOnlySyscall {
    pub(super) purpose: AfterLoaderSyscallPurpose,
    pub(super) number: i64,
    pub(super) args: [u64; 6],
    pub(super) known: Option<Sysno>,
}

pub(super) enum AfterLoaderForwardKind {
    TraceOnly(AfterLoaderTraceOnlySyscall),
}

pub(super) struct AfterLoaderForwardInFlight {
    tid: Pid,
    generation: u64,
    image: Option<ImageIdentity>,
    physical_generation: safeptrace::PhysicalEventGenerationId,
    observed_statuses: BTreeSet<safeptrace::PhysicalStatusId>,
    instruction_pointer: u64,
    resume_pointer: u64,
    instruction: [u8; 2],
    number: i64,
    args: [u64; 6],
    kind: AfterLoaderForwardKind,
}

pub(super) enum AfterLoaderForwardRoute {
    Continue(Stopped),
    Completed(Wait),
}

fn checked_output_span(address: u64, length: u64) -> Result<Option<(u64, u64)>, Errno> {
    if length == 0 {
        return Ok(None);
    }
    let end = address.checked_add(length).ok_or(Errno::EOVERFLOW)?;
    if address == 0 {
        return Err(Errno::EFAULT);
    }
    Ok(Some((address, end)))
}

fn syscall_output_spans(number: i64, args: [u64; 6]) -> Result<Vec<(u64, u64)>, Errno> {
    let span = match number {
        libc::SYS_read | libc::SYS_pread64 => checked_output_span(args[1], args[2])?,
        libc::SYS_getdents64 => checked_output_span(args[1], args[2])?,
        libc::SYS_readlinkat => checked_output_span(args[2], args[3])?,
        libc::SYS_getrandom => checked_output_span(args[0], args[1])?,
        libc::SYS_fstat => checked_output_span(args[1], 144)?,
        libc::SYS_newfstatat => checked_output_span(args[2], 144)?,
        libc::SYS_statx => checked_output_span(args[4], STATX_OUTPUT_BYTES as u64)?,
        libc::SYS_arch_prctl if args[0] == ARCH_SHSTK_STATUS => checked_output_span(args[1], 8)?,
        libc::SYS_sigaltstack if args[0] == 0 => checked_output_span(args[1], 24)?,
        libc::SYS_rt_sigaction if args[1] == 0 => checked_output_span(args[2], 32)?,
        libc::SYS_rt_sigprocmask if args[1] == 0 => checked_output_span(args[2], args[3])?,
        _ => None,
    };
    Ok(span.into_iter().collect())
}

fn private_syscall_allowed(number: i64) -> bool {
    matches!(
        number,
        libc::SYS_openat
            | libc::SYS_close
            | libc::SYS_read
            | libc::SYS_pread64
            | libc::SYS_getdents64
            | libc::SYS_readlinkat
            | libc::SYS_fstat
            | libc::SYS_newfstatat
            | libc::SYS_statx
            | libc::SYS_mmap
            | libc::SYS_mprotect
            | libc::SYS_munmap
            | libc::SYS_brk
            | libc::SYS_futex
            | libc::SYS_memfd_create
            | libc::SYS_ftruncate
            | libc::SYS_getpid
            | libc::SYS_gettid
            | libc::SYS_fcntl
    )
}

fn exact_ptmalloc_bootstrap_request(
    load_bias: u64,
    admission_rax: u64,
    permit: &AfterLoaderSyscallPermit,
) -> bool {
    let Some(destination) = load_bias.checked_add(PTMALLOC_BOOTSTRAP_TCACHE_KEY_RVA) else {
        return false;
    };
    let Some(syscall) = load_bias.checked_add(PTMALLOC_BOOTSTRAP_SYSCALL_RVA) else {
        return false;
    };
    let Some(resume) = syscall.checked_add(2) else {
        return false;
    };
    let Some(output_end) = destination.checked_add(8) else {
        return false;
    };
    admission_rax as i64 == -(libc::ENOSYS as i64)
        && permit.number == libc::SYS_getrandom
        && permit.args[0] == destination
        && permit.args[1] == 8
        && permit.args[2] == libc::GRND_NONBLOCK as u64
        && permit.instruction_pointer == syscall
        && permit.resume_pointer == resume
        && permit.instruction_length == 2
        && permit.instruction[..2] == [0x0f, 0x05]
        && permit.output_spans == [(destination, output_end)]
}

fn exact_ptmalloc_bootstrap_admission_state(
    profile: PtmallocBootstrapState,
    observed_load_bias: u64,
    observed: PtmallocBootstrapObservation,
) -> bool {
    profile.pre_dlopen_verified
        && profile.entry
            == (PtmallocBootstrapObservation {
                initialized: 0,
                tcache_key: [0; 8],
            })
        && !profile.entropy_consumed
        && profile.injected_key.is_none()
        && observed_load_bias == profile.load_bias
        && observed.initialized == 1
        && observed.tcache_key == [0; 8]
}

fn exact_ptmalloc_brk_growth_request(
    profile: PtmallocBootstrapState,
    observed_load_bias: u64,
    observed: PtmallocBootstrapObservation,
    permit: &AfterLoaderSyscallPermit,
) -> bool {
    let Some(syscall) = profile.load_bias.checked_add(PTMALLOC_BRK_SYSCALL_RVA) else {
        return false;
    };
    let Some(resume) = syscall.checked_add(2) else {
        return false;
    };
    profile.pre_dlopen_verified
        && profile.entry.initialized == 0
        && profile.entropy_consumed
        && profile.injected_key.is_some()
        && observed_load_bias == profile.load_bias
        && observed.initialized == 1
        && Some(observed.tcache_key) == profile.injected_key
        && permit.number == libc::SYS_brk
        && permit.instruction_pointer == syscall
        && permit.resume_pointer == resume
        && permit.instruction_length == 2
        && permit.instruction[..2] == [0x0f, 0x05]
        && permit.output_spans.is_empty()
}

fn checked_range(start: u64, length: u64) -> Result<(u64, u64), Errno> {
    if length == 0 {
        return Err(Errno::EINVAL);
    }
    Ok((start, start.checked_add(length).ok_or(Errno::EOVERFLOW)?))
}

fn ranges_overlap(left: (u64, u64), right: (u64, u64)) -> bool {
    left.0 < right.1 && right.0 < left.1
}

impl AfterLoaderPrivateState {
    fn new(task: &Stopped, image: ImageIdentity) -> Result<Self, TraceError> {
        let mappings = guest_maps(task.pid()).ok_or(Errno::EPROTO)?;
        let original_mappings = mappings
            .into_iter()
            .map(|mapping| (mapping.start, mapping.end))
            .collect();
        let original_descriptors = descriptor_state(task.pid())
            .map_err(|_| Errno::EPROTO)?
            .into_keys()
            .map(u64::from)
            .collect();
        Ok(Self {
            image,
            original_mappings,
            original_descriptors,
            owned_descriptors: BTreeMap::new(),
            proc_fd_audit: ProcFdAuditLifecycle::AwaitingOpen,
            owned_mappings: Vec::new(),
            current_break: None,
            private_brk_growth_consumed: false,
            shared_reservations: BTreeMap::new(),
            protected_ranges: Vec::new(),
            image_mappings: BTreeMap::new(),
            image_geometries: BTreeMap::new(),
            trampoline_mappings: BTreeMap::new(),
            trampoline_seals_added: BTreeSet::new(),
            sealed_trampolines: BTreeSet::new(),
            next_trampoline_serial: 0,
            ptmalloc_bootstrap: None,
            loader_cache: None,
            timer_suspension: None,
        })
    }

    fn image_id(&self, image: &LiteinstCallerImage) -> AfterLoaderImageId {
        AfterLoaderImageId {
            generation: self.image.generation,
            file: image.file_identity,
        }
    }

    pub(super) fn image_mapping_identity(
        &self,
        image: &LiteinstCallerImage,
    ) -> Option<MappingIdentity> {
        self.image_mappings.get(&self.image_id(image)).copied()
    }

    fn image_geometry(&self, image: &LiteinstCallerImage) -> Option<ResolvedImageGeometry> {
        self.image_geometries.get(&self.image_id(image)).copied()
    }

    fn has_causal_image_mapping(&self, geometry: ResolvedImageGeometry) -> bool {
        self.image_mappings.get(&geometry.image).copied() == Some(geometry.mapping)
    }

    fn bind_image_mapping(&mut self, image: AfterLoaderImageId, mapping: MappingIdentity) -> bool {
        if image.generation != self.image.generation || mapping.inode == 0 {
            return false;
        }
        if let Some(existing) = self.image_mappings.get(&image) {
            return *existing == mapping;
        }
        if self
            .image_mappings
            .iter()
            .any(|(other, existing)| *other != image && *existing == mapping)
            || self
                .trampoline_mappings
                .values()
                .any(|existing| *existing == mapping)
        {
            return false;
        }
        self.image_mappings.insert(image, mapping);
        true
    }

    fn bind_image_geometry(&mut self, geometry: ResolvedImageGeometry) -> bool {
        if geometry.span.0 >= geometry.span.1
            || !self.bind_image_mapping(geometry.image, geometry.mapping)
        {
            return false;
        }
        match self.image_geometries.get(&geometry.image) {
            Some(existing) => *existing == geometry,
            None => {
                self.image_geometries.insert(geometry.image, geometry);
                true
            }
        }
    }

    fn next_trampoline_id(
        &mut self,
        file: crate::after_loader::FileIdentity,
    ) -> Option<AfterLoaderTrampolineId> {
        let serial = self.next_trampoline_serial;
        self.next_trampoline_serial = serial.checked_add(1)?;
        Some(AfterLoaderTrampolineId {
            generation: self.image.generation,
            serial,
            file,
        })
    }

    fn trampoline_mapping(&self, trampoline: AfterLoaderTrampolineId) -> Option<MappingIdentity> {
        self.trampoline_mappings.get(&trampoline).copied()
    }

    fn bind_trampoline_mapping(
        &mut self,
        trampoline: AfterLoaderTrampolineId,
        mapping: MappingIdentity,
    ) -> bool {
        if trampoline.generation != self.image.generation || mapping.inode == 0 {
            return false;
        }
        if let Some(existing) = self.trampoline_mappings.get(&trampoline) {
            return *existing == mapping;
        }
        if self
            .trampoline_mappings
            .iter()
            .any(|(other, existing)| *other != trampoline && *existing == mapping)
            || self
                .image_mappings
                .values()
                .any(|existing| *existing == mapping)
        {
            return false;
        }
        self.trampoline_mappings.insert(trampoline, mapping);
        true
    }

    fn trampoline_awaiting_shared_reservation(&self) -> Option<AfterLoaderTrampolineId> {
        let mut candidate = None;
        for descriptor in self.owned_descriptors.values() {
            let AfterLoaderOwnedDescriptor::Trampoline {
                id: Some(trampoline),
                size: Some(size),
            } = descriptor
            else {
                continue;
            };
            if *size != TRAMPOLINE_ARENA_SIZE
                || self.trampoline_mapping(*trampoline).is_none()
                || self.shared_reservations.contains_key(trampoline)
                || self.owned_mappings.iter().any(|mapping| {
                    mapping.purpose
                        == (AfterLoaderMappingPurpose::SharedReservation {
                            trampoline: *trampoline,
                        })
                })
            {
                continue;
            }
            let aliases = self
                .owned_mappings
                .iter()
                .filter(|mapping| {
                    mapping.purpose
                        == AfterLoaderMappingPurpose::Trampoline {
                            trampoline: *trampoline,
                        }
                })
                .collect::<Vec<_>>();
            if aliases.len() != 2
                || aliases.iter().filter(|mapping| mapping.writable).count() != 1
                || aliases.iter().filter(|mapping| mapping.executable).count() != 1
            {
                continue;
            }
            if candidate.replace(*trampoline).is_some() {
                return None;
            }
        }
        candidate
    }

    fn owns_descriptor(&self, descriptor: u64) -> bool {
        self.owned_descriptors.contains_key(&descriptor)
            && !self.original_descriptors.contains(&descriptor)
    }

    fn owns_range(&self, range: (u64, u64), writable: bool) -> bool {
        self.owned_mappings.iter().any(|mapping| {
            mapping.start <= range.0 && range.1 <= mapping.end && (!writable || mapping.writable)
        })
    }

    fn loader_cache_scratch_overlaps(&self, range: (u64, u64)) -> bool {
        self.loader_cache.as_ref().is_some_and(|cache| {
            !matches!(cache.lifecycle, LoaderCacheLifecycle::Released { .. })
                && ranges_overlap(cache.scratch_reservation, range)
        })
    }

    fn loader_cache_scratch_release_is_armed_for(&self, args: [u64; 6], range: (u64, u64)) -> bool {
        self.loader_cache.as_ref().is_some_and(|cache| {
            range == cache.scratch_reservation
                && args == [cache.scratch_reservation.0, 3 * PAGE, 0, 0, 0, 0]
                && matches!(
                    cache.lifecycle,
                    LoaderCacheLifecycle::ScratchReleaseArmed { .. }
                )
        })
    }

    fn callback_stack_overlaps(&self, range: (u64, u64)) -> bool {
        self.owned_mappings.iter().any(|mapping| {
            mapping.purpose == AfterLoaderMappingPurpose::CallbackStack
                && ranges_overlap((mapping.start, mapping.end), range)
        })
    }

    fn callback_stack_protect_transition_is_exact(
        &self,
        range: (u64, u64),
        raw_length: u64,
        protection: i32,
    ) -> bool {
        let mut mappings = self
            .owned_mappings
            .iter()
            .filter(|mapping| mapping.purpose == AfterLoaderMappingPurpose::CallbackStack);
        let Some(mapping) = mappings.next() else {
            return false;
        };
        let Some(mapping_len) = mapping.end.checked_sub(mapping.start) else {
            return false;
        };
        let Some(usable_len) = mapping_len.checked_sub(2 * PAGE) else {
            return false;
        };
        mappings.next().is_none()
            && callback_stack_usable_length_is_admissible(usable_len)
            && !mapping.readable
            && !mapping.writable
            && !mapping.executable
            && !mapping.shared
            && mapping.descriptor.is_none()
            && mapping.offset == 0
            && raw_length == usable_len
            && mapping.start.checked_add(PAGE) == Some(range.0)
            && range.0.checked_add(usable_len) == Some(range.1)
            && range.1.checked_add(PAGE) == Some(mapping.end)
            && protection == (libc::PROT_READ | libc::PROT_WRITE)
    }

    fn protect_callback_stack(&mut self, range: (u64, u64)) -> bool {
        let Some(raw_length) = range.1.checked_sub(range.0) else {
            return false;
        };
        if !self.callback_stack_protect_transition_is_exact(
            range,
            raw_length,
            libc::PROT_READ | libc::PROT_WRITE,
        ) {
            return false;
        }
        let index = self
            .owned_mappings
            .iter()
            .position(|mapping| mapping.purpose == AfterLoaderMappingPurpose::CallbackStack)
            .expect("exact callback-stack transition has one mapping");
        let mapping = self.owned_mappings.remove(index);
        self.owned_mappings.extend([
            AfterLoaderOwnedMapping {
                end: range.0,
                ..mapping
            },
            AfterLoaderOwnedMapping {
                start: range.0,
                end: range.1,
                readable: true,
                writable: true,
                offset: 0,
                ..mapping
            },
            AfterLoaderOwnedMapping {
                start: range.1,
                offset: 0,
                ..mapping
            },
        ]);
        true
    }

    pub(super) fn callback_stack_range(&self) -> Option<GuestRange> {
        let mut mappings = self
            .owned_mappings
            .iter()
            .filter(|mapping| mapping.purpose == AfterLoaderMappingPurpose::CallbackStack)
            .collect::<Vec<_>>();
        mappings.sort_by_key(|mapping| mapping.start);
        let [lower, usable, upper] = mappings.as_slice() else {
            return None;
        };
        let common_shape = |mapping: &AfterLoaderOwnedMapping| {
            !mapping.executable && !mapping.shared && mapping.descriptor.is_none()
        };
        let usable_len = usable.end.checked_sub(usable.start)?;
        (common_shape(lower)
            && common_shape(usable)
            && common_shape(upper)
            && !lower.readable
            && !lower.writable
            && lower.end - lower.start == PAGE
            && lower.offset == 0
            && usable.readable
            && usable.writable
            && callback_stack_usable_length_is_admissible(usable_len)
            && usable.offset == 0
            && !upper.readable
            && !upper.writable
            && upper.end - upper.start == PAGE
            && upper.offset == 0
            && lower.end == usable.start
            && usable.end == upper.start)
            .then(|| GuestRange::new(usable.start, usable_len))
            .flatten()
    }

    fn controller_call_stack_range(&self) -> Option<GuestRange> {
        let mut mappings = self
            .owned_mappings
            .iter()
            .filter(|mapping| mapping.purpose == AfterLoaderMappingPurpose::Controller)
            .collect::<Vec<_>>();
        mappings.sort_by_key(|mapping| mapping.start);
        let candidates = mappings
            .windows(3)
            .filter_map(|window| {
                let [lower, usable, upper] = window else {
                    return None;
                };
                let anonymous_private = |mapping: &AfterLoaderOwnedMapping| {
                    !mapping.executable && !mapping.shared && mapping.descriptor.is_none()
                };
                (anonymous_private(lower)
                    && anonymous_private(usable)
                    && anonymous_private(upper)
                    && !lower.readable
                    && !lower.writable
                    && lower.end.checked_sub(lower.start) == Some(PAGE)
                    && lower.offset == 0
                    && usable.readable
                    && usable.writable
                    && usable.end.checked_sub(usable.start) == Some(STACK_SIZE)
                    && usable.offset == PAGE
                    && !upper.readable
                    && !upper.writable
                    && upper.end.checked_sub(upper.start) == Some(PAGE)
                    && upper.offset == PAGE + STACK_SIZE
                    && lower.end == usable.start
                    && usable.end == upper.start)
                    .then(|| GuestRange::new(usable.start, STACK_SIZE))
                    .flatten()
            })
            .collect::<Vec<_>>();
        let [range] = candidates.as_slice() else {
            return None;
        };
        Some(*range)
    }

    fn owns_same_image_range(&self, range: (u64, u64), image: AfterLoaderImageId) -> bool {
        if range.0 >= range.1 {
            return false;
        }
        for (index, mapping) in self.owned_mappings.iter().enumerate() {
            let mapped = (mapping.start, mapping.end);
            if !ranges_overlap(mapped, range) {
                continue;
            }
            if !matches!(
                mapping.purpose,
                AfterLoaderMappingPurpose::Image { image: owned }
                    | AfterLoaderMappingPurpose::ImageZeroFill { image: owned }
                    if owned == image
            ) {
                return false;
            }
            let clipped = (mapping.start.max(range.0), mapping.end.min(range.1));
            if self.owned_mappings[..index].iter().any(|prior| {
                ranges_overlap((prior.start.max(range.0), prior.end.min(range.1)), clipped)
            }) {
                return false;
            }
        }

        let mut cursor = range.0;
        while cursor < range.1 {
            let Some(mapping) = self
                .owned_mappings
                .iter()
                .find(|mapping| mapping.start <= cursor && cursor < mapping.end)
            else {
                return false;
            };
            cursor = mapping.end.min(range.1);
        }
        true
    }

    fn shared_reservation_unmap_is_exact(&self, range: (u64, u64)) -> bool {
        self.shared_reservations
            .values()
            .all(|(reservation, _)| !ranges_overlap(*reservation, range) || *reservation == range)
    }

    fn trampoline_close_shape(
        &self,
        descriptor: u64,
        trampoline: AfterLoaderTrampolineId,
        size: u64,
    ) -> Option<TrampolineCloseShape> {
        let aliases = self
            .owned_mappings
            .iter()
            .filter(|mapping| {
                mapping.purpose == AfterLoaderMappingPurpose::Trampoline { trampoline }
            })
            .collect::<Vec<_>>();
        let reservations = self
            .owned_mappings
            .iter()
            .filter(|mapping| {
                mapping.purpose == (AfterLoaderMappingPurpose::SharedReservation { trampoline })
            })
            .collect::<Vec<_>>();
        let reservation_binding = self.shared_reservations.get(&trampoline);
        let complete = self.trampoline_mapping(trampoline).is_some()
            && aliases.len() == 2
            && aliases.iter().all(|mapping| {
                mapping.end - mapping.start == size
                    && mapping.offset == 0
                    && mapping.readable
                    && mapping.shared
                    && mapping.descriptor == Some(descriptor)
            })
            && aliases
                .iter()
                .filter(|mapping| mapping.writable && !mapping.executable)
                .count()
                == 1
            && aliases
                .iter()
                .filter(|mapping| !mapping.writable && mapping.executable)
                .count()
                == 1
            && !ranges_overlap(
                (aliases[0].start, aliases[0].end),
                (aliases[1].start, aliases[1].end),
            )
            && reservations.len() == 1
            && reservation_binding.is_some_and(|(range, identity)| {
                *range == (reservations[0].start, reservations[0].end)
                    && shared_reservation_identity_is_exact(identity)
            })
            && reservations[0].end - reservations[0].start == PAGE
            && reservations[0].readable
            && reservations[0].writable
            && !reservations[0].executable
            && reservations[0].shared
            && reservations[0].descriptor.is_none()
            && reservations[0].offset == 0;
        if complete {
            Some(TrampolineCloseShape::Complete)
        } else if aliases.is_empty() && reservations.is_empty() && reservation_binding.is_none() {
            Some(TrampolineCloseShape::Abandoned)
        } else {
            None
        }
    }

    fn runtime_futex_mapping(&self, range: (u64, u64), runtime: &LiteinstCallerImage) -> bool {
        let image = self.image_id(runtime);
        self.owned_mappings.iter().any(|mapping| {
            mapping.start <= range.0
                && range.1 <= mapping.end
                && mapping.writable
                && !mapping.executable
                && !mapping.shared
                && matches!(
                    mapping.purpose,
                    AfterLoaderMappingPurpose::Image { image: mapped }
                        | AfterLoaderMappingPurpose::ImageZeroFill { image: mapped }
                        if mapped == image
                )
        })
    }

    fn refuses_original_overlap(&self, range: (u64, u64)) -> bool {
        self.original_mappings
            .iter()
            .copied()
            .any(|original| ranges_overlap(original, range))
    }

    fn refuses_protected_overlap(&self, range: (u64, u64)) -> bool {
        self.protected_ranges
            .iter()
            .copied()
            .any(|protected| ranges_overlap(protected, range))
    }

    fn remove_owned_range(&mut self, removed: (u64, u64)) {
        let mut retained = Vec::new();
        for mapping in self.owned_mappings.drain(..) {
            if !ranges_overlap((mapping.start, mapping.end), removed) {
                retained.push(mapping);
                continue;
            }
            if mapping.start < removed.0 {
                retained.push(AfterLoaderOwnedMapping {
                    end: removed.0,
                    ..mapping
                });
            }
            if removed.1 < mapping.end {
                retained.push(AfterLoaderOwnedMapping {
                    start: removed.1,
                    offset: mapping.offset + (removed.1 - mapping.start),
                    ..mapping
                });
            }
        }
        self.owned_mappings = retained;
        self.shared_reservations
            .retain(|_, (reservation, _)| *reservation != removed);
    }

    fn protect_owned_range(&mut self, changed: (u64, u64), protection: i32) {
        let mut retained = Vec::new();
        for mapping in self.owned_mappings.drain(..) {
            if !ranges_overlap((mapping.start, mapping.end), changed) {
                retained.push(mapping);
                continue;
            }
            if mapping.start < changed.0 {
                retained.push(AfterLoaderOwnedMapping {
                    end: changed.0,
                    ..mapping
                });
            }
            let middle_start = mapping.start.max(changed.0);
            let middle_end = mapping.end.min(changed.1);
            retained.push(AfterLoaderOwnedMapping {
                start: middle_start,
                end: middle_end,
                readable: protection & libc::PROT_READ != 0,
                writable: protection & libc::PROT_WRITE != 0,
                executable: protection & libc::PROT_EXEC != 0,
                offset: mapping.offset + (middle_start - mapping.start),
                ..mapping
            });
            if changed.1 < mapping.end {
                retained.push(AfterLoaderOwnedMapping {
                    start: changed.1,
                    offset: mapping.offset + (changed.1 - mapping.start),
                    ..mapping
                });
            }
        }
        self.owned_mappings = retained;
    }

    pub(super) fn prepared_liteinst_controls(
        &self,
    ) -> Option<(Vec<PreparedArenaFootprint>, Vec<GuestRange>)> {
        if self.sealed_trampolines.is_empty()
            || self
                .trampoline_mappings
                .keys()
                .copied()
                .collect::<BTreeSet<_>>()
                != self.sealed_trampolines
            || self
                .shared_reservations
                .keys()
                .copied()
                .collect::<BTreeSet<_>>()
                != self.sealed_trampolines
        {
            return None;
        }
        let mut occupied = Vec::new();
        let mut arenas = Vec::new();
        let mut reservations = Vec::new();
        for trampoline in &self.sealed_trampolines {
            let aliases = self
                .owned_mappings
                .iter()
                .filter(|mapping| {
                    mapping.purpose
                        == (AfterLoaderMappingPurpose::Trampoline {
                            trampoline: *trampoline,
                        })
                })
                .collect::<Vec<_>>();
            let writable = aliases
                .iter()
                .find(|mapping| {
                    mapping.readable
                        && mapping.writable
                        && !mapping.executable
                        && mapping.shared
                        && mapping.offset == 0
                        && mapping.end - mapping.start == TRAMPOLINE_ARENA_SIZE
                })
                .copied()?;
            let executable = aliases
                .iter()
                .find(|mapping| {
                    mapping.readable
                        && !mapping.writable
                        && mapping.executable
                        && mapping.shared
                        && mapping.offset == 0
                        && mapping.end - mapping.start == TRAMPOLINE_ARENA_SIZE
                })
                .copied()?;
            if aliases.len() != 2 {
                return None;
            }
            let reservation = self.owned_mappings.iter().find(|mapping| {
                mapping.purpose
                    == (AfterLoaderMappingPurpose::SharedReservation {
                        trampoline: *trampoline,
                    })
                    && mapping.readable
                    && mapping.writable
                    && !mapping.executable
                    && mapping.shared
                    && mapping.descriptor.is_none()
                    && mapping.offset == 0
                    && mapping.end - mapping.start == PAGE
                    && self
                        .shared_reservations
                        .get(trampoline)
                        .is_some_and(|(range, identity)| {
                            *range == (mapping.start, mapping.end)
                                && shared_reservation_identity_is_exact(identity)
                        })
            })?;
            let writable = GuestRange {
                start: writable.start,
                end: writable.end,
            };
            let executable = GuestRange {
                start: executable.start,
                end: executable.end,
            };
            let reservation = GuestRange {
                start: reservation.start,
                end: reservation.end,
            };
            if [writable, executable, reservation]
                .iter()
                .enumerate()
                .any(|(index, range)| {
                    [writable, executable, reservation][..index]
                        .iter()
                        .any(|prior| prior.overlaps(*range))
                        || occupied
                            .iter()
                            .any(|prior: &GuestRange| prior.overlaps(*range))
                })
            {
                return None;
            }
            occupied.extend([writable, executable, reservation]);
            arenas.push(PreparedArenaFootprint {
                writable,
                executable,
            });
            reservations.push(reservation);
        }
        Some((arenas, reservations))
    }

    fn helper_code_matches_sealed_runtime(
        &self,
        runtime: &LiteinstCallerImage,
        initializer: &crate::target_loader::TargetHostInitializer,
        helper: &LiteinstHelperCode,
    ) -> bool {
        let Some(geometry) = self.image_geometry(runtime) else {
            return false;
        };
        let helper_length = helper.range.end.checked_sub(helper.range.start);
        geometry.image == self.image_id(runtime)
            && self.image_mapping_identity(runtime) == Some(geometry.mapping)
            && geometry.mapping == helper.original_mapping.mapping_identity()
            && geometry.mapping == MappingIdentity::from_target_loader(initializer.mapping_identity)
            && geometry.load_bias == initializer.load_bias
            && initializer.tid == self.image.tid
            && initializer.start_ticks == self.image.start_ticks
            && geometry.span.0 <= helper.range.start
            && helper.range.end <= geometry.span.1
            && helper.original_mapping.contains_range(helper.range)
            && helper_length == Some(PAGE)
            && usize::try_from(PAGE) == Ok(helper.bytes.len())
    }

    fn bind_helper_isolation(
        &self,
        runtime: &LiteinstCallerImage,
        initializer: &crate::target_loader::TargetHostInitializer,
        helper: &LiteinstHelperCode,
    ) -> Option<AfterLoaderHelperIsolation> {
        if !self.helper_code_matches_sealed_runtime(runtime, initializer, helper) {
            return None;
        }
        let geometry = self.image_geometry(runtime)?;
        let mapping_delta = helper
            .range
            .start
            .checked_sub(helper.original_mapping.start)?;
        let file_offset = helper.original_mapping.offset.checked_add(mapping_delta)?;
        let start = usize::try_from(file_offset).ok()?;
        let end = start.checked_add(helper.bytes.len())?;
        let expected_bytes = runtime.bytes.get(start..end)?.to_vec();
        if expected_bytes != helper.bytes {
            return None;
        }
        let owning_mappings = self
            .owned_mappings
            .iter()
            .filter(|mapping| {
                mapping.purpose
                    == (AfterLoaderMappingPurpose::Image {
                        image: geometry.image,
                    })
                    && mapping.start <= helper.range.start
                    && helper.range.end <= mapping.end
                    && mapping.readable
                    && !mapping.writable
                    && mapping.executable
                    && !mapping.shared
                    && mapping
                        .offset
                        .checked_add(helper.range.start - mapping.start)
                        == Some(file_offset)
            })
            .count();
        if owning_mappings != 1 {
            return None;
        }
        Some(AfterLoaderHelperIsolation {
            image: geometry.image,
            range: helper.range,
            original_mapping: helper.original_mapping.clone(),
            mapping: geometry.mapping,
            load_bias: geometry.load_bias,
            file_offset,
            expected_bytes,
        })
    }

    pub(super) fn current_program_break(&self) -> Option<u64> {
        self.current_break
    }
}

impl<T: Tool + 'static> TracedTask<T> {
    pub(super) fn after_loader_private_timer_is_owned(&self) -> bool {
        self.liteinst_after_loader_private_state
            .as_ref()
            .is_some_and(|state| state.timer_suspension.is_some())
    }

    pub(super) fn retire_after_loader_private_timer_on_terminal(
        &mut self,
    ) -> Result<(), TraceError> {
        let retire = {
            let suspension = self
                .liteinst_after_loader_private_state
                .as_ref()
                .and_then(|state| state.timer_suspension.as_ref())
                .ok_or(Errno::EPROTO)?;
            self.timer.retire_private_execution_on_terminal(suspension)
        };
        if let Err(error) = retire {
            tracing::error!(
                tid = %self.tid(),
                %error,
                "failed to retire deterministic timer after terminal private after-loader execution"
            );
            return Err(Errno::EPROTO.into());
        }
        drop(
            self.liteinst_after_loader_private_state
                .as_mut()
                .and_then(|state| state.timer_suspension.take())
                .ok_or(Errno::EPROTO)?,
        );
        Ok(())
    }
}

fn page_down(value: u64) -> u64 {
    value & !(PAGE - 1)
}

fn page_up(value: u64) -> Result<u64, Errno> {
    value
        .checked_add(PAGE - 1)
        .map(page_down)
        .ok_or(Errno::EOVERFLOW)
}

fn read_exact_with_helper_isolation(
    task: &Stopped,
    address: u64,
    bytes: &mut [u8],
    isolation: Option<&AfterLoaderHelperIsolation>,
) -> Result<(), TraceError> {
    if bytes.is_empty() {
        return Ok(());
    }
    let length = u64::try_from(bytes.len()).map_err(|_| Errno::EOVERFLOW)?;
    let requested = GuestRange::new(address, length).ok_or(Errno::EOVERFLOW)?;
    let Some(isolation) = isolation.filter(|isolation| isolation.range.overlaps(requested)) else {
        let address = usize::try_from(address).map_err(|_| Errno::EOVERFLOW)?;
        return Ok(task.read_exact(address, bytes)?);
    };

    let overlap_start = requested.start.max(isolation.range.start);
    let overlap_end = requested.end.min(isolation.range.end);
    let prefix_len =
        usize::try_from(overlap_start - requested.start).map_err(|_| Errno::EOVERFLOW)?;
    if prefix_len != 0 {
        let address = usize::try_from(requested.start).map_err(|_| Errno::EOVERFLOW)?;
        task.read_exact(address, &mut bytes[..prefix_len])?;
    }

    let mut live_page = vec![0_u8; isolation.expected_bytes.len()];
    let helper_address = usize::try_from(isolation.range.start).map_err(|_| Errno::EOVERFLOW)?;
    if !read_stopped_ptrace_words(task, helper_address, &mut live_page)
        || live_page != isolation.expected_bytes
    {
        return Err(Errno::EPROTO.into());
    }
    let destination_start =
        usize::try_from(overlap_start - requested.start).map_err(|_| Errno::EOVERFLOW)?;
    let source_start =
        usize::try_from(overlap_start - isolation.range.start).map_err(|_| Errno::EOVERFLOW)?;
    let overlap_len = usize::try_from(overlap_end - overlap_start).map_err(|_| Errno::EOVERFLOW)?;
    bytes[destination_start..destination_start + overlap_len]
        .copy_from_slice(&live_page[source_start..source_start + overlap_len]);

    let suffix_start =
        usize::try_from(overlap_end - requested.start).map_err(|_| Errno::EOVERFLOW)?;
    if suffix_start < bytes.len() {
        let address = usize::try_from(overlap_end).map_err(|_| Errno::EOVERFLOW)?;
        task.read_exact(address, &mut bytes[suffix_start..])?;
    }
    Ok(())
}

/// Return the bytes on which Linux memory-management syscalls actually act.
///
/// The raw positive length remains part of the exact syscall permit. Linux
/// rounds that length upward for mmap, mprotect, and munmap; the address itself
/// must be page aligned for every fixed or returned range admitted here. This
/// private policy deliberately refuses mprotect's zero-length no-op because it
/// is not a loader memory effect and would add an unaudited syscall shape.
fn checked_page_effect_length(raw_length: u64) -> Result<u64, Errno> {
    if raw_length == 0 {
        return Err(Errno::EINVAL);
    }
    page_up(raw_length)
}

fn checked_page_effect_range(start: u64, raw_length: u64) -> Result<(u64, u64), Errno> {
    if !start.is_multiple_of(PAGE) {
        return Err(Errno::EINVAL);
    }
    checked_range(start, checked_page_effect_length(raw_length)?)
}

fn checked_private_mmap_effect_length(raw_length: u64) -> Result<u64, Errno> {
    let effective_length = checked_page_effect_length(raw_length)?;
    if effective_length > MAX_PRIVATE_MMAP_EFFECT {
        return Err(Errno::EINVAL);
    }
    Ok(effective_length)
}

fn private_mmap_offset_is_admissible(offset: u64) -> bool {
    offset <= i64::MAX as u64 && offset.is_multiple_of(PAGE)
}

fn exact_trampoline_mmap_length(raw_length: u64) -> bool {
    raw_length == TRAMPOLINE_ARENA_SIZE
}

fn exact_shared_reservation_mmap_length(raw_length: u64) -> bool {
    raw_length == PAGE
}

fn callback_stack_usable_length_is_admissible(length: u64) -> bool {
    length.is_multiple_of(PAGE)
        && (CALLBACK_STACK_MIN_USABLE_BYTES..=CALLBACK_STACK_MAX_USABLE_BYTES).contains(&length)
}

fn callback_stack_mapping_length_is_admissible(length: u64) -> bool {
    length
        .checked_sub(2 * PAGE)
        .is_some_and(callback_stack_usable_length_is_admissible)
}

fn exact_callback_stack_mmap_shape(args: [u64; 6], protection: i32, flags: i32) -> bool {
    args[0] == 0
        && callback_stack_mapping_length_is_admissible(args[1])
        && protection == libc::PROT_NONE
        && flags == (libc::MAP_PRIVATE | libc::MAP_ANONYMOUS | libc::MAP_STACK)
        && canonical_anonymous_mmap_descriptor(args[4])
        && args[5] == 0
}

fn descriptor_position(tid: Pid, descriptor: u64) -> io::Result<u64> {
    let bytes = bounded_proc(format!("/proc/{tid}/fdinfo/{descriptor}"), 64 * 1024)?;
    let text = std::str::from_utf8(&bytes).map_err(io::Error::other)?;
    text.lines()
        .find_map(|line| line.strip_prefix("pos:\t"))
        .ok_or_else(|| io::Error::other("descriptor position is absent"))?
        .parse()
        .map_err(io::Error::other)
}

fn target_range_is_zero(task: &Stopped, start: u64, length: u64) -> Result<bool, TraceError> {
    let end = start.checked_add(length).ok_or(Errno::EOVERFLOW)?;
    if start == end {
        return Ok(true);
    }
    // process_vm_readv deliberately follows the target's current user access
    // permissions and therefore rejects a freshly mapped PROT_NONE guard page.
    // The procfs memory file remains bound to this live stopped task and uses
    // the kernel's ptrace-authorized access path without changing protections.
    let memory = std::fs::File::open(format!("/proc/{}/mem", task.pid()))
        .map_err(|error| Errno::new(error.raw_os_error().unwrap_or(libc::EIO)))?;
    let mut address = start;
    let mut bytes = [0_u8; 4096];
    while address < end {
        let amount = usize::try_from((end - address).min(bytes.len() as u64))
            .map_err(|_| Errno::EOVERFLOW)?;
        memory
            .read_exact_at(&mut bytes[..amount], address)
            .map_err(|error| Errno::new(error.raw_os_error().unwrap_or(libc::EIO)))?;
        if bytes[..amount].iter().any(|byte| *byte != 0) {
            return Ok(false);
        }
        address += amount as u64;
    }
    Ok(true)
}

fn mapping_identity_matches(identity: MappingIdentity, map: &GuestMap) -> bool {
    map.mapping_identity() == identity
}

fn shared_reservation_identity_is_exact(identity: &SharedReservationIdentity) -> bool {
    identity.mapping.device_major == 0
        && identity.mapping.device_minor == 1
        && identity.mapping.inode != 0
        && identity
            .path
            .as_ref()
            .is_some_and(|path| path.as_os_str().as_encoded_bytes() == b"/dev/zero (deleted)")
}

fn exact_shared_reservation_identity(map: &GuestMap) -> Option<SharedReservationIdentity> {
    let identity = SharedReservationIdentity {
        mapping: map.mapping_identity(),
        path: map.path.clone(),
    };
    shared_reservation_identity_is_exact(&identity).then_some(identity)
}

fn loader_resolution_matches_geometry(
    mapping_identity: (u64, u64, u64),
    load_bias: u64,
    geometry: ResolvedImageGeometry,
) -> bool {
    mapping_identity == geometry.mapping.as_target_loader() && load_bias == geometry.load_bias
}

fn one_exact_geometry_candidate(
    candidates: &[(MappingIdentity, u64)],
) -> Option<(MappingIdentity, u64)> {
    (candidates.len() == 1).then(|| candidates[0])
}

fn dlopen_graph_images<'a>(
    provider: &'a LiteinstCallerImage,
    dependencies: &'a [LiteinstCallerImage],
) -> impl Iterator<Item = &'a LiteinstCallerImage> {
    std::iter::once(provider).chain(dependencies.iter())
}

fn dlopen_graph_image_for_path<'a>(
    provider: &'a LiteinstCallerImage,
    dependencies: &'a [LiteinstCallerImage],
    path: &[u8],
) -> Option<&'a LiteinstCallerImage> {
    dlopen_graph_images(provider, dependencies)
        .find(|image| path == image.path.as_os_str().as_encoded_bytes())
}

fn exact_geometry_bytes_match(observed: &[u8], expected: &[u8]) -> bool {
    observed == expected
}

fn stable_backing_stamp(metadata: &std::fs::Metadata) -> (u64, u64, u64, i64, i64, i64, i64) {
    (
        metadata.dev(),
        metadata.ino(),
        metadata.len(),
        metadata.mtime(),
        metadata.mtime_nsec(),
        metadata.ctime(),
        metadata.ctime_nsec(),
    )
}

fn open_path_with_root(root: &std::fs::File, path: &Path, flags: i32) -> io::Result<std::fs::File> {
    if !path.is_absolute() || path.as_os_str().as_bytes().contains(&0) {
        return Err(io::Error::other(
            "target-root path is not one absolute Unix path",
        ));
    }
    let path = CString::new(path.as_os_str().as_bytes()).map_err(io::Error::other)?;
    let how = OpenHow {
        flags: flags as u64,
        mode: 0,
        resolve: RESOLVE_IN_ROOT | RESOLVE_NO_MAGICLINKS,
    };
    // SAFETY: root and path are live, how has the Linux UAPI layout, and Linux
    // validates the flags, descriptor and structure size.
    let descriptor = unsafe {
        libc::syscall(
            libc::SYS_openat2,
            root.as_raw_fd(),
            path.as_ptr(),
            &how,
            std::mem::size_of::<OpenHow>(),
        )
    };
    if descriptor < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: successful openat2 returned a new owned descriptor.
    Ok(unsafe { std::fs::File::from_raw_fd(descriptor as libc::c_int) })
}

fn open_target_root(pid: Pid) -> io::Result<std::fs::File> {
    std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_PATH | libc::O_CLOEXEC)
        .open(format!("/proc/{pid}/root"))
}

fn read_link_from_path_descriptor(file: &std::fs::File) -> io::Result<PathBuf> {
    let mut bytes = vec![0_u8; 4096];
    // SAFETY: file is live, the empty C string requests the O_PATH symlink
    // itself, and bytes is a writable buffer with the supplied length.
    let amount = unsafe {
        libc::readlinkat(
            file.as_raw_fd(),
            c"".as_ptr(),
            bytes.as_mut_ptr().cast(),
            bytes.len(),
        )
    };
    if amount < 0 {
        return Err(io::Error::last_os_error());
    }
    let amount = usize::try_from(amount).map_err(io::Error::other)?;
    if amount == bytes.len() {
        return Err(io::Error::other(
            "target-root symlink payload exceeds its bound",
        ));
    }
    bytes.truncate(amount);
    Ok(PathBuf::from(std::ffi::OsString::from_vec(bytes)))
}

fn complete_file_bytes_and_stamp(
    file: &mut std::fs::File,
    expected: &[u8],
    stamp: StableBackingStamp,
) -> io::Result<bool> {
    let before = file.metadata()?;
    file.seek(SeekFrom::Start(0))?;
    let mut bytes = Vec::new();
    file.take(
        (expected.len() as u64)
            .checked_add(1)
            .ok_or_else(|| io::Error::other("alias byte bound overflow"))?,
    )
    .read_to_end(&mut bytes)?;
    let after = file.metadata()?;
    Ok(before.is_file()
        && stable_backing_stamp(&before) == stamp
        && stable_backing_stamp(&after) == stamp
        && bytes.as_slice() == expected)
}

fn target_loader_cache_alias_matches(
    pid: Pid,
    alias: &crate::after_loader::LiteinstLoaderCacheAlias,
) -> io::Result<bool> {
    let root = open_target_root(pid)?;
    let raw = open_path_with_root(&root, alias.raw_path(), libc::O_PATH | libc::O_CLOEXEC)?;
    let raw_symlink = open_path_with_root(
        &root,
        alias.raw_path(),
        libc::O_PATH | libc::O_NOFOLLOW | libc::O_CLOEXEC,
    )?;
    let canonical = open_path_with_root(
        &root,
        &alias.dependency().path,
        libc::O_PATH | libc::O_CLOEXEC,
    )?;
    let raw_metadata = raw.metadata()?;
    let canonical_metadata = canonical.metadata()?;
    let expected = alias.dependency();
    let expected_stamp = alias.dependency_stamp();
    // This exact reviewed profile launches the target in the controller's
    // mount root. Retaining literal fd-link spelling is intentional here: a
    // future chroot or distinct mount namespace needs a separately reviewed
    // target-domain pathname bridge rather than silently broadening this one.
    let raw_link = std::fs::read_link(format!("/proc/self/fd/{}", raw.as_raw_fd()))?;
    let canonical_link = std::fs::read_link(format!("/proc/self/fd/{}", canonical.as_raw_fd()))?;
    if !raw_metadata.is_file()
        || !canonical_metadata.is_file()
        || crate::after_loader::FileIdentity::from_metadata(&raw_metadata) != expected.file_identity
        || crate::after_loader::FileIdentity::from_metadata(&canonical_metadata)
            != expected.file_identity
        || stable_backing_stamp(&raw_metadata) != expected_stamp
        || stable_backing_stamp(&canonical_metadata) != expected_stamp
        || raw_link != expected.path
        || canonical_link != expected.path
    {
        return Ok(false);
    }

    let link_metadata = raw_symlink.metadata()?;
    if !link_metadata.file_type().is_symlink()
        || stable_backing_stamp(&link_metadata) != alias.raw_link_stamp()
        || read_link_from_path_descriptor(&raw_symlink)? != alias.raw_link()
    {
        return Ok(false);
    }

    let mut raw_file = std::fs::File::open(format!("/proc/self/fd/{}", raw.as_raw_fd()))?;
    let mut canonical_file =
        std::fs::File::open(format!("/proc/self/fd/{}", canonical.as_raw_fd()))?;
    if !complete_file_bytes_and_stamp(&mut raw_file, &expected.bytes, expected_stamp)?
        || !complete_file_bytes_and_stamp(&mut canonical_file, &expected.bytes, expected_stamp)?
        || mapping_identity_for_open_file(&raw_file)?
            != mapping_identity_for_open_file(&canonical_file)?
    {
        return Ok(false);
    }
    Ok(true)
}

fn loader_cache_alias_descriptor_matches(
    pid: Pid,
    descriptor: u64,
    canonical_path: &Path,
    file_identity: crate::after_loader::FileIdentity,
    bytes: &[u8],
    stamp: StableBackingStamp,
) -> io::Result<Option<MappingIdentity>> {
    let descriptor_path = format!("/proc/{pid}/fd/{descriptor}");
    let link = std::fs::read_link(&descriptor_path)?;
    let metadata = std::fs::metadata(&descriptor_path)?;
    let mut opened = std::fs::File::open(&descriptor_path)?;
    let matches = link == canonical_path
        && metadata.is_file()
        && crate::after_loader::FileIdentity::from_metadata(&metadata) == file_identity
        && stable_backing_stamp(&metadata) == stamp
        && complete_file_bytes_and_stamp(&mut opened, bytes, stamp)?;
    if !matches {
        return Ok(None);
    }
    mapping_identity_for_open_file(&opened).map(Some)
}

/// Observe the `/proc/maps` identity produced by mapping one exact open file.
/// The file-domain metadata is deliberately not converted into a maps device.
fn mapping_identity_for_open_file(file: &std::fs::File) -> io::Result<MappingIdentity> {
    let length = PAGE as usize;
    let address = unsafe {
        libc::mmap(
            std::ptr::null_mut(),
            length,
            libc::PROT_NONE,
            libc::MAP_PRIVATE,
            file.as_raw_fd(),
            0,
        )
    };
    if address == libc::MAP_FAILED {
        return Err(io::Error::last_os_error());
    }
    let result = (|| {
        let start = address as u64;
        let end = start
            .checked_add(PAGE)
            .ok_or_else(|| io::Error::other("temporary mapping range overflow"))?;
        let maps = guest_maps(Pid::from_raw(std::process::id() as i32))
            .ok_or_else(|| io::Error::other("cannot read controller maps"))?;
        let mapping = maps
            .iter()
            .filter(|mapping| mapping.start <= start && end <= mapping.end)
            .find(|mapping| {
                !mapping.readable
                    && !mapping.writable
                    && !mapping.executable
                    && !mapping.shared
                    && mapping.offset.checked_add(start - mapping.start) == Some(0)
                    && mapping.inode != 0
            })
            .ok_or_else(|| io::Error::other("temporary file mapping is not exact"))?;
        Ok(mapping.mapping_identity())
    })();
    let unmap = unsafe { libc::munmap(address, length) };
    if unmap != 0 {
        return Err(io::Error::last_os_error());
    }
    result
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct LoaderCacheArtifactEvidence {
    file: crate::after_loader::FileIdentity,
    mapping: MappingIdentity,
    length: u64,
}

fn open_loader_cache_without_atime(path: impl AsRef<Path>) -> io::Result<std::fs::File> {
    std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOATIME | libc::O_CLOEXEC)
        .open(path)
}

fn exact_open_backing_identity_in_root(
    root: &std::fs::File,
    path: &Path,
    image: &LiteinstCallerImage,
) -> io::Result<Option<MappingIdentity>> {
    let mut file = open_path_with_root(root, path, libc::O_RDONLY | libc::O_CLOEXEC)?;
    let before = file.metadata()?;
    if !before.is_file()
        || crate::after_loader::FileIdentity::from_metadata(&before) != image.file_identity
    {
        return Ok(None);
    }
    file.seek(SeekFrom::Start(0))?;
    let mut bytes = Vec::new();
    (&mut file)
        .take(
            (image.bytes.len() as u64)
                .checked_add(1)
                .ok_or_else(|| io::Error::other("backing byte bound overflow"))?,
        )
        .read_to_end(&mut bytes)?;
    let after = file.metadata()?;
    let path_after = open_path_with_root(root, path, libc::O_PATH | libc::O_CLOEXEC)?.metadata()?;
    let mapping = mapping_identity_for_open_file(&file)?;
    Ok((bytes.as_slice() == image.bytes.as_ref()
        && stable_backing_stamp(&before) == stable_backing_stamp(&after)
        && stable_backing_stamp(&before) == stable_backing_stamp(&path_after))
    .then_some(mapping))
}

fn exact_open_backing_matches_in_root(
    root: &std::fs::File,
    path: &Path,
    image: &LiteinstCallerImage,
    expected_mapping: MappingIdentity,
) -> io::Result<bool> {
    Ok(exact_open_backing_identity_in_root(root, path, image)? == Some(expected_mapping))
}

fn target_bound_image_mapping_identity(
    pid: Pid,
    image: &LiteinstCallerImage,
) -> io::Result<Option<MappingIdentity>> {
    let root = open_target_root(pid)?;
    exact_open_backing_identity_in_root(&root, &image.path, image)
}

fn exact_image_backing_paths_match(
    pid: Pid,
    image: &LiteinstCallerImage,
    geometry: ResolvedImageGeometry,
    maps: &[GuestMap],
) -> io::Result<bool> {
    let root = open_target_root(pid)?;
    let relevant = maps
        .iter()
        .filter(|mapping| {
            mapping.mapping_identity() == geometry.mapping
                && ranges_overlap((mapping.start, mapping.end), geometry.span)
        })
        .collect::<Vec<_>>();
    if relevant.is_empty()
        || relevant
            .iter()
            .any(|mapping| mapping.path.as_ref().is_none_or(|path| !path.is_absolute()))
    {
        return Ok(false);
    }
    let paths = relevant
        .into_iter()
        .map(|mapping| mapping.path.as_ref().unwrap().clone())
        .collect::<BTreeSet<_>>();
    for guest_path in paths {
        if !exact_open_backing_matches_in_root(&root, &guest_path, image, geometry.mapping)? {
            return Ok(false);
        }
    }
    Ok(true)
}

fn exact_image_mapping_paths_are_literal(
    image: &LiteinstCallerImage,
    geometry: ResolvedImageGeometry,
    maps: &[GuestMap],
) -> bool {
    let relevant = maps
        .iter()
        .filter(|mapping| {
            mapping.mapping_identity() == geometry.mapping
                && ranges_overlap((mapping.start, mapping.end), geometry.span)
                && mapping.inode != 0
        })
        .collect::<Vec<_>>();
    !relevant.is_empty()
        && relevant
            .iter()
            .all(|mapping| mapping.path.as_deref() == Some(image.path.as_path()))
}

fn exact_image_relro_range(
    image: &LiteinstCallerImage,
    geometry: ResolvedImageGeometry,
) -> Result<(u64, u64), Errno> {
    let elf = Elf::parse(&image.bytes).map_err(|_| Errno::EPROTO)?;
    let ranges = elf
        .program_headers
        .iter()
        .filter(|header| header.p_type == ph::PT_GNU_RELRO)
        .map(|header| {
            let start = geometry
                .load_bias
                .checked_add(page_down(header.p_vaddr))
                .ok_or(Errno::EOVERFLOW)?;
            let end = geometry
                .load_bias
                .checked_add(
                    header
                        .p_vaddr
                        .checked_add(header.p_memsz)
                        .ok_or(Errno::EOVERFLOW)
                        .and_then(page_up)?,
                )
                .ok_or(Errno::EOVERFLOW)?;
            (start < end).then_some((start, end)).ok_or(Errno::EPROTO)
        })
        .collect::<Result<Vec<_>, _>>()?;
    let [range] = ranges.as_slice() else {
        return Err(Errno::EPROTO);
    };
    Ok(*range)
}

fn altstack_fields(bytes: &[u8; 24]) -> (u64, u32, u64) {
    // stack_t has four padding bytes between ss_flags and ss_size on x86-64.
    // Observe the complete raw buffer but compare all three defined fields.
    (
        u64::from_ne_bytes(bytes[..8].try_into().unwrap()),
        u32::from_ne_bytes(bytes[8..12].try_into().unwrap()),
        u64::from_ne_bytes(bytes[16..].try_into().unwrap()),
    )
}

fn descriptor_state(tid: Pid) -> io::Result<BTreeMap<u32, (u64, u64, Vec<u8>)>> {
    let directory = format!("/proc/{tid}/fd");
    let mut state = BTreeMap::new();
    for entry in std::fs::read_dir(directory)? {
        if state.len() >= 256 {
            return Err(io::Error::other("descriptor count exceeds fixture bound"));
        }
        let entry = entry?;
        let fd = entry
            .file_name()
            .to_str()
            .ok_or_else(|| io::Error::other("nontext descriptor"))?
            .parse::<u32>()
            .map_err(io::Error::other)?;
        let metadata = std::fs::metadata(entry.path())?;
        let info = bounded_proc(format!("/proc/{tid}/fdinfo/{fd}"), 64 * 1024)?;
        if state
            .insert(fd, (metadata.dev(), metadata.ino(), info))
            .is_some()
        {
            return Err(io::Error::other("duplicate descriptor"));
        }
    }
    Ok(state)
}

fn proc_fd_link_snapshot(tid: Pid) -> io::Result<BTreeMap<u64, Vec<u8>>> {
    let directory = format!("/proc/{tid}/fd");
    let mut links = BTreeMap::new();
    for entry in std::fs::read_dir(directory)? {
        if links.len() >= 256 {
            return Err(io::Error::other("descriptor count exceeds fixture bound"));
        }
        let entry = entry?;
        let descriptor = entry
            .file_name()
            .to_str()
            .ok_or_else(|| io::Error::other("nontext descriptor"))?
            .parse::<u64>()
            .map_err(io::Error::other)?;
        let link = std::fs::read_link(entry.path())?;
        if links
            .insert(descriptor, link.as_os_str().as_encoded_bytes().to_vec())
            .is_some()
        {
            return Err(io::Error::other("duplicate descriptor"));
        }
    }
    Ok(links)
}

fn descriptor_flags(tid: Pid, descriptor: u64) -> io::Result<u64> {
    let bytes = bounded_proc(format!("/proc/{tid}/fdinfo/{descriptor}"), 64 * 1024)?;
    let text = std::str::from_utf8(&bytes).map_err(io::Error::other)?;
    let value = text
        .lines()
        .find_map(|line| line.strip_prefix("flags:\t"))
        .ok_or_else(|| io::Error::other("descriptor flags are absent"))?;
    u64::from_str_radix(value, 8).map_err(io::Error::other)
}

fn environment_map(bytes: &[u8]) -> io::Result<BTreeMap<OsString, OsString>> {
    let mut result = BTreeMap::new();
    if bytes.is_empty() {
        return Ok(result);
    }
    let content = bytes
        .strip_suffix(&[0])
        .ok_or_else(|| io::Error::other("unterminated environment"))?;
    for item in content.split(|byte| *byte == 0) {
        let equals = item
            .iter()
            .position(|byte| *byte == b'=')
            .ok_or_else(|| io::Error::other("environment item lacks equals"))?;
        if equals == 0
            || result
                .insert(
                    OsString::from_vec(item[..equals].to_vec()),
                    OsString::from_vec(item[equals + 1..].to_vec()),
                )
                .is_some()
        {
            return Err(io::Error::other("empty or duplicate environment key"));
        }
    }
    Ok(result)
}

fn bounded_proc(path: impl AsRef<std::path::Path>, limit: usize) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    std::fs::File::open(path)?
        .take(limit as u64 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > limit {
        return Err(io::Error::other("proc input exceeds bound"));
    }
    Ok(bytes)
}

fn signal_state(tid: Pid) -> io::Result<Vec<String>> {
    let bytes = bounded_proc(format!("/proc/{tid}/status"), 64 * 1024)?;
    let text = std::str::from_utf8(&bytes).map_err(io::Error::other)?;
    let keys = ["SigPnd:", "ShdPnd:", "SigBlk:", "SigIgn:", "SigCgt:"];
    let selected: Vec<_> = text
        .lines()
        .filter(|line| keys.iter().any(|k| line.starts_with(k)))
        .map(str::to_owned)
        .collect();
    if selected.len() != keys.len() {
        return Err(io::Error::other("signal state incomplete"));
    }
    for key in ["SigPnd:", "ShdPnd:"] {
        let line = selected.iter().find(|line| line.starts_with(key)).unwrap();
        if u64::from_str_radix(line.split_whitespace().nth(1).unwrap_or(""), 16)
            .map_err(io::Error::other)?
            != 0
        {
            return Err(io::Error::other("signal pending in after-loader call"));
        }
    }
    Ok(selected)
}

fn register_words(r: &libc::user_regs_struct) -> [u64; 27] {
    [
        r.r15, r.r14, r.r13, r.r12, r.rbp, r.rbx, r.r11, r.r10, r.r9, r.r8, r.rax, r.rcx, r.rdx,
        r.rsi, r.rdi, r.orig_rax, r.rip, r.cs, r.eflags, r.rsp, r.ss, r.fs_base, r.gs_base, r.ds,
        r.es, r.fs, r.gs,
    ]
}

fn after_loader_event_summary(event: &Event) -> String {
    match event {
        Event::NewChild(operation, child) => format!(
            "NewChild(operation={operation:?} tid={} generation={:?})",
            child.pid(),
            child.physical_event_generation(),
        ),
        Event::Exec(previous_tid) => format!("Exec(previous_tid={previous_tid})"),
        Event::VforkDone => "VforkDone".to_owned(),
        Event::Exit => "Exit".to_owned(),
        Event::Seccomp => "Seccomp".to_owned(),
        Event::Stop => "Stop".to_owned(),
        Event::Syscall => "Syscall".to_owned(),
        Event::Signal(signal) => format!("Signal({signal:?})"),
    }
}

fn after_loader_wait_summary(wait: &Wait) -> String {
    match wait {
        Wait::Stopped(task, event) => format!(
            "Stopped(tid={} generation={:?} physical_status={:?} event={})",
            task.pid(),
            task.physical_event_generation(),
            task.physical_status_id(),
            after_loader_event_summary(event),
        ),
        Wait::Exited(pid, status) => format!("Exited(tid={pid} status={status:?})"),
    }
}

impl<L: Tool + 'static> TracedTask<L> {
    pub(super) fn after_loader_config(&self) -> Option<LiteinstAfterLoaderConfig> {
        self.global_state
            .liteinst_runtime
            .as_ref()?
            .after_loader
            .clone()
    }

    fn caller_error(&self, message: impl fmt::Display) -> Error {
        Error::runtime(
            self.tid(),
            "LiteInst after-loader call",
            message.to_string(),
        )
    }

    fn loader_cache_artifact_evidence(
        &self,
        config: &LiteinstAfterLoaderConfig,
    ) -> Result<LoaderCacheArtifactEvidence, Error> {
        let cache = config.loader_cache();
        let artifact = config.immutable_loader_cache();
        if cache.path().as_os_str().as_encoded_bytes() != LOADER_CACHE_PATH
            || artifact.logical_path() != cache.path()
            || artifact.source_identity() != cache.file_identity()
            || artifact.bytes() != cache.bytes()
            || artifact.seals() != crate::after_loader::IMMUTABLE_FILE_SEALS
            || cache.bytes().is_empty()
        {
            return Err(self.caller_error(
                "loader-cache logical identity, immutable bytes or seal policy differs",
            ));
        }

        let source_before =
            std::fs::metadata(cache.path()).map_err(|error| self.caller_error(error))?;
        let source_bytes = bounded_proc(cache.path(), crate::after_loader::MAX_LOADER_CACHE_FILE)
            .map_err(|error| self.caller_error(error))?;
        let source_after =
            std::fs::metadata(cache.path()).map_err(|error| self.caller_error(error))?;
        if !source_before.is_file()
            || crate::after_loader::FileIdentity::from_metadata(&source_before)
                != cache.file_identity()
            || stable_backing_stamp(&source_before) != stable_backing_stamp(&source_after)
            || source_bytes.as_slice() != cache.bytes()
        {
            return Err(self.caller_error(
                "loader-cache source identity or complete bytes changed after binding",
            ));
        }

        let mut sealed = open_loader_cache_without_atime(artifact.sealed_source())
            .map_err(|error| self.caller_error(error))?;
        let sealed_before = sealed
            .metadata()
            .map_err(|error| self.caller_error(error))?;
        let seals = unsafe { libc::fcntl(sealed.as_raw_fd(), libc::F_GET_SEALS) };
        let status_flags = unsafe { libc::fcntl(sealed.as_raw_fd(), libc::F_GETFL) };
        let mut sealed_bytes = Vec::new();
        (&mut sealed)
            .take(
                (cache.bytes().len() as u64)
                    .checked_add(1)
                    .ok_or(Errno::EOVERFLOW)?,
            )
            .read_to_end(&mut sealed_bytes)
            .map_err(|error| self.caller_error(error))?;
        let sealed_after = sealed
            .metadata()
            .map_err(|error| self.caller_error(error))?;
        if !sealed_before.is_file()
            || crate::after_loader::FileIdentity::from_metadata(&sealed_before)
                != artifact.sealed_identity()
            || stable_backing_stamp(&sealed_before) != stable_backing_stamp(&sealed_after)
            || sealed_bytes.as_slice() != cache.bytes()
            || seals != artifact.seals()
            || status_flags < 0
            || status_flags & libc::O_ACCMODE != libc::O_RDONLY
        {
            return Err(self.caller_error(
                "sealed loader-cache identity, complete bytes, flags or seals differ",
            ));
        }
        sealed
            .seek(SeekFrom::Start(0))
            .map_err(|error| self.caller_error(error))?;
        let mapping =
            mapping_identity_for_open_file(&sealed).map_err(|error| self.caller_error(error))?;
        Ok(LoaderCacheArtifactEvidence {
            file: artifact.sealed_identity(),
            mapping,
            length: cache.bytes().len() as u64,
        })
    }

    fn initialize_loader_cache_consumption(
        &mut self,
        task: &Stopped,
        config: &LiteinstAfterLoaderConfig,
        scratch_base: u64,
    ) -> Result<(), Error> {
        let _ = self.loader_cache_artifact_evidence(config)?;
        let scratch_page = scratch_base.checked_add(PAGE).ok_or(Errno::EOVERFLOW)?;
        let reservation_end = scratch_base.checked_add(3 * PAGE).ok_or(Errno::EOVERFLOW)?;
        let reservation = (scratch_base, reservation_end);
        let scratch_start = scratch_page
            .checked_add(LOADER_CACHE_SCRATCH_OFFSET)
            .ok_or(Errno::EOVERFLOW)?;
        let scratch_end = scratch_page
            .checked_add(LOADER_CACHE_SCRATCH_END_OFFSET)
            .ok_or(Errno::EOVERFLOW)?;
        let page_end = scratch_page.checked_add(PAGE).ok_or(Errno::EOVERFLOW)?;
        let sealed_path = config
            .immutable_loader_cache()
            .sealed_source()
            .as_os_str()
            .as_encoded_bytes();
        let state = self.after_loader_private_state()?;
        let owner = state
            .owned_mappings
            .iter()
            .find(|mapping| mapping.start == scratch_page && mapping.end == page_end)
            .copied()
            .ok_or_else(|| {
                self.caller_error("loader-cache scratch has no exact middle-page owner")
            })?;
        let maps = guest_maps(task.pid()).ok_or(Errno::EPROTO)?;
        if state.loader_cache.is_some()
            || !loader_cache_scratch_owner_shape_is_exact((scratch_start, scratch_end), owner)
            || !loader_cache_scratch_logical_reservation_is_exact(
                reservation,
                (scratch_start, scratch_end),
                owner,
                &state.owned_mappings,
            )
            || !loader_cache_scratch_physical_reservation_is_exact(
                task.pid(),
                reservation,
                owner,
                &maps,
            )
            || sealed_path.contains(&0)
            || sealed_path
                .len()
                .checked_add(1)
                .is_none_or(|length| length == 0 || length > LOADER_CACHE_SCRATCH_BYTES)
        {
            return Err(self.caller_error(
                "loader-cache redirect scratch is not one exact unused controller slice",
            ));
        }
        let mut observed = vec![0_u8; LOADER_CACHE_SCRATCH_BYTES];
        task.read_exact(scratch_start as usize, &mut observed)?;
        let aliases = config
            .loader_cache_aliases()
            .map(|alias| {
                (
                    alias.raw_path().to_path_buf(),
                    LoaderCacheAliasLifecycle::AwaitingOpen {
                        image: state.image_id(alias.dependency()),
                    },
                )
            })
            .collect::<BTreeMap<_, _>>();
        self.after_loader_private_state_mut()?.loader_cache = Some(LoaderCacheState {
            scratch_reservation: reservation,
            scratch: (scratch_start, scratch_end),
            scratch_owner: owner,
            scratch_preimage: observed.clone(),
            lifecycle: LoaderCacheLifecycle::AwaitingOpen,
            aliases,
        });
        self.caller_observe(
            "loader-cache consumption armed",
            format!(
                "scratch={scratch_start:#x}-{scratch_end:#x} bytes={} lifecycle=AwaitingOpen",
                observed.len(),
            ),
        )
    }

    fn authenticate_loader_cache_interpreter_site(
        &self,
        task: &Stopped,
        config: &LiteinstAfterLoaderConfig,
        permit: &AfterLoaderSyscallPermit,
        syscall_rva: u64,
    ) -> Result<ResolvedImageGeometry, Error> {
        let digest: [u8; 32] = Sha256::digest(config._interpreter.bytes.as_ref()).into();
        if digest != LOADER_CACHE_INTERPRETER_SHA256 {
            return Err(self.caller_error("loader-cache interpreter digest differs"));
        }
        let state = self.after_loader_private_state()?;
        let image_id = state.image_id(&config._interpreter);
        let geometry = state.image_geometry(&config._interpreter).ok_or_else(|| {
            self.caller_error("loader-cache interpreter geometry was not bound at entry")
        })?;
        if geometry.image != image_id {
            return Err(self.caller_error("loader-cache interpreter identity changed"));
        }
        let renewed = self.resolve_after_loader_image_geometry(
            task,
            &config._interpreter,
            image_id,
            "loader-cache-syscall-renewal",
        )?;
        if renewed != geometry {
            return Err(self.caller_error("loader-cache interpreter geometry changed"));
        }
        self.authenticate_after_loader_image_backing(task, &config._interpreter, renewed)?;
        let syscall = geometry
            .load_bias
            .checked_add(syscall_rva)
            .ok_or(Errno::EOVERFLOW)?;
        let resume = syscall.checked_add(2).ok_or(Errno::EOVERFLOW)?;
        let maps = guest_maps(task.pid()).ok_or(Errno::EPROTO)?;
        let site = GuestRange::new(syscall, 2).ok_or(Errno::EOVERFLOW)?;
        let exact_mapping_count = maps
            .iter()
            .filter(|mapping| {
                mapping.readable
                    && mapping.executable
                    && !mapping.writable
                    && !mapping.shared
                    && mapping_identity_matches(geometry.mapping, mapping)
                    && mapping.contains_range(site)
            })
            .count();
        let mut instruction = [0_u8; 2];
        task.read_exact(syscall as usize, &mut instruction)?;
        if exact_mapping_count != 1
            || permit.instruction_pointer != syscall
            || permit.resume_pointer != resume
            || permit.instruction_length != 2
            || permit.instruction[..2] != [0x0f, 0x05]
            || instruction != [0x0f, 0x05]
        {
            return Err(self.caller_error(
                "loader-cache syscall left its exact authenticated interpreter site",
            ));
        }
        Ok(geometry)
    }

    fn authenticate_loader_cache_scratch_owner(
        &self,
        task: &Stopped,
    ) -> Result<LoaderCacheState, Error> {
        let state = self
            .after_loader_private_state()?
            .loader_cache
            .clone()
            .ok_or_else(|| self.caller_error("loader-cache scratch was not initialized"))?;
        let private = self.after_loader_private_state()?;
        let maps = guest_maps(task.pid()).ok_or(Errno::EPROTO)?;
        if !loader_cache_scratch_owner_shape_is_exact(state.scratch, state.scratch_owner)
            || !loader_cache_scratch_logical_reservation_is_exact(
                state.scratch_reservation,
                state.scratch,
                state.scratch_owner,
                &private.owned_mappings,
            )
            || !loader_cache_scratch_physical_reservation_is_exact(
                task.pid(),
                state.scratch_reservation,
                state.scratch_owner,
                &maps,
            )
        {
            return Err(self.caller_error(
                "loader-cache scratch lost its exact guarded logical or physical reservation",
            ));
        }
        Ok(state)
    }

    fn authenticate_loader_cache_scratch(&self, task: &Stopped) -> Result<LoaderCacheState, Error> {
        let state = self.authenticate_loader_cache_scratch_owner(task)?;
        let mut actual = vec![0_u8; LOADER_CACHE_SCRATCH_BYTES];
        task.read_exact(state.scratch.0 as usize, &mut actual)?;
        if actual != state.scratch_preimage {
            return Err(self.caller_error("loader-cache scratch preimage changed"));
        }
        Ok(state)
    }

    fn authenticate_loader_cache_descriptor(
        &self,
        task: &Stopped,
        config: &LiteinstAfterLoaderConfig,
        descriptor: u64,
    ) -> Result<LoaderCacheArtifactEvidence, Error> {
        let evidence = self.loader_cache_artifact_evidence(config)?;
        if !matches!(
            self.after_loader_private_state()?.owned_descriptors.get(&descriptor),
            Some(AfterLoaderOwnedDescriptor::LoaderCache {
                file,
                mapping,
                length,
                position: 0,
            }) if *file == evidence.file && *mapping == evidence.mapping && *length == evidence.length
        ) {
            return Err(self.caller_error("loader-cache descriptor authority changed"));
        }
        let path = format!("/proc/{}/fd/{descriptor}", task.pid());
        let metadata = std::fs::metadata(&path).map_err(|error| self.caller_error(error))?;
        let link = std::fs::read_link(&path).map_err(|error| self.caller_error(error))?;
        let flags =
            descriptor_flags(task.pid(), descriptor).map_err(|error| self.caller_error(error))?;
        let position = descriptor_position(task.pid(), descriptor)
            .map_err(|error| self.caller_error(error))?;
        let mut file =
            open_loader_cache_without_atime(&path).map_err(|error| self.caller_error(error))?;
        let seals = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GET_SEALS) };
        let mut bytes = Vec::new();
        (&mut file)
            .take(evidence.length.checked_add(1).ok_or(Errno::EOVERFLOW)?)
            .read_to_end(&mut bytes)
            .map_err(|error| self.caller_error(error))?;
        let expected_flags = libc::O_RDONLY as u64 | libc::O_CLOEXEC as u64 | KERNEL_O_LARGEFILE;
        if !metadata.is_file()
            || crate::after_loader::FileIdentity::from_metadata(&metadata) != evidence.file
            || metadata.len() != evidence.length
            || link.as_os_str().as_encoded_bytes() != LOADER_CACHE_MEMFD_LINK
            || flags != expected_flags
            || position != 0
            || seals != config.immutable_loader_cache().seals()
            || bytes.as_slice() != config.loader_cache().bytes()
        {
            return Err(self.caller_error(
                "live loader-cache descriptor identity, bytes, seals, flags or position differ",
            ));
        }
        Ok(evidence)
    }

    fn authenticate_loader_cache_alias_owned_descriptor(
        &self,
        task: &Stopped,
        config: &LiteinstAfterLoaderConfig,
        descriptor: u64,
        owned: &AfterLoaderOwnedDescriptor,
    ) -> Result<(PathBuf, AfterLoaderImageId), Error> {
        let AfterLoaderOwnedDescriptor::LoaderCacheAlias {
            image,
            soname,
            raw_path,
            canonical_path,
            file,
            bytes,
            stamp,
            position,
        } = owned
        else {
            return Err(self.caller_error("descriptor is not a typed loader-cache alias"));
        };
        let alias = config
            .loader_cache_alias_for_path(raw_path.as_os_str().as_bytes())
            .ok_or_else(|| self.caller_error("loader-cache alias left the bound cache profile"))?;
        let expected_image = self
            .after_loader_private_state()?
            .image_id(alias.dependency());
        let actual_mapping = loader_cache_alias_descriptor_matches(
            task.pid(),
            descriptor,
            canonical_path,
            *file,
            bytes,
            *stamp,
        )
        .map_err(|error| self.caller_error(error))?;
        let canonical_mapping = target_bound_image_mapping_identity(task.pid(), alias.dependency())
            .map_err(|error| self.caller_error(error))?;
        let expected_flags = libc::O_RDONLY as u64 | libc::O_CLOEXEC as u64 | KERNEL_O_LARGEFILE;
        if *image != expected_image
            || soname != alias.soname()
            || canonical_path != &alias.dependency().path
            || *file != alias.dependency().file_identity
            || bytes.as_ref() != alias.dependency().bytes.as_ref()
            || *stamp != alias.dependency_stamp()
            || descriptor_flags(task.pid(), descriptor).map_err(|error| self.caller_error(error))?
                != expected_flags
            || descriptor_position(task.pid(), descriptor)
                .map_err(|error| self.caller_error(error))?
                != *position
            || actual_mapping.is_none()
            || actual_mapping != canonical_mapping
            || !target_loader_cache_alias_matches(task.pid(), alias)
                .map_err(|error| self.caller_error(error))?
        {
            return Err(self.caller_error(
                "loader-cache alias descriptor identity, path, bytes, stamp, flags, position or target-root binding differ",
            ));
        }
        Ok((raw_path.clone(), *image))
    }

    fn authenticate_loader_cache_alias_descriptor(
        &self,
        task: &Stopped,
        config: &LiteinstAfterLoaderConfig,
        descriptor: u64,
    ) -> Result<(PathBuf, AfterLoaderImageId), Error> {
        let state = self.after_loader_private_state()?;
        let owned = state.owned_descriptors.get(&descriptor).ok_or_else(|| {
            self.caller_error("loader-cache alias descriptor ownership disappeared")
        })?;
        let (raw_path, image) =
            self.authenticate_loader_cache_alias_owned_descriptor(task, config, descriptor, owned)?;
        let lifecycle = state
            .loader_cache
            .as_ref()
            .and_then(|cache| cache.aliases.get(&raw_path))
            .ok_or_else(|| self.caller_error("loader-cache alias lifecycle disappeared"))?;
        let live = match lifecycle {
            LoaderCacheAliasLifecycle::Open {
                descriptor: expected,
                image: expected_image,
                ..
            }
            | LoaderCacheAliasLifecycle::Statted {
                descriptor: expected,
                image: expected_image,
            }
            | LoaderCacheAliasLifecycle::Mapped {
                descriptor: expected,
                image: expected_image,
                ..
            } => *expected == descriptor && *expected_image == image,
            LoaderCacheAliasLifecycle::AwaitingOpen { .. }
            | LoaderCacheAliasLifecycle::MappedClosed { .. }
            | LoaderCacheAliasLifecycle::Consumed { .. } => false,
        };
        if !live {
            return Err(self.caller_error("loader-cache alias descriptor lifecycle differs"));
        }
        Ok((raw_path, image))
    }

    fn authenticate_loader_cache_mapping(
        &self,
        task: &Stopped,
        config: &LiteinstAfterLoaderConfig,
        expected: LoaderCacheMapping,
    ) -> Result<(), Error> {
        let evidence = self.loader_cache_artifact_evidence(config)?;
        let state = self.after_loader_private_state()?;
        let lifecycle_descriptor = match state.loader_cache.as_ref().map(|cache| cache.lifecycle) {
            Some(LoaderCacheLifecycle::MappedOpen {
                descriptor,
                mapping,
            })
            | Some(LoaderCacheLifecycle::MappedClosed {
                descriptor,
                mapping,
            }) if mapping == expected => descriptor,
            _ => {
                return Err(self.caller_error("loader-cache mapping lifecycle authority changed"));
            }
        };
        let owned = state
            .owned_mappings
            .iter()
            .filter(|mapping| mapping.purpose == AfterLoaderMappingPurpose::LoaderCache)
            .collect::<Vec<_>>();
        let [owned] = owned.as_slice() else {
            return Err(self.caller_error("loader-cache mapping ownership is not unique"));
        };
        if expected.identity != evidence.mapping
            || expected.raw_length != evidence.length
            || owned.start != expected.start
            || owned.end != expected.end
            || !owned.readable
            || owned.writable
            || owned.executable
            || owned.shared
            || owned.descriptor != Some(lifecycle_descriptor)
            || owned.offset != 0
        {
            return Err(self.caller_error("owned loader-cache mapping geometry differs"));
        }
        let maps = guest_maps(task.pid()).ok_or(Errno::EPROTO)?;
        let physical = maps
            .iter()
            .filter(|mapping| {
                mapping.start == expected.start
                    && mapping.end == expected.end
                    && mapping.readable
                    && !mapping.writable
                    && !mapping.executable
                    && !mapping.shared
                    && mapping.offset == 0
                    && mapping.mapping_identity() == evidence.mapping
                    && mapping.path.as_ref().is_some_and(|path| {
                        path.as_os_str().as_encoded_bytes() == LOADER_CACHE_MEMFD_LINK
                    })
            })
            .count();
        if physical != 1 {
            return Err(self.caller_error("physical loader-cache mapping identity differs"));
        }
        let length = usize::try_from(expected.raw_length).map_err(|_| Errno::EOVERFLOW)?;
        let mut bytes = vec![0_u8; length];
        task.read_exact(expected.start as usize, &mut bytes)?;
        let raw_end = expected
            .start
            .checked_add(expected.raw_length)
            .ok_or(Errno::EOVERFLOW)?;
        if bytes.as_slice() != config.loader_cache().bytes()
            || !target_range_is_zero(task, raw_end, expected.end - raw_end)?
        {
            return Err(self.caller_error("live loader-cache mapping bytes differ"));
        }
        Ok(())
    }

    fn observe_ptmalloc_bootstrap(
        &self,
        task: &Stopped,
        config: &LiteinstAfterLoaderConfig,
    ) -> Result<Option<(u64, PtmallocBootstrapObservation)>, Error> {
        if !ptmalloc_bootstrap_profile_matches(&config.provider.bytes)
            .map_err(|message| self.caller_error(message))?
        {
            return Ok(None);
        }
        let state = self.after_loader_private_state()?;
        let geometry = state
            .image_geometry(&config.provider)
            .ok_or_else(|| self.caller_error("profiled ptmalloc provider geometry is unbound"))?;
        if geometry.image != state.image_id(&config.provider) {
            return Err(self.caller_error("profiled ptmalloc provider identity changed"));
        }
        let renewed = self.resolve_after_loader_image_geometry(
            task,
            &config.provider,
            state.image_id(&config.provider),
            "ptmalloc-bootstrap-renewal",
        )?;
        if renewed != geometry {
            return Err(self.caller_error("profiled ptmalloc provider geometry changed"));
        }
        self.authenticate_after_loader_image_backing(task, &config.provider, renewed)?;
        let function = geometry
            .load_bias
            .checked_add(PTMALLOC_BOOTSTRAP_FUNCTION_RVA)
            .ok_or(Errno::EOVERFLOW)?;
        let function_end = function
            .checked_add(PTMALLOC_BOOTSTRAP_FUNCTION.len() as u64)
            .ok_or(Errno::EOVERFLOW)?;
        let tcache_key = geometry
            .load_bias
            .checked_add(PTMALLOC_BOOTSTRAP_TCACHE_KEY_RVA)
            .ok_or(Errno::EOVERFLOW)?;
        let initialized = geometry
            .load_bias
            .checked_add(PTMALLOC_BOOTSTRAP_INITIALIZED_RVA)
            .ok_or(Errno::EOVERFLOW)?;
        if function < geometry.span.0
            || geometry.span.1 < function_end
            || tcache_key < geometry.span.0
            || geometry.span.1 < tcache_key.checked_add(8).ok_or(Errno::EOVERFLOW)?
            || initialized < geometry.span.0
            || geometry.span.1 < initialized.checked_add(1).ok_or(Errno::EOVERFLOW)?
        {
            return Err(self.caller_error("profiled ptmalloc ranges left provider geometry"));
        }
        let maps = guest_maps(task.pid()).ok_or(Errno::EPROTO)?;
        let function_range = GuestRange::new(function, PTMALLOC_BOOTSTRAP_FUNCTION.len() as u64)
            .ok_or(Errno::EOVERFLOW)?;
        let executable_maps = maps
            .iter()
            .filter(|mapping| {
                mapping.readable
                    && mapping.executable
                    && !mapping.writable
                    && !mapping.shared
                    && mapping_identity_matches(geometry.mapping, mapping)
                    && mapping.contains_range(function_range)
            })
            .count();
        if executable_maps != 1 {
            return Err(
                self.caller_error("profiled ptmalloc function lacks one exact provider RX mapping")
            );
        }
        let provider_elf = Elf::parse(&config.provider.bytes).map_err(|error| {
            self.caller_error(format!("profiled provider parse failed: {error}"))
        })?;
        let writable = provider_elf
            .program_headers
            .iter()
            .filter(|load| load.p_type == ph::PT_LOAD && load.p_flags == (ph::PF_R | ph::PF_W))
            .collect::<Vec<_>>();
        let [writable] = writable.as_slice() else {
            return Err(self.caller_error("profiled provider writable PT_LOAD changed"));
        };
        let bss_start = geometry
            .load_bias
            .checked_add(page_up(
                writable
                    .p_vaddr
                    .checked_add(writable.p_filesz)
                    .ok_or(Errno::EOVERFLOW)?,
            )?)
            .ok_or(Errno::EOVERFLOW)?;
        let bss_end = geometry
            .load_bias
            .checked_add(page_up(
                writable
                    .p_vaddr
                    .checked_add(writable.p_memsz)
                    .ok_or(Errno::EOVERFLOW)?,
            )?)
            .ok_or(Errno::EOVERFLOW)?;
        let state_range = GuestRange::new(tcache_key, 8).ok_or(Errno::EOVERFLOW)?;
        let initialized_range = GuestRange::new(initialized, 1).ok_or(Errno::EOVERFLOW)?;
        let bss_maps = maps
            .iter()
            .filter(|mapping| {
                mapping.start == bss_start
                    && mapping.end == bss_end
                    && mapping.offset == 0
                    && mapping.readable
                    && mapping.writable
                    && !mapping.executable
                    && !mapping.shared
                    && mapping.inode == 0
                    && mapping.path.is_none()
                    && mapping.contains_range(state_range)
                    && mapping.contains_range(initialized_range)
                    && guest_hook_mapping_attributes(task.pid(), mapping).is_some_and(
                        |attributes| attributes.fork_safe && attributes.protection_key == 0,
                    )
            })
            .count();
        if bss_maps != 1 {
            return Err(self.caller_error(
                "profiled ptmalloc state lacks one exact private anonymous BSS mapping",
            ));
        }
        let mut live_function = vec![0_u8; PTMALLOC_BOOTSTRAP_FUNCTION.len()];
        task.read_exact(function as usize, &mut live_function)?;
        if live_function != PTMALLOC_BOOTSTRAP_FUNCTION {
            return Err(self.caller_error("live profiled ptmalloc function bytes differ"));
        }
        let mut key = [0_u8; 8];
        task.read_exact(tcache_key as usize, &mut key)?;
        let mut initialized_byte = [0_u8; 1];
        task.read_exact(initialized as usize, &mut initialized_byte)?;
        Ok(Some((
            geometry.load_bias,
            PtmallocBootstrapObservation {
                initialized: initialized_byte[0],
                tcache_key: key,
            },
        )))
    }

    fn initialize_ptmalloc_bootstrap_state(
        &mut self,
        task: &Stopped,
        config: &LiteinstAfterLoaderConfig,
    ) -> Result<(), Error> {
        let observed = self.observe_ptmalloc_bootstrap(task, config)?;
        if self
            .after_loader_private_state()?
            .ptmalloc_bootstrap
            .is_some()
        {
            return Err(self.caller_error("profiled ptmalloc entry observation was repeated"));
        }
        let Some((load_bias, entry)) = observed else {
            return Ok(());
        };
        if !matches!(entry.initialized, 0 | 1)
            || entry.initialized == 0 && entry.tcache_key != [0; 8]
        {
            return Err(self.caller_error("profiled ptmalloc entry state is not canonical"));
        }
        self.after_loader_private_state_mut()?.ptmalloc_bootstrap = Some(PtmallocBootstrapState {
            load_bias,
            entry,
            pre_dlopen_verified: false,
            entropy_consumed: false,
            injected_key: None,
        });
        self.caller_observe(
            "profiled ptmalloc entry state",
            format!(
                "initialized={} tcache_key_sha256={}",
                entry.initialized,
                sha256_hex(&entry.tcache_key),
            ),
        )
    }

    fn verify_ptmalloc_bootstrap_before_dlopen(
        &mut self,
        task: &Stopped,
        config: &LiteinstAfterLoaderConfig,
    ) -> Result<(), Error> {
        let observed = self.observe_ptmalloc_bootstrap(task, config)?;
        let Some(mut profile) = self.after_loader_private_state()?.ptmalloc_bootstrap else {
            return if observed.is_none() {
                Ok(())
            } else {
                Err(self.caller_error("profiled ptmalloc state appeared after entry"))
            };
        };
        let Some((load_bias, current)) = observed else {
            return Err(self.caller_error("profiled ptmalloc provider disappeared before dlopen"));
        };
        if profile.pre_dlopen_verified
            || profile.entropy_consumed
            || profile.injected_key.is_some()
            || profile.load_bias != load_bias
            || profile.entry != current
        {
            return Err(self.caller_error("profiled ptmalloc state changed before dlopen"));
        }
        profile.pre_dlopen_verified = true;
        self.after_loader_private_state_mut()?.ptmalloc_bootstrap = Some(profile);
        self.caller_observe(
            "profiled ptmalloc pre-dlopen state",
            format!(
                "initialized={} tcache_key_sha256={}",
                current.initialized,
                sha256_hex(&current.tcache_key),
            ),
        )
    }

    fn verify_ptmalloc_bootstrap_retained(
        &self,
        task: &Stopped,
        config: &LiteinstAfterLoaderConfig,
    ) -> Result<(), Error> {
        let observed = self.observe_ptmalloc_bootstrap(task, config)?;
        let Some(profile) = self.after_loader_private_state()?.ptmalloc_bootstrap else {
            return if observed.is_none() {
                Ok(())
            } else {
                Err(self.caller_error("profiled ptmalloc state appeared after dlopen"))
            };
        };
        let Some((load_bias, current)) = observed else {
            return Err(self.caller_error("profiled ptmalloc provider disappeared after dlopen"));
        };
        let exact = profile.pre_dlopen_verified
            && profile.load_bias == load_bias
            && match profile.entry.initialized {
                0 => {
                    profile.entropy_consumed
                        && profile.injected_key.is_some()
                        && self
                            .after_loader_private_state()?
                            .private_brk_growth_consumed
                        && current.initialized == 1
                        && current.tcache_key == profile.injected_key.unwrap()
                }
                1 => {
                    !profile.entropy_consumed
                        && profile.injected_key.is_none()
                        && !self
                            .after_loader_private_state()?
                            .private_brk_growth_consumed
                        && current == profile.entry
                }
                _ => false,
            };
        if !exact {
            return Err(self.caller_error("profiled ptmalloc post-dlopen state differs"));
        }
        Ok(())
    }

    pub(super) fn caller_observe(
        &self,
        operation: &str,
        detail: impl Into<String>,
    ) -> Result<(), Error> {
        let config = self.after_loader_config().ok_or(Errno::EPROTO)?;
        config
            .diagnostics
            .record(operation, self.timer.diagnostic_clock(), detail)
            .map_err(|e| self.caller_error(e))
    }

    fn caller_cstring(&self, task: &Stopped, address: u64, limit: usize) -> Result<Vec<u8>, Error> {
        let maps = guest_maps(task.pid()).ok_or(Errno::EPROTO)?;
        let mapping = maps
            .iter()
            .find(|mapping| mapping.readable && mapping.contains(address))
            .ok_or(Errno::EFAULT)?;
        let available = usize::try_from(mapping.end - address)
            .map_err(|_| Errno::EOVERFLOW)?
            .min(limit.checked_add(1).ok_or(Errno::EOVERFLOW)?);
        let mut bytes = vec![0; available];
        task.read_exact(address as usize, &mut bytes)?;
        let nul = bytes
            .iter()
            .position(|byte| *byte == 0)
            .ok_or_else(|| self.caller_error("private string is unterminated or oversized"))?;
        bytes.truncate(nul);
        Ok(bytes)
    }

    fn caller_cstring_vector(
        &self,
        task: &Stopped,
        address: u64,
        maximum_items: usize,
        maximum_bytes: usize,
    ) -> Result<Vec<Vec<u8>>, Error> {
        if address == 0 {
            return Err(self.caller_error("private string vector is null"));
        }
        let mut result = Vec::new();
        let mut bytes = 0_usize;
        for index in 0..=maximum_items {
            let pointer_address = address
                .checked_add(
                    u64::try_from(index)
                        .map_err(|_| Errno::EOVERFLOW)?
                        .checked_mul(8)
                        .ok_or(Errno::EOVERFLOW)?,
                )
                .ok_or(Errno::EOVERFLOW)?;
            let pointer_range = GuestRange::new(pointer_address, 8).ok_or(Errno::EFAULT)?;
            if !guest_maps(task.pid()).is_some_and(|maps| {
                maps.iter()
                    .any(|mapping| mapping.readable && mapping.contains_range(pointer_range))
            }) {
                return Err(self.caller_error("private string-vector pointer is unreadable"));
            }
            let mut raw_pointer = [0; 8];
            task.read_exact(pointer_address as usize, &mut raw_pointer)?;
            let pointer = u64::from_ne_bytes(raw_pointer);
            if pointer == 0 {
                return Ok(result);
            }
            if index == maximum_items {
                return Err(self.caller_error("private string vector exceeds its item bound"));
            }
            let remaining = maximum_bytes
                .checked_sub(bytes)
                .ok_or_else(|| self.caller_error("private string vector exceeds its byte bound"))?;
            let item = self.caller_cstring(task, pointer, remaining)?;
            bytes = bytes
                .checked_add(item.len())
                .and_then(|value| value.checked_add(1))
                .ok_or(Errno::EOVERFLOW)?;
            if bytes > maximum_bytes {
                return Err(self.caller_error("private string vector exceeds its byte bound"));
            }
            result.push(item);
        }
        Err(self.caller_error("private string vector is unterminated"))
    }

    fn validate_after_loader_command_execve(
        &self,
        task: &Stopped,
        config: &LiteinstAfterLoaderConfig,
        registers: &libc::user_regs_struct,
    ) -> Result<(), Error> {
        let expected_path = config.executable.path.as_os_str().as_encoded_bytes();
        if self.caller_cstring(task, registers.rdi, 4096)? != expected_path {
            return Err(self.caller_error("command-bootstrap exec path differs"));
        }
        let argv = self.caller_cstring_vector(task, registers.rsi, 1, 4096)?;
        if argv.len() != 1 || argv[0].as_slice() != expected_path {
            return Err(self.caller_error("command-bootstrap argv differs"));
        }
        let environment = self.caller_cstring_vector(task, registers.rdx, 256, 1024 * 1024)?;
        let mut observed = BTreeMap::new();
        for item in environment {
            let separator = item.iter().position(|byte| *byte == b'=').ok_or_else(|| {
                self.caller_error("command-bootstrap environment entry lacks '='")
            })?;
            if separator == 0 {
                return Err(self.caller_error("command-bootstrap environment key is empty"));
            }
            let key = std::ffi::OsString::from_vec(item[..separator].to_vec());
            let value = std::ffi::OsString::from_vec(item[separator + 1..].to_vec());
            if observed.insert(key, value).is_some() {
                return Err(self.caller_error("command-bootstrap environment key is duplicated"));
            }
        }
        config
            .validate_environment(&observed)
            .map_err(|error| self.caller_error(error))
    }

    fn after_loader_private_state(&self) -> Result<&AfterLoaderPrivateState, Error> {
        self.liteinst_after_loader_private_state
            .as_ref()
            .ok_or_else(|| self.caller_error("private resource state is unavailable"))
    }

    fn after_loader_private_state_mut(&mut self) -> Result<&mut AfterLoaderPrivateState, Error> {
        self.liteinst_after_loader_private_state
            .as_mut()
            .ok_or(Errno::EPROTO.into())
    }

    fn validate_after_loader_output_spans(&self, spans: &[(u64, u64)]) -> Result<(), Error> {
        let state = self.after_loader_private_state()?;
        if spans
            .iter()
            .copied()
            .any(|span| !state.owns_range(span, true) || state.loader_cache_scratch_overlaps(span))
        {
            return Err(self.caller_error(
                "private syscall output is outside writable ownership or overlaps cache scratch",
            ));
        }
        Ok(())
    }

    fn authenticate_proc_fd_audit_call(
        &self,
        task: &Stopped,
        config: &LiteinstAfterLoaderConfig,
        permit: &AfterLoaderSyscallPermit,
        data_spans: &[(u64, u64)],
    ) -> Result<(), Error> {
        let profile = proc_fd_audit_profile(&config.sealed_runtime.image.bytes)
            .map_err(|reason| self.caller_error(reason))?;
        let state = self.after_loader_private_state()?;
        let active = self
            .liteinst_after_loader_private_call
            .as_ref()
            .ok_or_else(|| self.caller_error("proc-fd audit has no active private call"))?;
        if active.function != CallerFunction::Initializer {
            return Err(self.caller_error("proc-fd audit is outside the active initializer call"));
        }
        let geometry = state
            .image_geometry(&config.sealed_runtime.image)
            .ok_or_else(|| self.caller_error("proc-fd audit runtime geometry is unbound"))?;
        self.authenticate_after_loader_image_backing(task, &config.sealed_runtime.image, geometry)?;
        let registers = task.getregs()?;
        let controller_stack = state.controller_call_stack_range().ok_or_else(|| {
            self.caller_error("proc-fd audit controller call stack is unavailable")
        })?;
        if controller_stack.end != active.call_stack_top
            || active
                .call_stack_top
                .checked_sub(STACK_SIZE)
                .is_none_or(|start| start != controller_stack.start)
        {
            return Err(self.caller_error(
                "proc-fd audit stack differs from the active initializer call stack",
            ));
        }
        if !controller_call_stack_maps_are_exact(
            &guest_maps(task.pid()).ok_or(Errno::EPROTO)?,
            controller_stack,
        ) {
            return Err(self.caller_error(
                "proc-fd audit controller call stack guards or physical mapping differ",
            ));
        }
        let mut call_chain = [0_u8; 32];
        task.read_exact(registers.rsp as usize, &mut call_chain)?;
        let stack_word =
            |offset: usize| u64::from_ne_bytes(call_chain[offset..offset + 8].try_into().unwrap());
        let raw_return_address = stack_word(0);
        let copied_arg5 = stack_word(8);
        let return_address = stack_word(16);
        let original_arg5 = stack_word(24);
        let controller_return_slot = active
            .call_stack_top
            .checked_sub(8)
            .ok_or(Errno::EOVERFLOW)?;
        let mut raw_controller_return = [0_u8; 8];
        task.read_exact(controller_return_slot as usize, &mut raw_controller_return)?;
        let controller_return_address = u64::from_ne_bytes(raw_controller_return);
        let expected_controller_return = active.entry.checked_add(17).ok_or(Errno::EOVERFLOW)?;
        let call_start = return_address.checked_sub(5).ok_or(Errno::EPROTO)?;
        let mut call_instruction = [0_u8; 5];
        task.read_exact(call_start as usize, &mut call_instruction)?;
        let raw_call_start = raw_return_address.checked_sub(5).ok_or(Errno::EPROTO)?;
        let mut raw_call_instruction = [0_u8; 5];
        task.read_exact(raw_call_start as usize, &mut raw_call_instruction)?;
        let raw_start = geometry
            .load_bias
            .checked_add(profile.raw_syscall_start)
            .ok_or(Errno::EOVERFLOW)?;
        let trusted_gate = geometry
            .load_bias
            .checked_add(profile.trusted_gate)
            .ok_or(Errno::EOVERFLOW)?;
        let mut shim = [0_u8; 14];
        task.read_exact(raw_start as usize, &mut shim)?;
        let mut gate = [0_u8; 26];
        task.read_exact(trusted_gate as usize, &mut gate)?;
        let shim_call = shim[4..9].try_into().unwrap();
        if shim[..4] != [0xff, 0x74, 0x24, 0x08]
            || shim[9..] != [0x48, 0x83, 0xc4, 0x08, 0xc3]
            || rel32_call_target(raw_start.checked_add(9).ok_or(Errno::EOVERFLOW)?, shim_call)
                != Some(trusted_gate)
            || gate
                != [
                    0x48, 0x89, 0xf8, 0x48, 0x89, 0xf7, 0x48, 0x89, 0xd6, 0x48, 0x89, 0xca, 0x4d,
                    0x89, 0xc2, 0x4d, 0x89, 0xc8, 0x4c, 0x8b, 0x4c, 0x24, 0x08, 0x0f, 0x05, 0xc3,
                ]
        {
            return Err(self.caller_error("proc-fd raw syscall shim or trusted gate bytes differ"));
        }
        if !proc_fd_audit_site_is_exact(
            profile,
            geometry.load_bias,
            permit,
            &registers,
            controller_stack,
            controller_return_slot,
            controller_return_address,
            expected_controller_return,
            return_address,
            call_instruction,
            raw_return_address,
            raw_call_instruction,
            copied_arg5,
            original_arg5,
            data_spans,
        ) {
            return Err(self.caller_error(
                "proc-fd audit site, call chain or controller-stack data ranges differ",
            ));
        }
        Ok(())
    }

    fn bind_after_loader_syscall_effect(
        &mut self,
        effect: AfterLoaderSyscallEffect,
    ) -> Result<(), Error> {
        let permit = self
            .liteinst_after_loader_syscall_permit
            .as_mut()
            .ok_or(Errno::EPROTO)?;
        permit.effect = effect;
        Ok(())
    }

    fn after_loader_cache_open_effect(
        &self,
        task: &Stopped,
        config: &LiteinstAfterLoaderConfig,
        permit: &AfterLoaderSyscallPermit,
    ) -> Result<AfterLoaderSyscallEffect, Error> {
        let geometry = self.authenticate_loader_cache_interpreter_site(
            task,
            config,
            permit,
            LOADER_CACHE_OPENAT_SYSCALL_RVA,
        )?;
        let logical_path = geometry
            .load_bias
            .checked_add(LOADER_CACHE_PATH_RVA)
            .ok_or(Errno::EOVERFLOW)?;
        if !exact_loader_cache_openat_arguments(&permit.args, logical_path)
            || self.caller_cstring(task, logical_path, LOADER_CACHE_PATH.len())?
                != LOADER_CACHE_PATH
        {
            return Err(self.caller_error(
                "loader-cache openat logical path, raw flags, mode or preserved wrapper registers differ",
            ));
        }
        let cache = self.authenticate_loader_cache_scratch(task)?;
        let state = self.after_loader_private_state()?;
        if cache.lifecycle != LoaderCacheLifecycle::AwaitingOpen
            || state.owned_descriptors.values().any(|descriptor| {
                matches!(descriptor, AfterLoaderOwnedDescriptor::LoaderCache { .. })
            })
            || state
                .owned_mappings
                .iter()
                .any(|mapping| mapping.purpose == AfterLoaderMappingPurpose::LoaderCache)
        {
            return Err(self.caller_error(
                "loader-cache openat was duplicated or followed an earlier cache lifecycle step",
            ));
        }
        let evidence = self.loader_cache_artifact_evidence(config)?;
        let admission_regs = task.getregs()?;
        if !after_loader_syscall_registers_match(permit.number, permit.args, &admission_regs)
            || admission_regs.rip != permit.resume_pointer
        {
            return Err(self.caller_error(
                "loader-cache admission registers differ from the bound logical syscall",
            ));
        }
        let mut path = config
            .immutable_loader_cache()
            .sealed_source()
            .as_os_str()
            .as_encoded_bytes()
            .to_vec();
        path.push(0);
        let redirect = LoaderCacheRedirect {
            scratch: cache.scratch,
            preimage: cache.scratch_preimage,
            path,
            admission_registers: register_words(&admission_regs),
            admission_xstate: task.get_x86_extended_state()?,
        };
        if loader_cache_redirect_arguments(permit.args, &redirect).is_none() {
            return Err(self.caller_error("sealed loader-cache redirect path exceeds its scratch"));
        }
        Ok(AfterLoaderSyscallEffect::OpenLoaderCache {
            descriptor: AfterLoaderOwnedDescriptor::LoaderCache {
                file: evidence.file,
                mapping: evidence.mapping,
                length: evidence.length,
                position: 0,
            },
            redirect,
        })
    }

    fn after_loader_cache_stat_effect(
        &self,
        task: &Stopped,
        config: &LiteinstAfterLoaderConfig,
        permit: &AfterLoaderSyscallPermit,
    ) -> Result<AfterLoaderSyscallEffect, Error> {
        self.authenticate_loader_cache_interpreter_site(
            task,
            config,
            permit,
            LOADER_CACHE_FSTAT_SYSCALL_RVA,
        )?;
        let descriptor = permit.args[0];
        let cache = self.authenticate_loader_cache_scratch(task)?;
        if cache.lifecycle != (LoaderCacheLifecycle::Open { descriptor }) {
            return Err(self.caller_error("loader-cache fstat is duplicated or out of order"));
        }
        self.authenticate_loader_cache_descriptor(task, config, descriptor)?;
        let metadata = std::fs::metadata(format!("/proc/{}/fd/{descriptor}", task.pid()))
            .map_err(|error| self.caller_error(error))?;
        Ok(AfterLoaderSyscallEffect::StatLoaderCache {
            descriptor,
            destination: permit.args[1],
            fields: AfterLoaderStatFields::from_metadata(&metadata),
        })
    }

    fn after_loader_cache_map_effect(
        &self,
        task: &Stopped,
        config: &LiteinstAfterLoaderConfig,
        permit: &AfterLoaderSyscallPermit,
    ) -> Result<AfterLoaderSyscallEffect, Error> {
        self.authenticate_loader_cache_interpreter_site(
            task,
            config,
            permit,
            LOADER_CACHE_MMAP_SYSCALL_RVA,
        )?;
        let descriptor = permit.args[4];
        let evidence = self.authenticate_loader_cache_descriptor(task, config, descriptor)?;
        let cache = self.authenticate_loader_cache_scratch(task)?;
        if cache.lifecycle != (LoaderCacheLifecycle::Statted { descriptor })
            || !loader_cache_mmap_arguments_match(permit.args, descriptor, evidence.length)
        {
            return Err(self.caller_error(
                "loader-cache mmap is out of order or differs in address, length, protection, flags, descriptor or offset",
            ));
        }
        Ok(AfterLoaderSyscallEffect::MapLoaderCache {
            descriptor,
            raw_length: evidence.length,
            identity: evidence.mapping,
        })
    }

    fn after_loader_cache_close_effect(
        &self,
        task: &Stopped,
        config: &LiteinstAfterLoaderConfig,
        permit: &AfterLoaderSyscallPermit,
    ) -> Result<AfterLoaderSyscallEffect, Error> {
        self.authenticate_loader_cache_interpreter_site(
            task,
            config,
            permit,
            LOADER_CACHE_CLOSE_SYSCALL_RVA,
        )?;
        let descriptor = permit.args[0];
        let cache = self.authenticate_loader_cache_scratch(task)?;
        let LoaderCacheLifecycle::MappedOpen {
            descriptor: expected_descriptor,
            mapping,
        } = cache.lifecycle
        else {
            return Err(self.caller_error("loader-cache close is duplicated or out of order"));
        };
        if descriptor != expected_descriptor {
            return Err(self.caller_error("loader-cache close names another descriptor"));
        }
        self.authenticate_loader_cache_descriptor(task, config, descriptor)?;
        self.authenticate_loader_cache_mapping(task, config, mapping)?;
        Ok(AfterLoaderSyscallEffect::CloseLoaderCache {
            descriptor,
            mapping,
        })
    }

    fn after_loader_cache_retire_effect(
        &self,
        task: &Stopped,
        config: &LiteinstAfterLoaderConfig,
        permit: &AfterLoaderSyscallPermit,
    ) -> Result<Option<AfterLoaderSyscallEffect>, Error> {
        let cache = self.authenticate_loader_cache_scratch(task)?;
        let LoaderCacheLifecycle::MappedClosed {
            descriptor,
            mapping,
        } = cache.lifecycle
        else {
            return Ok(None);
        };
        if !cache
            .aliases
            .values()
            .all(LoaderCacheAliasLifecycle::is_consumed)
            || self
                .after_loader_private_state()?
                .owned_descriptors
                .values()
                .any(|owned| matches!(owned, AfterLoaderOwnedDescriptor::LoaderCacheAlias { .. }))
        {
            return Err(self.caller_error(
                "loader-cache munmap preceded exact consumption of every cache-derived alias",
            ));
        }
        self.authenticate_loader_cache_scratch(task)?;
        let requested = checked_page_effect_range(permit.args[0], permit.args[1])?;
        if !ranges_overlap((mapping.start, mapping.end), requested) {
            return Ok(None);
        }
        self.authenticate_loader_cache_interpreter_site(
            task,
            config,
            permit,
            LOADER_CACHE_MUNMAP_SYSCALL_RVA,
        )?;
        if permit.args[0] != mapping.start || permit.args[1] != mapping.raw_length {
            return Err(self.caller_error(
                "loader-cache munmap is partial or differs from its exact raw mapping range",
            ));
        }
        if !procfs_path_is_absent(format!("/proc/{}/fd/{descriptor}", task.pid()))
            .map_err(|error| self.caller_error(error))?
        {
            return Err(self.caller_error("closed loader-cache descriptor reappeared"));
        }
        self.authenticate_loader_cache_mapping(task, config, mapping)?;
        Ok(Some(AfterLoaderSyscallEffect::RetireLoaderCache {
            descriptor,
            mapping,
        }))
    }

    fn after_loader_proc_fd_directory_open_effect(
        &self,
        task: &Stopped,
        config: &LiteinstAfterLoaderConfig,
        permit: &AfterLoaderSyscallPermit,
    ) -> Result<AfterLoaderSyscallEffect, Error> {
        let args = permit.args;
        let path_span = checked_range(args[1], (PROC_SELF_FD_DIRECTORY.len() + 1) as u64)?;
        self.authenticate_proc_fd_audit_call(task, config, permit, &[path_span])?;
        let state = self.after_loader_private_state()?;
        if !exact_proc_fd_directory_openat_arguments(&args)
            || self.caller_cstring(task, args[1], PROC_SELF_FD_DIRECTORY.len() + 1)?
                != PROC_SELF_FD_DIRECTORY
            || state.proc_fd_audit != ProcFdAuditLifecycle::AwaitingOpen
            || state.owned_descriptors.values().any(|descriptor| {
                matches!(
                    descriptor,
                    AfterLoaderOwnedDescriptor::ProcFdDirectory { .. }
                )
            })
        {
            return Err(self.caller_error(
                "private proc-fd directory open differs in path, flags, phase or uniqueness",
            ));
        }
        Ok(AfterLoaderSyscallEffect::OpenProcFdDirectory {
            expected: proc_fd_link_snapshot(task.pid())
                .map_err(|error| self.caller_error(error))?,
        })
    }

    fn after_loader_proc_fd_getdents_effect(
        &self,
        task: &Stopped,
        config: &LiteinstAfterLoaderConfig,
        permit: &AfterLoaderSyscallPermit,
    ) -> Result<AfterLoaderSyscallEffect, Error> {
        let args = permit.args;
        let output_span = checked_range(args[1], args[2])?;
        self.authenticate_proc_fd_audit_call(task, config, permit, &[output_span])?;
        let descriptor = self
            .after_loader_private_state()?
            .owned_descriptors
            .get(&args[0])
            .ok_or_else(|| self.caller_error("private getdents descriptor is not owned"))?;
        let AfterLoaderOwnedDescriptor::ProcFdDirectory {
            expected,
            seen,
            linked,
            eof,
        } = descriptor
        else {
            return Err(self.caller_error("private getdents descriptor is not the proc-fd audit"));
        };
        if !exact_proc_fd_getdents_arguments(&args)
            || self.after_loader_private_state()?.proc_fd_audit
                != (ProcFdAuditLifecycle::Scanning {
                    descriptor: args[0],
                })
            || *eof
            || linked != seen
            || proc_fd_link_snapshot(task.pid()).map_err(|error| self.caller_error(error))?
                != *expected
        {
            return Err(self.caller_error(
                "private proc-fd getdents shape, lifecycle or live descriptor set differs",
            ));
        }
        Ok(AfterLoaderSyscallEffect::ReadProcFdDirectory {
            descriptor: args[0],
            destination: args[1],
            capacity: args[2],
        })
    }

    fn after_loader_proc_fd_readlink_effect(
        &self,
        task: &Stopped,
        config: &LiteinstAfterLoaderConfig,
        permit: &AfterLoaderSyscallPermit,
    ) -> Result<AfterLoaderSyscallEffect, Error> {
        let args = permit.args;
        if !exact_proc_fd_readlink_arguments(&args) {
            return Err(self.caller_error("private proc-fd readlink argument shape differs"));
        }
        let path = self.caller_cstring(task, args[1], 64)?;
        let path_span = checked_range(
            args[1],
            u64::try_from(path.len() + 1).map_err(|_| Errno::EOVERFLOW)?,
        )?;
        let output_span = checked_range(args[2], args[3])?;
        self.authenticate_proc_fd_audit_call(task, config, permit, &[path_span, output_span])?;
        let descriptor = proc_self_fd_number(&path)
            .ok_or_else(|| self.caller_error("private proc-fd readlink path is not canonical"))?;
        let directories = self
            .after_loader_private_state()?
            .owned_descriptors
            .iter()
            .filter_map(|(directory, owned)| match owned {
                AfterLoaderOwnedDescriptor::ProcFdDirectory {
                    expected,
                    seen,
                    linked,
                    eof,
                } => Some((*directory, expected, seen, linked, *eof)),
                _ => None,
            })
            .collect::<Vec<_>>();
        let [(directory, expected, seen, linked, eof)] = directories.as_slice() else {
            return Err(self.caller_error("private proc-fd readlink audit is not unique"));
        };
        if self.after_loader_private_state()?.proc_fd_audit
            != (ProcFdAuditLifecycle::Scanning {
                descriptor: *directory,
            })
        {
            return Err(self.caller_error("private proc-fd readlink lifecycle is not scanning"));
        }
        let target = expected.get(&descriptor).ok_or_else(|| {
            self.caller_error("private proc-fd readlink target is outside the bound descriptor set")
        })?;
        if *eof
            || !seen.contains(&descriptor)
            || linked.contains(&descriptor)
            || target.len() >= PROC_FD_READLINK_BYTES as usize
            || proc_fd_link_snapshot(task.pid()).map_err(|error| self.caller_error(error))?
                != **expected
        {
            return Err(self.caller_error(
                "private proc-fd readlink lifecycle, target length or live descriptor set differs",
            ));
        }
        Ok(AfterLoaderSyscallEffect::ReadProcFdLink {
            directory: *directory,
            descriptor,
            destination: args[2],
            expected: target.clone(),
        })
    }

    fn after_loader_open_effect(
        &self,
        task: &Stopped,
        config: &LiteinstAfterLoaderConfig,
        args: [u64; 6],
    ) -> Result<AfterLoaderSyscallEffect, Error> {
        if !exact_readonly_openat_arguments(&args) {
            return Err(
                self.caller_error("private openat flags or directory are not read-only and bound")
            );
        }
        let path = self.caller_cstring(task, args[1], 4096)?;
        if path == LOADER_CACHE_PATH {
            return Err(self.caller_error(
                "logical loader-cache open requires its exact interpreter-site redirect",
            ));
        }
        if let Some(alias) = config.loader_cache_alias_for_path(&path) {
            let cache = self.authenticate_loader_cache_scratch(task)?;
            let LoaderCacheLifecycle::MappedClosed {
                descriptor,
                mapping,
            } = cache.lifecycle
            else {
                return Err(self.caller_error(
                    "bound loader-cache alias appeared before the exact cache mapping was closed",
                ));
            };
            self.authenticate_loader_cache_mapping(task, config, mapping)?;
            let descriptor_absent =
                procfs_path_is_absent(format!("/proc/{}/fd/{descriptor}", task.pid()))
                    .map_err(|error| self.caller_error(error))?;
            if !descriptor_absent
                || alias.raw_path().as_os_str().as_encoded_bytes() != path
                || alias.dependency().dynamic_soname().as_deref() != Some(alias.soname())
                || !config.deferred_dependencies.iter().any(|dependency| {
                    dependency.path == alias.dependency().path
                        && dependency.file_identity == alias.dependency().file_identity
                        && dependency.bytes == alias.dependency().bytes
                })
            {
                return Err(self.caller_error(
                    "bound loader-cache alias authority changed after cache consumption",
                ));
            }
            let state = self.after_loader_private_state()?;
            let image = state.image_id(alias.dependency());
            if cache.aliases.get(alias.raw_path())
                != Some(&LoaderCacheAliasLifecycle::AwaitingOpen { image })
                || !target_loader_cache_alias_matches(task.pid(), alias)
                    .map_err(|error| self.caller_error(error))?
            {
                return Err(self.caller_error(
                    "bound loader-cache alias is duplicated or changed in the target mount namespace",
                ));
            }
            return Ok(AfterLoaderSyscallEffect::OpenLoaderCacheAlias(
                AfterLoaderOwnedDescriptor::LoaderCacheAlias {
                    image,
                    soname: alias.soname().to_owned(),
                    raw_path: alias.raw_path().to_path_buf(),
                    canonical_path: alias.dependency().path.clone(),
                    file: alias.dependency().file_identity,
                    bytes: alias.dependency().bytes.clone(),
                    stamp: alias.dependency_stamp(),
                    position: 0,
                },
            ));
        }
        if path
            == config
                .sealed_runtime
                .image
                .path
                .as_os_str()
                .as_encoded_bytes()
        {
            return Ok(AfterLoaderSyscallEffect::Open(
                AfterLoaderOwnedDescriptor::SealedRuntime {
                    bytes: config.sealed_runtime.image.bytes.clone(),
                    position: 0,
                },
            ));
        }
        if path == b"/proc/self/maps" {
            let proc_path = format!("/proc/{}/maps", task.pid());
            let metadata =
                std::fs::metadata(&proc_path).map_err(|error| self.caller_error(error))?;
            let bytes = bounded_proc(&proc_path, 2 * 1024 * 1024)
                .map_err(|error| self.caller_error(error))?;
            return Ok(AfterLoaderSyscallEffect::Open(
                AfterLoaderOwnedDescriptor::ProcMaps {
                    device: metadata.dev(),
                    inode: metadata.ino(),
                    bytes: bytes.into(),
                    position: 0,
                },
            ));
        }
        if let Some(suffix) = path.strip_prefix(b"/proc/self/fd/") {
            let descriptor = std::str::from_utf8(suffix)
                .ok()
                .and_then(|value| value.parse::<u64>().ok())
                .ok_or(Errno::EPROTO)?;
            let owned = self
                .after_loader_private_state()?
                .owned_descriptors
                .get(&descriptor)
                .and_then(AfterLoaderOwnedDescriptor::reopened)
                .ok_or_else(|| self.caller_error("private proc descriptor path is not owned"))?;
            return Ok(AfterLoaderSyscallEffect::Open(owned));
        }
        let expected = dlopen_graph_image_for_path(&config.provider, &config.dependencies, &path)
            .ok_or_else(|| {
            self.caller_error("private openat path is outside the bound graph")
        })?;
        Err(self.caller_error(format!(
            "canonical bound-graph path {} is refused during private dlopen; only exact sealed inputs are admissible",
            expected.path.display(),
        )))
    }

    fn after_loader_read_effect(
        &self,
        task: &Stopped,
        number: i64,
        args: [u64; 6],
    ) -> Result<AfterLoaderSyscallEffect, Error> {
        if args[2] > MAX_PRIVATE_READ {
            return Err(self.caller_error("private read count exceeds its bound"));
        }
        let descriptor = self
            .after_loader_private_state()?
            .owned_descriptors
            .get(&args[0])
            .ok_or_else(|| self.caller_error("private read descriptor is not owned"))?;
        let (bytes, tracked_position) = descriptor
            .bytes_and_position()
            .ok_or_else(|| self.caller_error("private descriptor is not readable input"))?;
        let kernel_position =
            descriptor_position(task.pid(), args[0]).map_err(|error| self.caller_error(error))?;
        if kernel_position != tracked_position {
            return Err(
                self.caller_error("private descriptor position changed outside an admitted read")
            );
        }
        if let AfterLoaderOwnedDescriptor::ProcMaps {
            device,
            inode,
            bytes: opened,
            ..
        } = descriptor
        {
            let path = format!("/proc/{}/maps", task.pid());
            let metadata = std::fs::metadata(&path).map_err(|error| self.caller_error(error))?;
            let current =
                bounded_proc(&path, 2 * 1024 * 1024).map_err(|error| self.caller_error(error))?;
            if metadata.dev() != *device
                || metadata.ino() != *inode
                || current.as_slice() != opened.as_ref()
            {
                return Err(
                    self.caller_error("private proc-maps input changed after its exact open")
                );
            }
        }
        let offset = if number == libc::SYS_read {
            tracked_position
        } else {
            args[3]
        };
        let start = usize::try_from(offset).map_err(|_| Errno::EOVERFLOW)?;
        let count = usize::try_from(args[2]).map_err(|_| Errno::EOVERFLOW)?;
        let end = exact_read_window_end(start, bytes.len(), count).ok_or_else(|| {
            self.caller_error("private read begins beyond input or requests zero before EOF")
        })?;
        Ok(AfterLoaderSyscallEffect::Read {
            descriptor: args[0],
            destination: args[1],
            offset,
            expected: bytes[start..end].to_vec(),
            advances: number == libc::SYS_read,
        })
    }

    fn after_loader_stat_effect(
        &self,
        task: &Stopped,
        descriptor: u64,
        destination: u64,
    ) -> Result<AfterLoaderSyscallEffect, Error> {
        if !self
            .after_loader_private_state()?
            .owns_descriptor(descriptor)
        {
            return Err(self.caller_error("private stat descriptor is not owned"));
        }
        let metadata = std::fs::metadata(format!("/proc/{}/fd/{descriptor}", task.pid()))
            .map_err(|error| self.caller_error(error))?;
        if !metadata.is_file() {
            return Err(self.caller_error("private stat target is not a regular file"));
        }
        Ok(AfterLoaderSyscallEffect::Stat {
            descriptor,
            destination,
            fields: AfterLoaderStatFields::from_metadata(&metadata),
        })
    }

    fn admit_after_loader_controller_syscall(
        &self,
        task: &Stopped,
        config: &LiteinstAfterLoaderConfig,
        number: i64,
        args: [u64; 6],
    ) -> Result<AfterLoaderSyscallEffect, Error> {
        let state = self.after_loader_private_state()?;
        if state.image != self.after_loader_identity(task)? {
            return Err(self.caller_error("private resource image changed"));
        }
        let spans = syscall_output_spans(number, args)?;
        self.validate_after_loader_output_spans(&spans)?;
        match number {
            libc::SYS_mmap => self.after_loader_map_effect(task, args, None),
            libc::SYS_mprotect => {
                let protection = canonical_c_int_argument(args[2]).ok_or_else(|| {
                    self.caller_error("private mprotect protection is not a canonical C int")
                })?;
                let range = checked_page_effect_range(args[0], args[1])?;
                if state.loader_cache_scratch_overlaps(range)
                    || !state.owns_range(range, false)
                    || state.callback_stack_overlaps(range)
                    || state.refuses_protected_overlap(range)
                    || protection & !(libc::PROT_READ | libc::PROT_WRITE | libc::PROT_EXEC) != 0
                    || protection & libc::PROT_WRITE != 0 && protection & libc::PROT_EXEC != 0
                {
                    return Err(self.caller_error(
                        "private mprotect is outside an owned range or requests W+X",
                    ));
                }
                Ok(AfterLoaderSyscallEffect::Protect {
                    start: args[0],
                    raw_length: args[1],
                    protection,
                    loader_cache_alias: None,
                })
            }
            libc::SYS_munmap => {
                let range = checked_page_effect_range(args[0], args[1])?;
                let exact_scratch_release =
                    state.loader_cache_scratch_release_is_armed_for(args, range);
                if state.loader_cache_scratch_overlaps(range) && !exact_scratch_release {
                    return Err(
                        self.caller_error("private munmap overlaps armed loader-cache scratch")
                    );
                }
                if (!exact_scratch_release && !state.owns_range(range, false))
                    || state.callback_stack_overlaps(range)
                {
                    return Err(self.caller_error("private munmap is outside an owned range"));
                }
                if !state.shared_reservation_unmap_is_exact(range) {
                    return Err(
                        self.caller_error("private munmap partially overlaps a shared reservation")
                    );
                }
                if state.refuses_protected_overlap(range) {
                    return Err(
                        self.caller_error("private munmap overlaps a protected runtime range")
                    );
                }
                Ok(AfterLoaderSyscallEffect::Remove {
                    start: args[0],
                    raw_length: args[1],
                })
            }
            libc::SYS_arch_prctl
                if controller_semantic_arguments_match(libc::SYS_arch_prctl, args) =>
            {
                let mut before = [0; 8];
                task.read_exact(args[1] as usize, &mut before)?;
                Ok(AfterLoaderSyscallEffect::CetStatus {
                    destination: args[1],
                    before,
                })
            }
            libc::SYS_sigaltstack
                if controller_semantic_arguments_match(libc::SYS_sigaltstack, args) =>
            {
                Ok(AfterLoaderSyscallEffect::None)
            }
            libc::SYS_rt_sigaction
                if controller_semantic_arguments_match(libc::SYS_rt_sigaction, args) =>
            {
                Ok(AfterLoaderSyscallEffect::None)
            }
            libc::SYS_rt_sigprocmask
                if controller_semantic_arguments_match(libc::SYS_rt_sigprocmask, args) =>
            {
                Ok(AfterLoaderSyscallEffect::None)
            }
            libc::SYS_openat => self.after_loader_open_effect(task, config, args),
            libc::SYS_brk
                if controller_semantic_arguments_match(libc::SYS_brk, args)
                    && state.current_break.is_none() =>
            {
                Ok(AfterLoaderSyscallEffect::RecordBreak)
            }
            libc::SYS_fcntl
                if matches!(
                    state.owned_descriptors.get(&args[0]),
                    Some(AfterLoaderOwnedDescriptor::SealedRuntime { .. })
                ) && controller_semantic_arguments_match(libc::SYS_fcntl, args) =>
            {
                Ok(AfterLoaderSyscallEffect::ExpectedResult(
                    crate::after_loader::RUNTIME_SEALS as i64,
                ))
            }
            libc::SYS_close
                if state.owns_descriptor(args[0])
                    && controller_semantic_arguments_match(libc::SYS_close, args) =>
            {
                Ok(AfterLoaderSyscallEffect::Close(args[0]))
            }
            _ => Err(self.caller_error(format!(
                "controller syscall is not admitted: nr={number} args={args:?}"
            ))),
        }
    }

    fn after_loader_map_effect(
        &self,
        task: &Stopped,
        args: [u64; 6],
        function: Option<CallerFunction>,
    ) -> Result<AfterLoaderSyscallEffect, Error> {
        let length = args[1];
        if checked_private_mmap_effect_length(length).is_err()
            || !private_mmap_offset_is_admissible(args[5])
        {
            return Err(self.caller_error("private mmap length or offset is outside bounds"));
        }
        let protection = canonical_c_int_argument(args[2])
            .ok_or_else(|| self.caller_error("private mmap protection is not a canonical C int"))?;
        let flags = canonical_c_int_argument(args[3])
            .ok_or_else(|| self.caller_error("private mmap flags are not a canonical C int"))?;
        if protection & !(libc::PROT_READ | libc::PROT_WRITE | libc::PROT_EXEC) != 0
            || protection & libc::PROT_WRITE != 0 && protection & libc::PROT_EXEC != 0
        {
            return Err(self.caller_error("private mmap requests unsupported protection or W+X"));
        }
        let state = self.after_loader_private_state()?;
        if args[0] != 0
            && state.loader_cache_scratch_overlaps(checked_page_effect_range(args[0], length)?)
        {
            return Err(self.caller_error("private mmap overlaps armed loader-cache scratch"));
        }
        let anonymous_descriptor = canonical_anonymous_mmap_descriptor(args[4]);
        let (descriptor, purpose) = match function {
            None => {
                let expected = libc::MAP_PRIVATE | libc::MAP_ANONYMOUS;
                if args[0] != 0 || !anonymous_descriptor || args[5] != 0 || flags != expected {
                    return Err(self.caller_error(
                        "controller mmap is not an exact new private anonymous mapping",
                    ));
                }
                (None, AfterLoaderMappingPurpose::Controller)
            }
            Some(CallerFunction::Dlopen) if !anonymous_descriptor => {
                let descriptor = state
                    .owned_descriptors
                    .get(&args[4])
                    .ok_or_else(|| self.caller_error("private file mmap descriptor disappeared"))?;
                let image = match descriptor {
                    AfterLoaderOwnedDescriptor::SealedRuntime { .. } => state.image_id(
                        &self
                            .after_loader_config()
                            .ok_or(Errno::EPROTO)?
                            .sealed_runtime
                            .image,
                    ),
                    AfterLoaderOwnedDescriptor::BoundImage { image, .. } => *image,
                    AfterLoaderOwnedDescriptor::LoaderCacheAlias { image, .. } => *image,
                    _ => {
                        return Err(self.caller_error(
                            "private loader mmap descriptor is outside the bound image graph",
                        ));
                    }
                };
                let base_flags = libc::MAP_PRIVATE | libc::MAP_DENYWRITE;
                if flags != base_flags && flags != base_flags | libc::MAP_FIXED {
                    return Err(self.caller_error("private loader file mmap flags are not exact"));
                }
                if flags & libc::MAP_FIXED != 0 {
                    let range = checked_page_effect_range(args[0], length)?;
                    if args[0] == 0
                        || args[0] % PAGE != 0
                        || !state.owns_same_image_range(range, image)
                        || state.refuses_original_overlap(range)
                        || state.refuses_protected_overlap(range)
                    {
                        return Err(self.caller_error(
                            "private loader fixed mmap is outside its owned image reservation",
                        ));
                    }
                } else if args[0] != 0 {
                    return Err(
                        self.caller_error("first private loader file mmap carries an address hint")
                    );
                }
                (Some(args[4]), AfterLoaderMappingPurpose::Image { image })
            }
            Some(CallerFunction::Dlopen) => {
                let range = checked_page_effect_range(args[0], length)?;
                let expected_flags = libc::MAP_PRIVATE | libc::MAP_ANONYMOUS | libc::MAP_FIXED;
                let owner = state.owned_mappings.iter().find_map(|mapping| {
                    (mapping.start <= range.0
                        && range.1 <= mapping.end
                        && matches!(mapping.purpose, AfterLoaderMappingPurpose::Image { .. }))
                    .then_some(mapping.purpose)
                });
                let Some(AfterLoaderMappingPurpose::Image { image }) = owner else {
                    return Err(self.caller_error(
                        "private loader zero-fill mmap has no exact image reservation",
                    ));
                };
                if args[0] == 0
                    || args[0] % PAGE != 0
                    || args[5] != 0
                    || flags != expected_flags
                    || state.refuses_original_overlap(range)
                    || state.refuses_protected_overlap(range)
                {
                    return Err(self.caller_error("private loader zero-fill mmap geometry differs"));
                }
                (None, AfterLoaderMappingPurpose::ImageZeroFill { image })
            }
            Some(CallerFunction::Initializer) if !anonymous_descriptor => {
                let descriptor = state
                    .owned_descriptors
                    .get(&args[4])
                    .ok_or_else(|| self.caller_error("trampoline mmap descriptor disappeared"))?;
                let trampoline = match descriptor {
                    AfterLoaderOwnedDescriptor::Trampoline {
                        id: Some(trampoline),
                        size: Some(size),
                    } if *size == TRAMPOLINE_ARENA_SIZE => *trampoline,
                    _ => {
                        return Err(self.caller_error(
                            "trampoline mmap descriptor is not exactly sized and bound",
                        ));
                    }
                };
                let writable = protection == (libc::PROT_READ | libc::PROT_WRITE)
                    && args[0] == 0
                    && flags == libc::MAP_SHARED;
                let executable = protection == (libc::PROT_READ | libc::PROT_EXEC)
                    && args[0] != 0
                    && args[0] % PAGE == 0
                    && flags == libc::MAP_SHARED | libc::MAP_FIXED_NOREPLACE;
                let aliases = state
                    .owned_mappings
                    .iter()
                    .filter(|mapping| {
                        matches!(
                            mapping.purpose,
                            AfterLoaderMappingPurpose::Trampoline {
                                trampoline: mapped,
                            } if mapped == trampoline
                        )
                    })
                    .collect::<Vec<_>>();
                let writable_order = aliases.is_empty();
                let executable_order = aliases.len() == 1
                    && aliases[0].writable
                    && !aliases[0].executable
                    && aliases[0].shared
                    && aliases[0].offset == 0
                    && aliases[0].end - aliases[0].start == TRAMPOLINE_ARENA_SIZE;
                let candidate = checked_page_effect_range(args[0], length).ok();
                let candidate_is_free = candidate.is_some_and(|candidate| {
                    !state
                        .owned_mappings
                        .iter()
                        .any(|mapping| ranges_overlap((mapping.start, mapping.end), candidate))
                });
                let is_near_executable = executable
                    && guest_maps(task.pid()).is_some_and(|maps| {
                        maps.iter().any(|mapping| {
                            mapping.executable
                                && !mapping.writable
                                && i32::try_from(args[0] as i128 - mapping.start as i128).is_ok()
                        })
                    });
                if !exact_trampoline_mmap_length(length)
                    || args[5] != 0
                    || (!writable && !executable)
                    || writable && !writable_order
                    || executable
                        && (!executable_order || !candidate_is_free || !is_near_executable)
                    || executable
                        && state
                            .refuses_original_overlap(checked_page_effect_range(args[0], length)?)
                    || executable
                        && state
                            .refuses_protected_overlap(checked_page_effect_range(args[0], length)?)
                {
                    return Err(
                        self.caller_error("trampoline mmap is not one exact RW or RX shared alias")
                    );
                }
                (
                    Some(args[4]),
                    AfterLoaderMappingPurpose::Trampoline { trampoline },
                )
            }
            Some(CallerFunction::Initializer) => {
                let callback_stack = exact_callback_stack_mmap_shape(args, protection, flags)
                    && !state
                        .owned_mappings
                        .iter()
                        .any(|mapping| mapping.purpose == AfterLoaderMappingPurpose::CallbackStack);
                if callback_stack {
                    (None, AfterLoaderMappingPurpose::CallbackStack)
                } else {
                    let expected_flags = libc::MAP_SHARED | libc::MAP_ANONYMOUS;
                    if args[0] != 0
                        || args[5] != 0
                        || !exact_shared_reservation_mmap_length(length)
                        || protection != (libc::PROT_READ | libc::PROT_WRITE)
                        || flags != expected_flags
                    {
                        return Err(self.caller_error(
                            "anonymous initializer mmap is neither the exact callback stack nor one exact shared reservation",
                        ));
                    }
                    let trampoline =
                        state
                            .trampoline_awaiting_shared_reservation()
                            .ok_or_else(|| {
                                self.caller_error(
                                "shared reservation has no unique fully aliased trampoline owner",
                            )
                            })?;
                    (
                        None,
                        AfterLoaderMappingPurpose::SharedReservation { trampoline },
                    )
                }
            }
            Some(CallerFunction::ErrnoLocation) => {
                return Err(self.caller_error("errno accessor attempted a private mmap"));
            }
        };
        Ok(AfterLoaderSyscallEffect::Map {
            requested: args[0],
            raw_length: length,
            protection,
            flags,
            descriptor,
            offset: args[5],
            purpose,
        })
    }

    fn admit_after_loader_function_syscall(
        &self,
        task: &Stopped,
        config: &LiteinstAfterLoaderConfig,
        function: CallerFunction,
        permit: &AfterLoaderSyscallPermit,
    ) -> Result<AfterLoaderSyscallEffect, Error> {
        if function == CallerFunction::ErrnoLocation {
            return Err(self.caller_error(format!(
                "private function syscall {} is refused",
                permit.number
            )));
        }
        let state = self.after_loader_private_state()?;
        if permit.image != Some(state.image) {
            return Err(self.caller_error("private function image changed"));
        }
        if let Some(cache) = state.loader_cache.as_ref() {
            let exact_next_step = match cache.lifecycle {
                LoaderCacheLifecycle::Open { descriptor } => {
                    permit.number == libc::SYS_fstat && permit.args[0] == descriptor
                }
                LoaderCacheLifecycle::Statted { descriptor } => {
                    permit.number == libc::SYS_mmap && permit.args[4] == descriptor
                }
                LoaderCacheLifecycle::MappedOpen { descriptor, .. } => {
                    permit.number == libc::SYS_close && permit.args[0] == descriptor
                }
                LoaderCacheLifecycle::AwaitingOpen
                | LoaderCacheLifecycle::MappedClosed { .. }
                | LoaderCacheLifecycle::Retired { .. } => true,
                LoaderCacheLifecycle::ScratchReleaseArmed { .. }
                | LoaderCacheLifecycle::Released { .. } => false,
            };
            if !exact_next_step {
                return Err(self.caller_error(
                    "private syscall interleaved with the exact loader-cache fstat/mmap/close sequence",
                ));
            }
            let mut exact_alias_step = true;
            for (raw_path, lifecycle) in &cache.aliases {
                let alias = config
                    .loader_cache_alias_for_path(raw_path.as_os_str().as_bytes())
                    .ok_or_else(|| {
                        self.caller_error("loader-cache alias plan left its bound profile")
                    })?;
                let plan = loader_cache_alias_mmap_plan(&alias.dependency().bytes)
                    .map_err(|reason| self.caller_error(reason))?;
                exact_alias_step &= loader_cache_alias_next_step_is_exact(
                    &plan,
                    lifecycle,
                    permit.number,
                    permit.args,
                );
            }
            if !exact_alias_step {
                return Err(self.caller_error(
                    "private syscall interleaved with the exact loader-cache alias sequence",
                ));
            }
        }
        if permit.number == libc::SYS_getrandom {
            if function != CallerFunction::Dlopen {
                return Err(self.caller_error(
                    "private bootstrap entropy request has a different phase or ABI shape",
                ));
            }
            let profile = state.ptmalloc_bootstrap.ok_or_else(|| {
                self.caller_error("private bootstrap entropy has no exact provider profile")
            })?;
            let destination = profile
                .load_bias
                .checked_add(PTMALLOC_BOOTSTRAP_TCACHE_KEY_RVA)
                .ok_or(Errno::EOVERFLOW)?;
            let syscall = profile
                .load_bias
                .checked_add(PTMALLOC_BOOTSTRAP_SYSCALL_RVA)
                .ok_or(Errno::EOVERFLOW)?;
            if !exact_ptmalloc_bootstrap_request(profile.load_bias, task.getregs()?.rax, permit)
                || permit.instruction_pointer != syscall
                || permit.args[0] != destination
            {
                return Err(self.caller_error(
                    "private bootstrap entropy request differs from the one-shot profile",
                ));
            }
            let Some((load_bias, current)) = self.observe_ptmalloc_bootstrap(task, config)? else {
                return Err(
                    self.caller_error("private bootstrap entropy provider profile disappeared")
                );
            };
            if !exact_ptmalloc_bootstrap_admission_state(profile, load_bias, current) {
                return Err(self
                    .caller_error("private bootstrap entropy target state differs at admission"));
            }
            return Ok(AfterLoaderSyscallEffect::PtmallocBootstrapEntropy {
                destination,
                before: current.tcache_key,
            });
        }
        if !private_syscall_allowed(permit.number) {
            return Err(self.caller_error(format!(
                "private function syscall {} is refused",
                permit.number
            )));
        }
        self.validate_after_loader_output_spans(&permit.output_spans)?;
        let args = permit.args;
        if permit.number == libc::SYS_mprotect {
            let pending = state
                .loader_cache
                .as_ref()
                .into_iter()
                .flat_map(|cache| cache.aliases.iter())
                .filter_map(|(raw_path, lifecycle)| match lifecycle {
                    LoaderCacheAliasLifecycle::MappedClosed {
                        image, geometry, ..
                    } => Some((raw_path.clone(), *image, *geometry)),
                    _ => None,
                })
                .collect::<Vec<_>>();
            if let [(raw_path, image, geometry)] = pending.as_slice() {
                let alias = config
                    .loader_cache_alias_for_path(raw_path.as_os_str().as_bytes())
                    .ok_or_else(|| {
                        self.caller_error("pending loader-cache alias left its bound profile")
                    })?;
                let renewed = self.resolve_loader_cache_alias_pre_relro_geometry(
                    task,
                    alias.dependency(),
                    *image,
                )?;
                let relro = exact_image_relro_range(alias.dependency(), renewed)?;
                let protection = canonical_c_int_argument(args[2]).ok_or_else(|| {
                    self.caller_error(
                        "loader-cache alias RELRO protection is not a canonical C int",
                    )
                })?;
                self.authenticate_loader_cache_interpreter_site(
                    task,
                    config,
                    permit,
                    LOADER_CACHE_ALIAS_RELRO_MPROTECT_SYSCALL_RVA,
                )?;
                self.authenticate_after_loader_image_backing(task, alias.dependency(), renewed)?;
                let maps = guest_maps(task.pid()).ok_or(Errno::EPROTO)?;
                if renewed != *geometry
                    || args[0] != relro.0
                    || args[1] != relro.1 - relro.0
                    || protection != libc::PROT_READ
                    || state.loader_cache_scratch_overlaps(relro)
                    || !state.owns_range(relro, false)
                    || state.callback_stack_overlaps(relro)
                    || state.refuses_protected_overlap(relro)
                    || !target_loader_cache_alias_matches(task.pid(), alias)
                        .map_err(|error| self.caller_error(error))?
                    || !exact_image_mapping_paths_are_literal(alias.dependency(), renewed, &maps)
                {
                    return Err(self.caller_error(
                        "loader-cache alias RELRO transition differs from its exact profiled state",
                    ));
                }
                return Ok(AfterLoaderSyscallEffect::Protect {
                    start: args[0],
                    raw_length: args[1],
                    protection,
                    loader_cache_alias: Some((raw_path.clone(), *image, *geometry)),
                });
            }
            if !pending.is_empty() {
                return Err(self.caller_error("loader-cache alias RELRO authority is not unique"));
            }
        }
        match permit.number {
            libc::SYS_openat => {
                let path = self.caller_cstring(task, args[1], 4096)?;
                let effect =
                    if function == CallerFunction::Initializer && path == PROC_SELF_FD_DIRECTORY {
                        self.after_loader_proc_fd_directory_open_effect(task, config, permit)?
                    } else if function == CallerFunction::Dlopen && path == LOADER_CACHE_PATH {
                        self.after_loader_cache_open_effect(task, config, permit)?
                    } else {
                        if function == CallerFunction::Dlopen
                            && config.loader_cache_alias_for_path(&path).is_some()
                        {
                            self.authenticate_loader_cache_interpreter_site(
                                task,
                                config,
                                permit,
                                LOADER_CACHE_OPENAT_SYSCALL_RVA,
                            )?;
                        }
                        self.after_loader_open_effect(task, config, args)?
                    };
                let admitted = matches!(
                    (&effect, function),
                    (
                        AfterLoaderSyscallEffect::Open(
                            AfterLoaderOwnedDescriptor::SealedRuntime { .. }
                        ),
                        CallerFunction::Dlopen
                    ) | (
                        AfterLoaderSyscallEffect::OpenLoaderCacheAlias(
                            AfterLoaderOwnedDescriptor::LoaderCacheAlias { .. }
                        ),
                        CallerFunction::Dlopen
                    ) | (
                        AfterLoaderSyscallEffect::OpenLoaderCache { .. },
                        CallerFunction::Dlopen
                    ) | (
                        AfterLoaderSyscallEffect::Open(AfterLoaderOwnedDescriptor::ProcMaps { .. }),
                        CallerFunction::Initializer
                    ) | (
                        AfterLoaderSyscallEffect::OpenProcFdDirectory { .. },
                        CallerFunction::Initializer
                    )
                );
                if !admitted {
                    return Err(
                        self.caller_error("private openat is outside its exact function phase")
                    );
                }
                Ok(effect)
            }
            libc::SYS_getdents64 if function == CallerFunction::Initializer => {
                self.after_loader_proc_fd_getdents_effect(task, config, permit)
            }
            libc::SYS_readlinkat if function == CallerFunction::Initializer => {
                self.after_loader_proc_fd_readlink_effect(task, config, permit)
            }
            libc::SYS_fstat
                if function == CallerFunction::Dlopen
                    && matches!(
                        state.owned_descriptors.get(&args[0]),
                        Some(AfterLoaderOwnedDescriptor::LoaderCache { .. })
                    ) =>
            {
                self.after_loader_cache_stat_effect(task, config, permit)
            }
            libc::SYS_mmap
                if function == CallerFunction::Dlopen
                    && matches!(
                        state.owned_descriptors.get(&args[4]),
                        Some(AfterLoaderOwnedDescriptor::LoaderCache { .. })
                    ) =>
            {
                self.after_loader_cache_map_effect(task, config, permit)
            }
            libc::SYS_close
                if function == CallerFunction::Dlopen
                    && matches!(
                        state.owned_descriptors.get(&args[0]),
                        Some(AfterLoaderOwnedDescriptor::LoaderCache { .. })
                    ) =>
            {
                self.after_loader_cache_close_effect(task, config, permit)
            }
            libc::SYS_read
                if matches!(
                    state.owned_descriptors.get(&args[0]),
                    Some(AfterLoaderOwnedDescriptor::LoaderCacheAlias { .. })
                ) =>
            {
                if function != CallerFunction::Dlopen || args[2] != LOADER_CACHE_ALIAS_HEADER_BYTES
                {
                    return Err(self.caller_error(
                        "loader-cache alias header read has a different function or exact length",
                    ));
                }
                self.authenticate_loader_cache_interpreter_site(
                    task,
                    config,
                    permit,
                    LOADER_CACHE_ALIAS_READ_SYSCALL_RVA,
                )?;
                let (raw_path, image) =
                    self.authenticate_loader_cache_alias_descriptor(task, config, args[0])?;
                if !matches!(
                    self.after_loader_private_state()?
                        .loader_cache
                        .as_ref()
                        .and_then(|cache| cache.aliases.get(&raw_path)),
                    Some(LoaderCacheAliasLifecycle::Open {
                        descriptor,
                        image: expected_image,
                        read_end: 0,
                    }) if *descriptor == args[0] && *expected_image == image
                ) {
                    return Err(self.caller_error(
                        "loader-cache alias header read is duplicated or out of order",
                    ));
                }
                self.after_loader_read_effect(task, permit.number, args)
            }
            libc::SYS_pread64
                if matches!(
                    state.owned_descriptors.get(&args[0]),
                    Some(AfterLoaderOwnedDescriptor::LoaderCacheAlias { .. })
                ) =>
            {
                Err(self
                    .caller_error("profiled loader-cache alias does not admit a pread64 sequence"))
            }
            libc::SYS_fstat
                if matches!(
                    state.owned_descriptors.get(&args[0]),
                    Some(AfterLoaderOwnedDescriptor::LoaderCacheAlias { .. })
                ) =>
            {
                if function != CallerFunction::Dlopen {
                    return Err(
                        self.caller_error("loader-cache alias fstat is outside private dlopen")
                    );
                }
                self.authenticate_loader_cache_interpreter_site(
                    task,
                    config,
                    permit,
                    LOADER_CACHE_FSTAT_SYSCALL_RVA,
                )?;
                let (raw_path, image) =
                    self.authenticate_loader_cache_alias_descriptor(task, config, args[0])?;
                if !matches!(
                    self.after_loader_private_state()?
                        .loader_cache
                        .as_ref()
                        .and_then(|cache| cache.aliases.get(&raw_path)),
                    Some(LoaderCacheAliasLifecycle::Open {
                        descriptor,
                        image: expected_image,
                        read_end: LOADER_CACHE_ALIAS_HEADER_BYTES,
                    }) if *descriptor == args[0] && *expected_image == image
                ) {
                    return Err(self
                        .caller_error("loader-cache alias fstat preceded its exact header read"));
                }
                let metadata = std::fs::metadata(format!("/proc/{}/fd/{}", task.pid(), args[0]))
                    .map_err(|error| self.caller_error(error))?;
                if !metadata.is_file() {
                    return Err(self.caller_error("loader-cache alias fstat target is not a file"));
                }
                Ok(AfterLoaderSyscallEffect::StatLoaderCacheAlias {
                    descriptor: args[0],
                    destination: args[1],
                    fields: AfterLoaderStatFields::deterministic_loader_cache_alias(&metadata),
                })
            }
            libc::SYS_mmap
                if matches!(
                    state.owned_descriptors.get(&args[4]),
                    Some(AfterLoaderOwnedDescriptor::LoaderCacheAlias { .. })
                ) =>
            {
                if function != CallerFunction::Dlopen {
                    return Err(
                        self.caller_error("loader-cache alias mmap is outside private dlopen")
                    );
                }
                self.authenticate_loader_cache_interpreter_site(
                    task,
                    config,
                    permit,
                    LOADER_CACHE_MMAP_SYSCALL_RVA,
                )?;
                let (raw_path, image) =
                    self.authenticate_loader_cache_alias_descriptor(task, config, args[4])?;
                let mapping_ready = matches!(
                    self.after_loader_private_state()?
                        .loader_cache
                        .as_ref()
                        .and_then(|cache| cache.aliases.get(&raw_path)),
                    Some(LoaderCacheAliasLifecycle::Statted {
                        descriptor,
                        image: expected_image,
                    } | LoaderCacheAliasLifecycle::Mapped {
                        descriptor,
                        image: expected_image,
                        ..
                    }) if *descriptor == args[4] && *expected_image == image
                );
                if !mapping_ready {
                    return Err(self.caller_error(
                        "loader-cache alias mmap preceded exact fstat or changed identity",
                    ));
                }
                self.after_loader_map_effect(task, args, Some(function))
            }
            libc::SYS_close
                if matches!(
                    state.owned_descriptors.get(&args[0]),
                    Some(AfterLoaderOwnedDescriptor::LoaderCacheAlias { .. })
                ) =>
            {
                if function != CallerFunction::Dlopen {
                    return Err(
                        self.caller_error("loader-cache alias close is outside private dlopen")
                    );
                }
                self.authenticate_loader_cache_interpreter_site(
                    task,
                    config,
                    permit,
                    LOADER_CACHE_CLOSE_SYSCALL_RVA,
                )?;
                let (raw_path, image) =
                    self.authenticate_loader_cache_alias_descriptor(task, config, args[0])?;
                let alias = config
                    .loader_cache_alias_for_path(raw_path.as_os_str().as_bytes())
                    .ok_or_else(|| {
                        self.caller_error("loader-cache alias close left its bound profile")
                    })?;
                let plan = loader_cache_alias_mmap_plan(&alias.dependency().bytes)
                    .map_err(|reason| self.caller_error(reason))?;
                if !matches!(
                    self.after_loader_private_state()?
                        .loader_cache
                        .as_ref()
                        .and_then(|cache| cache.aliases.get(&raw_path)),
                    Some(LoaderCacheAliasLifecycle::Mapped {
                        descriptor,
                        image: expected_image,
                        mappings,
                        zero_fills,
                    }) if *descriptor == args[0]
                        && *expected_image == image
                        && mappings.len() == plan.file_maps.len()
                        && zero_fills.len() == 1
                ) {
                    return Err(self.caller_error(
                        "loader-cache alias close preceded its causal file mapping",
                    ));
                }
                Ok(AfterLoaderSyscallEffect::Close(args[0]))
            }
            libc::SYS_close
                if function == CallerFunction::Initializer
                    && matches!(
                        state.owned_descriptors.get(&args[0]),
                        Some(AfterLoaderOwnedDescriptor::ProcFdDirectory { .. })
                    ) =>
            {
                self.authenticate_proc_fd_audit_call(task, config, permit, &[])?;
                let Some(directory) = state.owned_descriptors.get(&args[0]) else {
                    return Err(Errno::EPROTO.into());
                };
                let AfterLoaderOwnedDescriptor::ProcFdDirectory { expected, .. } = directory else {
                    return Err(Errno::EPROTO.into());
                };
                if args[1..] != [0, 0, 0, 0, 0]
                    || state.proc_fd_audit
                        != (ProcFdAuditLifecycle::Scanning {
                            descriptor: args[0],
                        })
                    || !directory.proc_fd_scan_is_complete()
                    || proc_fd_link_snapshot(task.pid())
                        .map_err(|error| self.caller_error(error))?
                        != *expected
                {
                    return Err(self.caller_error(
                        "proc-fd audit close preceded exact EOF, links or live snapshot",
                    ));
                }
                let mut remaining = expected.clone();
                if remaining.remove(&args[0]).is_none() {
                    return Err(Errno::EPROTO.into());
                }
                Ok(AfterLoaderSyscallEffect::CloseProcFdDirectory {
                    descriptor: args[0],
                    remaining,
                })
            }
            libc::SYS_close
                if state.owns_descriptor(args[0])
                    && state
                        .owned_descriptors
                        .get(&args[0])
                        .is_some_and(|descriptor| {
                            !matches!(
                                descriptor,
                                AfterLoaderOwnedDescriptor::ProcFdDirectory { .. }
                            )
                        }) =>
            {
                Ok(AfterLoaderSyscallEffect::Close(args[0]))
            }
            libc::SYS_read | libc::SYS_pread64
                if state.owns_descriptor(args[0])
                    && match (state.owned_descriptors.get(&args[0]), function) {
                        (
                            Some(AfterLoaderOwnedDescriptor::ProcMaps { .. }),
                            CallerFunction::Initializer,
                        ) => true,
                        (
                            Some(
                                AfterLoaderOwnedDescriptor::SealedRuntime { .. }
                                | AfterLoaderOwnedDescriptor::BoundImage { .. },
                            ),
                            CallerFunction::Dlopen,
                        ) => true,
                        _ => false,
                    } =>
            {
                self.after_loader_read_effect(task, permit.number, args)
            }
            libc::SYS_fstat
                if state.owns_descriptor(args[0]) && function == CallerFunction::Dlopen =>
            {
                self.after_loader_stat_effect(task, args[0], args[1])
            }
            libc::SYS_newfstatat
                if state.owns_descriptor(args[0])
                    && function == CallerFunction::Dlopen
                    && !matches!(
                        state.owned_descriptors.get(&args[0]),
                        Some(
                            AfterLoaderOwnedDescriptor::LoaderCache { .. }
                                | AfterLoaderOwnedDescriptor::LoaderCacheAlias { .. }
                        )
                    )
                    && self.caller_cstring(task, args[1], 1)?.is_empty()
                    && canonical_c_int_argument_is(args[3], libc::AT_EMPTY_PATH) =>
            {
                self.after_loader_stat_effect(task, args[0], args[2])
            }
            libc::SYS_statx
                if function == CallerFunction::Initializer
                    && matches!(
                        state.owned_descriptors.get(&args[0]),
                        Some(AfterLoaderOwnedDescriptor::ProcMaps { .. })
                    )
                    && self.caller_cstring(task, args[1], 1)?.is_empty()
                    && exact_owned_descriptor_statx_arguments(&args) =>
            {
                Ok(AfterLoaderSyscallEffect::Statx {
                    descriptor: args[0],
                    destination: args[4],
                    expected: descriptor_statx_bytes(task.pid(), args[0])
                        .map_err(|error| self.caller_error(error))?,
                })
            }
            libc::SYS_mmap => {
                let effect = self.after_loader_map_effect(task, args, Some(function))?;
                let active_aliases = state
                    .loader_cache
                    .as_ref()
                    .into_iter()
                    .flat_map(|cache| cache.aliases.values())
                    .filter_map(|lifecycle| match lifecycle {
                        LoaderCacheAliasLifecycle::Mapped { image, .. } => Some(*image),
                        _ => None,
                    })
                    .collect::<Vec<_>>();
                if let [image] = active_aliases.as_slice()
                    && !matches!(
                        effect,
                        AfterLoaderSyscallEffect::Map {
                            purpose: AfterLoaderMappingPurpose::ImageZeroFill {
                                image: mapped,
                            },
                            ..
                        } if mapped == *image
                    )
                {
                    return Err(self.caller_error(
                        "loader-cache alias sequence admitted a non-causal anonymous mmap",
                    ));
                }
                if active_aliases.len() > 1 {
                    return Err(
                        self.caller_error("loader-cache alias mmap authority is not unique")
                    );
                }
                Ok(effect)
            }
            libc::SYS_mprotect => {
                let protection = canonical_c_int_argument(args[2]).ok_or_else(|| {
                    self.caller_error(
                        "private function mprotect protection is not a canonical C int",
                    )
                })?;
                let range = checked_page_effect_range(args[0], args[1])?;
                let exact_callback_transition = function == CallerFunction::Initializer
                    && state.callback_stack_protect_transition_is_exact(range, args[1], protection);
                if state.loader_cache_scratch_overlaps(range) {
                    return Err(
                        self.caller_error("private mprotect overlaps armed loader-cache scratch")
                    );
                }
                if state.owned_mappings.iter().any(|mapping| {
                    mapping.purpose == AfterLoaderMappingPurpose::LoaderCache
                        && ranges_overlap((mapping.start, mapping.end), range)
                }) {
                    return Err(self.caller_error(
                        "private mprotect overlaps the immutable loader-cache mapping",
                    ));
                }
                if !exact_callback_transition
                    && (!state.owns_range(range, false)
                        || state.callback_stack_overlaps(range)
                        || state.refuses_protected_overlap(range)
                        || protection & !(libc::PROT_READ | libc::PROT_WRITE | libc::PROT_EXEC)
                            != 0
                        || protection & libc::PROT_WRITE != 0 && protection & libc::PROT_EXEC != 0)
                {
                    return Err(self.caller_error("private function mprotect is unowned or W+X"));
                }
                Ok(AfterLoaderSyscallEffect::Protect {
                    start: args[0],
                    raw_length: args[1],
                    protection,
                    loader_cache_alias: None,
                })
            }
            libc::SYS_munmap => {
                let range = checked_page_effect_range(args[0], args[1])?;
                if state.loader_cache_scratch_overlaps(range) {
                    return Err(
                        self.caller_error("private munmap overlaps armed loader-cache scratch")
                    );
                }
                if let Some(effect) = self.after_loader_cache_retire_effect(task, config, permit)? {
                    return Ok(effect);
                }
                if state.owned_mappings.iter().any(|mapping| {
                    mapping.purpose == AfterLoaderMappingPurpose::LoaderCache
                        && ranges_overlap((mapping.start, mapping.end), range)
                }) {
                    return Err(self.caller_error(
                        "private munmap partially or prematurely overlaps the loader cache",
                    ));
                }
                if !state.owns_range(range, false) || state.callback_stack_overlaps(range) {
                    return Err(self.caller_error("private function munmap is unowned"));
                }
                if !state.shared_reservation_unmap_is_exact(range) {
                    return Err(self.caller_error(
                        "private function munmap partially overlaps a shared reservation",
                    ));
                }
                if state.refuses_protected_overlap(range) {
                    return Err(self.caller_error(
                        "private function munmap overlaps a protected runtime range",
                    ));
                }
                Ok(AfterLoaderSyscallEffect::Remove {
                    start: args[0],
                    raw_length: args[1],
                })
            }
            libc::SYS_brk if args[0] == 0 => {
                let expected = state.current_break.ok_or_else(|| {
                    self.caller_error("private brk query has no preobserved value")
                })?;
                Ok(AfterLoaderSyscallEffect::CheckBreak(expected))
            }
            libc::SYS_brk
                if function == CallerFunction::Dlopen && !state.private_brk_growth_consumed =>
            {
                let profile = state.ptmalloc_bootstrap.ok_or_else(|| {
                    self.caller_error("private brk growth has no exact ptmalloc profile")
                })?;
                let Some((observed_bias, observed)) =
                    self.observe_ptmalloc_bootstrap(task, config)?
                else {
                    return Err(
                        self.caller_error("private brk growth provider profile disappeared")
                    );
                };
                if !exact_ptmalloc_brk_growth_request(profile, observed_bias, observed, permit) {
                    return Err(self.caller_error(
                        "private brk growth is outside the consumed exact ptmalloc syscall site",
                    ));
                }
                let previous = state.current_break.ok_or_else(|| {
                    self.caller_error("private brk growth preceded the initial bound query")
                })?;
                let growth = private_brk_growth_range(previous, args[0]).map_err(|_| {
                    self.caller_error("private brk growth is not monotonic or exceeds its bound")
                })?;
                let before_maps = guest_maps(task.pid()).ok_or(Errno::EPROTO)?;
                if growth.is_some_and(|range| {
                    state.refuses_protected_overlap(range)
                        || state
                            .owned_mappings
                            .iter()
                            .any(|mapping| ranges_overlap((mapping.start, mapping.end), range))
                        || before_maps
                            .iter()
                            .any(|mapping| ranges_overlap((mapping.start, mapping.end), range))
                }) {
                    return Err(self.caller_error(
                        "private brk growth overlaps an existing or protected mapping",
                    ));
                }
                Ok(AfterLoaderSyscallEffect::AdvanceBreak {
                    previous,
                    requested: args[0],
                    before_maps,
                })
            }
            libc::SYS_futex if function == CallerFunction::Initializer => {
                let range = checked_range(args[0], 4)?;
                if args[0] % 4 != 0
                    || !state.runtime_futex_mapping(range, &config.sealed_runtime.image)
                    || !initializer_futex_arguments_match(args)
                {
                    return Err(self.caller_error(
                        "private futex is not the exact runtime FUTEX_WAKE_PRIVATE",
                    ));
                }
                let mut word = [0_u8; 4];
                task.read_exact(args[0] as usize, &mut word)?;
                Ok(AfterLoaderSyscallEffect::FutexWake {
                    address: args[0],
                    word,
                })
            }
            libc::SYS_memfd_create if function == CallerFunction::Initializer => {
                if self.caller_cstring(task, args[0], 64)? != b"liteinst2-trampoline"
                    || args[1] != (libc::MFD_CLOEXEC as u64 | MFD_ALLOW_SEALING)
                {
                    return Err(
                        self.caller_error("private memfd is not the bound LiteInst trampoline")
                    );
                }
                Ok(AfterLoaderSyscallEffect::Open(
                    AfterLoaderOwnedDescriptor::Trampoline {
                        id: None,
                        size: None,
                    },
                ))
            }
            libc::SYS_ftruncate
                if function == CallerFunction::Initializer
                    && matches!(
                        state.owned_descriptors.get(&args[0]),
                        Some(AfterLoaderOwnedDescriptor::Trampoline {
                            id: Some(_),
                            size: None,
                        })
                    )
                    && args[1] == TRAMPOLINE_ARENA_SIZE =>
            {
                Ok(AfterLoaderSyscallEffect::Resize {
                    descriptor: args[0],
                    length: args[1],
                })
            }
            libc::SYS_fcntl
                if function == CallerFunction::Initializer
                    && initializer_fcntl_add_seals_arguments_match(args) =>
            {
                let (trampoline, size) = match state.owned_descriptors.get(&args[0]) {
                    Some(AfterLoaderOwnedDescriptor::Trampoline {
                        id: Some(trampoline),
                        size: Some(size),
                    }) => (*trampoline, *size),
                    _ => {
                        return Err(self
                            .caller_error("trampoline seals target is not a complete descriptor"));
                    }
                };
                if size != TRAMPOLINE_ARENA_SIZE
                    || state.trampoline_seals_added.contains(&args[0])
                    || state.sealed_trampolines.contains(&trampoline)
                    || state.trampoline_close_shape(args[0], trampoline, size)
                        != Some(TrampolineCloseShape::Complete)
                {
                    return Err(self.caller_error(
                        "trampoline seals were added out of order or to an incomplete arena",
                    ));
                }
                Ok(AfterLoaderSyscallEffect::AddTrampolineSeals {
                    descriptor: args[0],
                })
            }
            libc::SYS_fcntl
                if function == CallerFunction::Initializer
                    && controller_semantic_arguments_match(libc::SYS_fcntl, args) =>
            {
                let trampoline = match state.owned_descriptors.get(&args[0]) {
                    Some(AfterLoaderOwnedDescriptor::Trampoline {
                        id: Some(trampoline),
                        size: Some(TRAMPOLINE_ARENA_SIZE),
                    }) => *trampoline,
                    _ => {
                        return Err(
                            self.caller_error("trampoline seal query lost its complete descriptor")
                        );
                    }
                };
                if !state.trampoline_seals_added.contains(&args[0])
                    || state.sealed_trampolines.contains(&trampoline)
                {
                    return Err(self
                        .caller_error("trampoline seals were queried before one exact addition"));
                }
                Ok(AfterLoaderSyscallEffect::VerifyTrampolineSeals {
                    descriptor: args[0],
                    trampoline,
                })
            }
            libc::SYS_getpid | libc::SYS_gettid if function == CallerFunction::Initializer => {
                Ok(AfterLoaderSyscallEffect::Identity(task.pid()))
            }
            libc::SYS_fcntl
                if state.owns_descriptor(args[0])
                    && function == CallerFunction::Dlopen
                    && !matches!(
                        state.owned_descriptors.get(&args[0]),
                        Some(AfterLoaderOwnedDescriptor::LoaderCache { .. })
                    )
                    && matches!(
                        canonical_c_int_argument(args[1]),
                        Some(libc::F_GETFD | libc::F_GET_SEALS)
                    )
                    && args[2] == 0 =>
            {
                let expected = match canonical_c_int_argument(args[1]).ok_or(Errno::EPROTO)? {
                    libc::F_GETFD => libc::FD_CLOEXEC as i64,
                    libc::F_GET_SEALS
                        if matches!(
                            state.owned_descriptors.get(&args[0]),
                            Some(AfterLoaderOwnedDescriptor::SealedRuntime { .. })
                        ) =>
                    {
                        crate::after_loader::RUNTIME_SEALS as i64
                    }
                    _ => {
                        return Err(self.caller_error(
                            "private fcntl query is not bound to the sealed runtime",
                        ));
                    }
                };
                Ok(AfterLoaderSyscallEffect::ExpectedResult(expected))
            }
            _ => Err(self.caller_error(format!(
                "private function syscall arguments are refused: nr={} args={args:?}",
                permit.number
            ))),
        }
    }

    fn complete_after_loader_private_syscall(
        &mut self,
        task: &mut Stopped,
        config: &LiteinstAfterLoaderConfig,
        permit: &AfterLoaderSyscallPermit,
        raw_result: i64,
    ) -> Result<AfterLoaderSyscallCompletion, Error> {
        if matches!(
            &permit.effect,
            AfterLoaderSyscallEffect::PtmallocBootstrapEntropy { .. }
        ) {
            return Err(self.caller_error(
                "profiled bootstrap entropy reached the real-kernel completion path",
            ));
        }
        if let AfterLoaderSyscallEffect::CetStatus { destination, .. } = &permit.effect {
            let mut after = [0; 8];
            task.read_exact(*destination as usize, &mut after)?;
            return match complete_cet_status_query(&permit.effect, raw_result, after)
                .expect("CET effect must have a CET completion")
            {
                Ok(completion) => Ok(completion),
                Err(CetStatusCompletionError::OutputChanged) => {
                    Err(self.caller_error("unsupported CET status query changed its output buffer"))
                }
                Err(CetStatusCompletionError::SyscallFailed(result)) => Err(self.caller_error(
                    format!("admitted private syscall failed before its exact effect: {result}"),
                )),
                Err(CetStatusCompletionError::NonzeroSuccess(_)) => {
                    Err(self.caller_error("CET status query did not return zero"))
                }
            };
        }
        if raw_result < 0 {
            return if matches!(&permit.effect, AfterLoaderSyscallEffect::Map { .. }) {
                Ok(AfterLoaderSyscallCompletion::KernelResult(raw_result))
            } else {
                Err(self.caller_error(format!(
                    "admitted private syscall failed before its exact effect: {}",
                    raw_result
                )))
            };
        }
        if !private_memory_effect_result_is_exact(&permit.effect, raw_result) {
            return Err(self
                .caller_error("private mprotect or munmap completion did not return exact zero"));
        }
        match &permit.effect {
            AfterLoaderSyscallEffect::None => {}
            AfterLoaderSyscallEffect::PtmallocBootstrapEntropy { .. } => {
                unreachable!("emulated bootstrap entropy returned before kernel effect handling")
            }
            AfterLoaderSyscallEffect::CetStatus { .. } => {
                unreachable!("CET completion returned before ordinary effect handling")
            }
            AfterLoaderSyscallEffect::OpenProcFdDirectory { expected } => {
                let descriptor = raw_result as u64;
                let state = self.after_loader_private_state()?;
                if state.original_descriptors.contains(&descriptor)
                    || state.owned_descriptors.contains_key(&descriptor)
                {
                    return Err(self.caller_error("proc-fd audit open reused a live descriptor"));
                }
                let descriptor_path = format!("/proc/{}/fd/{descriptor}", task.pid());
                let metadata = std::fs::metadata(&descriptor_path)
                    .map_err(|error| self.caller_error(error))?;
                let link = std::fs::read_link(&descriptor_path)
                    .map_err(|error| self.caller_error(error))?;
                let expected_link = format!("/proc/{}/fd", task.pid());
                let expected_flags =
                    libc::O_DIRECTORY as u64 | libc::O_CLOEXEC as u64 | KERNEL_O_LARGEFILE;
                if !metadata.is_dir()
                    || link.as_os_str().as_encoded_bytes() != expected_link.as_bytes()
                    || descriptor_flags(task.pid(), descriptor)
                        .map_err(|error| self.caller_error(error))?
                        != expected_flags
                    || descriptor_position(task.pid(), descriptor)
                        .map_err(|error| self.caller_error(error))?
                        != 0
                {
                    return Err(self.caller_error(
                        "opened proc-fd audit directory identity, flags or position differ",
                    ));
                }
                let mut expected = expected.clone();
                if expected
                    .insert(descriptor, expected_link.as_bytes().to_vec())
                    .is_some()
                    || proc_fd_link_snapshot(task.pid())
                        .map_err(|error| self.caller_error(error))?
                        != expected
                {
                    return Err(self
                        .caller_error("proc-fd audit directory open changed another descriptor"));
                }
                let state = self.after_loader_private_state_mut()?;
                if state.proc_fd_audit != ProcFdAuditLifecycle::AwaitingOpen
                    || state
                        .owned_descriptors
                        .insert(
                            descriptor,
                            AfterLoaderOwnedDescriptor::ProcFdDirectory {
                                expected,
                                seen: BTreeSet::new(),
                                linked: BTreeSet::new(),
                                eof: false,
                            },
                        )
                        .is_some()
                {
                    return Err(Errno::EPROTO.into());
                }
                state.proc_fd_audit = ProcFdAuditLifecycle::Scanning { descriptor };
            }
            AfterLoaderSyscallEffect::ReadProcFdDirectory {
                descriptor,
                destination,
                capacity,
            } => {
                let amount = u64::try_from(raw_result).map_err(|_| Errno::EPROTO)?;
                if amount > *capacity {
                    return Err(self.caller_error("proc-fd getdents exceeded its exact buffer"));
                }
                let expected = match self
                    .after_loader_private_state()?
                    .owned_descriptors
                    .get(descriptor)
                {
                    Some(AfterLoaderOwnedDescriptor::ProcFdDirectory { expected, .. }) => {
                        expected.clone()
                    }
                    _ => {
                        return Err(
                            self.caller_error("proc-fd getdents completion lost its directory")
                        );
                    }
                };
                if proc_fd_link_snapshot(task.pid()).map_err(|error| self.caller_error(error))?
                    != expected
                {
                    return Err(
                        self.caller_error("live descriptor set changed during proc-fd getdents")
                    );
                }
                let mut descriptors = BTreeSet::new();
                if amount != 0 {
                    let mut bytes = vec![0_u8; usize::try_from(amount).map_err(|_| Errno::EPROTO)?];
                    task.read_exact(*destination as usize, &mut bytes)?;
                    descriptors = proc_fd_dirent_descriptors(&bytes)
                        .map_err(|reason| self.caller_error(reason))?;
                }
                if !self
                    .after_loader_private_state_mut()?
                    .owned_descriptors
                    .get_mut(descriptor)
                    .is_some_and(|owned| owned.record_proc_fd_dirents(descriptors, amount == 0))
                {
                    return Err(Errno::EPROTO.into());
                }
            }
            AfterLoaderSyscallEffect::ReadProcFdLink {
                directory,
                descriptor,
                destination,
                expected,
            } => {
                if raw_result != expected.len() as i64 {
                    return Err(self.caller_error("proc-fd readlink returned a different length"));
                }
                let mut actual = vec![0_u8; expected.len()];
                task.read_exact(*destination as usize, &mut actual)?;
                let snapshot =
                    proc_fd_link_snapshot(task.pid()).map_err(|error| self.caller_error(error))?;
                if actual != *expected || snapshot.get(descriptor) != Some(expected) {
                    return Err(self.caller_error(
                        "proc-fd readlink bytes differ from the renewed physical link",
                    ));
                }
                let state = self.after_loader_private_state_mut()?;
                if !matches!(
                    state.owned_descriptors.get(directory),
                    Some(AfterLoaderOwnedDescriptor::ProcFdDirectory { expected, .. })
                        if expected == &snapshot
                ) || !state
                    .owned_descriptors
                    .get_mut(directory)
                    .is_some_and(|owned| owned.record_proc_fd_readlink(*descriptor))
                {
                    return Err(Errno::EPROTO.into());
                }
            }
            AfterLoaderSyscallEffect::CloseProcFdDirectory {
                descriptor,
                remaining,
            } => {
                if !private_close_result_is_exact(raw_result)
                    || !procfs_path_is_absent(format!("/proc/{}/fd/{descriptor}", task.pid()))
                        .map_err(|error| self.caller_error(error))?
                    || proc_fd_link_snapshot(task.pid())
                        .map_err(|error| self.caller_error(error))?
                        != *remaining
                {
                    return Err(self.caller_error(
                        "proc-fd audit close changed more than its exact directory descriptor",
                    ));
                }
                let state = self.after_loader_private_state_mut()?;
                let removed = state.owned_descriptors.remove(descriptor);
                if state.proc_fd_audit
                    != (ProcFdAuditLifecycle::Scanning {
                        descriptor: *descriptor,
                    })
                    || !matches!(
                        removed,
                        Some(AfterLoaderOwnedDescriptor::ProcFdDirectory { .. })
                    )
                {
                    return Err(Errno::EPROTO.into());
                }
                state.proc_fd_audit = ProcFdAuditLifecycle::Complete;
            }
            AfterLoaderSyscallEffect::OpenLoaderCache {
                descriptor: expected_descriptor,
                redirect,
            } => {
                let descriptor = raw_result as u64;
                let state = self.after_loader_private_state()?;
                if state.original_descriptors.contains(&descriptor)
                    || state.owned_descriptors.contains_key(&descriptor)
                    || state.loader_cache.as_ref().is_none_or(|cache| {
                        cache.lifecycle != LoaderCacheLifecycle::AwaitingOpen
                            || cache.scratch != redirect.scratch
                            || cache.scratch_preimage != redirect.preimage
                    })
                {
                    return Err(self.caller_error(
                        "loader-cache open reused a descriptor or changed lifecycle authority",
                    ));
                }
                let evidence = self.loader_cache_artifact_evidence(config)?;
                if !matches!(
                    expected_descriptor,
                    AfterLoaderOwnedDescriptor::LoaderCache {
                        file,
                        mapping,
                        length,
                        position: 0,
                    } if *file == evidence.file
                        && *mapping == evidence.mapping
                        && *length == evidence.length
                ) {
                    return Err(self.caller_error(
                        "loader-cache open effect differs from the renewed sealed artifact",
                    ));
                }
                let path = format!("/proc/{}/fd/{descriptor}", task.pid());
                let metadata =
                    std::fs::metadata(&path).map_err(|error| self.caller_error(error))?;
                let link = std::fs::read_link(&path).map_err(|error| self.caller_error(error))?;
                let flags = descriptor_flags(task.pid(), descriptor)
                    .map_err(|error| self.caller_error(error))?;
                let mut opened = open_loader_cache_without_atime(&path)
                    .map_err(|error| self.caller_error(error))?;
                let seals = unsafe { libc::fcntl(opened.as_raw_fd(), libc::F_GET_SEALS) };
                let mut bytes = Vec::new();
                (&mut opened)
                    .take(evidence.length.checked_add(1).ok_or(Errno::EOVERFLOW)?)
                    .read_to_end(&mut bytes)
                    .map_err(|error| self.caller_error(error))?;
                let expected_flags =
                    libc::O_RDONLY as u64 | libc::O_CLOEXEC as u64 | KERNEL_O_LARGEFILE;
                if !metadata.is_file()
                    || crate::after_loader::FileIdentity::from_metadata(&metadata) != evidence.file
                    || metadata.len() != evidence.length
                    || link.as_os_str().as_encoded_bytes() != LOADER_CACHE_MEMFD_LINK
                    || flags != expected_flags
                    || descriptor_position(task.pid(), descriptor)
                        .map_err(|error| self.caller_error(error))?
                        != 0
                    || seals != config.immutable_loader_cache().seals()
                    || bytes.as_slice() != config.loader_cache().bytes()
                {
                    return Err(self.caller_error(
                        "opened loader-cache identity, complete bytes, seals, flags or position differ",
                    ));
                }
                let preimage = self.restore_loader_cache_redirect(task, permit, raw_result)?;
                let state = self.after_loader_private_state_mut()?;
                let cache = state.loader_cache.as_mut().ok_or(Errno::EPROTO)?;
                if cache.scratch_preimage != preimage
                    || !cache.lifecycle.complete_open(descriptor)
                    || state
                        .owned_descriptors
                        .insert(descriptor, expected_descriptor.clone())
                        .is_some()
                {
                    return Err(Errno::EPROTO.into());
                }
            }
            AfterLoaderSyscallEffect::StatLoaderCache {
                descriptor,
                destination,
                fields,
            } => {
                if raw_result != 0 {
                    return Err(self.caller_error("loader-cache fstat did not return zero"));
                }
                self.authenticate_loader_cache_descriptor(task, config, *descriptor)?;
                self.authenticate_loader_cache_scratch(task)?;
                let metadata = std::fs::metadata(format!("/proc/{}/fd/{descriptor}", task.pid()))
                    .map_err(|error| self.caller_error(error))?;
                let mut bytes = [0_u8; 144];
                task.read_exact(*destination as usize, &mut bytes)?;
                if AfterLoaderStatFields::from_metadata(&metadata) != *fields
                    || !stat_output_matches(*fields, &bytes)
                {
                    return Err(
                        self.caller_error("loader-cache fstat target or exact output changed")
                    );
                }
                if !self
                    .after_loader_private_state_mut()?
                    .loader_cache
                    .as_mut()
                    .ok_or(Errno::EPROTO)?
                    .lifecycle
                    .complete_stat(*descriptor)
                {
                    return Err(
                        self.caller_error("loader-cache fstat lifecycle changed before completion")
                    );
                }
            }
            AfterLoaderSyscallEffect::MapLoaderCache {
                descriptor,
                raw_length,
                identity,
            } => {
                let evidence =
                    self.authenticate_loader_cache_descriptor(task, config, *descriptor)?;
                self.authenticate_loader_cache_scratch(task)?;
                let start = raw_result as u64;
                let (start, end) = checked_page_effect_range(start, *raw_length)?;
                let state = self.after_loader_private_state()?;
                if start % PAGE != 0
                    || evidence.length != *raw_length
                    || evidence.mapping != *identity
                    || state.refuses_original_overlap((start, end))
                    || state.refuses_protected_overlap((start, end))
                    || state
                        .owned_mappings
                        .iter()
                        .any(|mapping| ranges_overlap((mapping.start, mapping.end), (start, end)))
                {
                    return Err(self.caller_error(
                        "loader-cache mmap result overlaps memory or changed artifact identity",
                    ));
                }
                let maps = guest_maps(task.pid()).ok_or(Errno::EPROTO)?;
                let physical = maps
                    .iter()
                    .filter(|mapping| {
                        mapping.start == start
                            && mapping.end == end
                            && mapping.readable
                            && !mapping.writable
                            && !mapping.executable
                            && !mapping.shared
                            && mapping.offset == 0
                            && mapping.mapping_identity() == *identity
                            && mapping.path.as_ref().is_some_and(|path| {
                                path.as_os_str().as_encoded_bytes() == LOADER_CACHE_MEMFD_LINK
                            })
                    })
                    .count();
                let mut bytes =
                    vec![0_u8; usize::try_from(*raw_length).map_err(|_| Errno::EOVERFLOW)?];
                task.read_exact(start as usize, &mut bytes)?;
                let raw_end = start.checked_add(*raw_length).ok_or(Errno::EOVERFLOW)?;
                if physical != 1
                    || bytes.as_slice() != config.loader_cache().bytes()
                    || !target_range_is_zero(task, raw_end, end - raw_end)?
                {
                    return Err(self.caller_error(
                        "loader-cache mmap physical identity or complete bytes differ",
                    ));
                }
                let mapping = LoaderCacheMapping {
                    start,
                    raw_length: *raw_length,
                    end,
                    identity: *identity,
                };
                let state = self.after_loader_private_state_mut()?;
                if !state
                    .loader_cache
                    .as_mut()
                    .ok_or(Errno::EPROTO)?
                    .lifecycle
                    .complete_map(*descriptor, mapping, *raw_length, *identity)
                {
                    return Err(Errno::EPROTO.into());
                }
                state.owned_mappings.push(AfterLoaderOwnedMapping {
                    start,
                    end,
                    readable: true,
                    writable: false,
                    executable: false,
                    shared: false,
                    descriptor: Some(*descriptor),
                    offset: 0,
                    purpose: AfterLoaderMappingPurpose::LoaderCache,
                });
            }
            AfterLoaderSyscallEffect::CloseLoaderCache {
                descriptor,
                mapping,
            } => {
                if raw_result != 0 {
                    return Err(self.caller_error("loader-cache close did not return zero"));
                }
                self.authenticate_loader_cache_scratch(task)?;
                self.authenticate_loader_cache_mapping(task, config, *mapping)?;
                let descriptor_absent =
                    procfs_path_is_absent(format!("/proc/{}/fd/{descriptor}", task.pid()))
                        .map_err(|error| self.caller_error(error))?;
                if !descriptor_absent
                    || !matches!(
                        self.after_loader_private_state()?
                            .owned_descriptors
                            .get(descriptor),
                        Some(AfterLoaderOwnedDescriptor::LoaderCache { .. })
                    )
                {
                    return Err(self.caller_error(
                        "loader-cache descriptor survived close or lost typed ownership",
                    ));
                }
                let state = self.after_loader_private_state_mut()?;
                if !state
                    .loader_cache
                    .as_mut()
                    .ok_or(Errno::EPROTO)?
                    .lifecycle
                    .complete_close(*descriptor, *mapping)
                    || state.owned_descriptors.remove(descriptor).is_none()
                {
                    return Err(Errno::EPROTO.into());
                }
            }
            AfterLoaderSyscallEffect::RetireLoaderCache {
                descriptor,
                mapping,
            } => {
                if raw_result != 0 {
                    return Err(self.caller_error("loader-cache munmap did not return zero"));
                }
                self.authenticate_loader_cache_scratch(task)?;
                let evidence = self.loader_cache_artifact_evidence(config)?;
                let maps = guest_maps(task.pid()).ok_or(Errno::EPROTO)?;
                let descriptor_absent =
                    procfs_path_is_absent(format!("/proc/{}/fd/{descriptor}", task.pid()))
                        .map_err(|error| self.caller_error(error))?;
                if mapping.identity != evidence.mapping
                    || mapping.raw_length != evidence.length
                    || !descriptor_absent
                    || maps.iter().any(|candidate| {
                        ranges_overlap(
                            (candidate.start, candidate.end),
                            (mapping.start, mapping.end),
                        ) || candidate.mapping_identity() == mapping.identity
                    })
                {
                    return Err(self.caller_error(
                        "loader-cache descriptor or mapping survived exact retirement",
                    ));
                }
                let state = self.after_loader_private_state_mut()?;
                let owners = state
                    .owned_mappings
                    .iter()
                    .filter(|owned| owned.purpose == AfterLoaderMappingPurpose::LoaderCache)
                    .count();
                if owners != 1
                    || !state
                        .loader_cache
                        .as_mut()
                        .ok_or(Errno::EPROTO)?
                        .lifecycle
                        .complete_retire(*descriptor, *mapping)
                {
                    return Err(Errno::EPROTO.into());
                }
                state.remove_owned_range((mapping.start, mapping.end));
                if state
                    .owned_mappings
                    .iter()
                    .any(|owned| owned.purpose == AfterLoaderMappingPurpose::LoaderCache)
                    || state.owned_descriptors.values().any(|owned| {
                        matches!(owned, AfterLoaderOwnedDescriptor::LoaderCache { .. })
                    })
                {
                    return Err(Errno::EPROTO.into());
                }
            }
            AfterLoaderSyscallEffect::Open(kind)
            | AfterLoaderSyscallEffect::OpenLoaderCacheAlias(kind) => {
                let descriptor = raw_result as u64;
                let alias_effect = matches!(
                    &permit.effect,
                    AfterLoaderSyscallEffect::OpenLoaderCacheAlias(_)
                );
                if alias_effect
                    != matches!(kind, AfterLoaderOwnedDescriptor::LoaderCacheAlias { .. })
                {
                    return Err(self.caller_error(
                        "loader-cache alias open effect lost its dedicated descriptor type",
                    ));
                }
                let state = self.after_loader_private_state()?;
                if state.original_descriptors.contains(&descriptor)
                    || state.owned_descriptors.contains_key(&descriptor)
                {
                    return Err(self.caller_error("private open reused a live descriptor"));
                }
                let metadata = std::fs::metadata(format!("/proc/{}/fd/{descriptor}", task.pid()))
                    .map_err(|error| self.caller_error(error))?;
                let flags = descriptor_flags(task.pid(), descriptor)
                    .map_err(|error| self.caller_error(error))?;
                let expected_access = match kind {
                    AfterLoaderOwnedDescriptor::Trampoline { .. } => libc::O_RDWR,
                    _ => libc::O_RDONLY,
                } as u64;
                let expected_flags = expected_access | libc::O_CLOEXEC as u64 | KERNEL_O_LARGEFILE;
                if flags != expected_flags || !metadata.is_file() {
                    return Err(self.caller_error(
                        "private descriptor type, access mode or close-on-exec flag differs",
                    ));
                }
                let stored = match kind {
                    AfterLoaderOwnedDescriptor::SealedRuntime { bytes, position } => {
                        if metadata.dev() != config.sealed_runtime.image.file_identity.device
                            || metadata.ino() != config.sealed_runtime.image.file_identity.inode
                            || metadata.len() != bytes.len() as u64
                            || *position != 0
                        {
                            return Err(self.caller_error(
                                "opened sealed-runtime descriptor identity differs",
                            ));
                        }
                        let mut file =
                            std::fs::File::open(format!("/proc/{}/fd/{descriptor}", task.pid()))
                                .map_err(|error| self.caller_error(error))?;
                        let mut bytes = Vec::new();
                        (&mut file)
                            .take(crate::after_loader::MAX_RUNTIME_FILE as u64 + 1)
                            .read_to_end(&mut bytes)
                            .map_err(|error| self.caller_error(error))?;
                        if bytes.as_slice() != config.sealed_runtime.image.bytes.as_ref() {
                            return Err(self.caller_error("opened sealed-runtime bytes differ"));
                        }
                        kind.clone()
                    }
                    AfterLoaderOwnedDescriptor::BoundImage {
                        image,
                        bytes: bound_bytes,
                        position,
                    } => {
                        if metadata.dev() != image.file.device
                            || metadata.ino() != image.file.inode
                            || metadata.len() != bound_bytes.len() as u64
                            || *position != 0
                        {
                            return Err(
                                self.caller_error("opened graph descriptor identity differs")
                            );
                        }
                        let expected = dlopen_graph_images(&config.provider, &config.dependencies)
                            .find(|expected| state.image_id(expected) == *image)
                            .ok_or_else(|| {
                                self.caller_error("opened graph identity is no longer bound")
                            })?;
                        let mut file =
                            std::fs::File::open(format!("/proc/{}/fd/{descriptor}", task.pid()))
                                .map_err(|error| self.caller_error(error))?;
                        let mut bytes = Vec::new();
                        (&mut file)
                            .take(
                                (bound_bytes.len() as u64)
                                    .checked_add(1)
                                    .ok_or(Errno::EOVERFLOW)?,
                            )
                            .read_to_end(&mut bytes)
                            .map_err(|error| self.caller_error(error))?;
                        if bytes.as_slice() != expected.bytes.as_ref()
                            || bytes.as_slice() != bound_bytes.as_ref()
                        {
                            return Err(self.caller_error("opened graph bytes differ"));
                        }
                        kind.clone()
                    }
                    AfterLoaderOwnedDescriptor::LoaderCacheAlias { .. } => {
                        self.authenticate_loader_cache_alias_owned_descriptor(
                            task, config, descriptor, kind,
                        )?;
                        kind.clone()
                    }
                    AfterLoaderOwnedDescriptor::ProcMaps {
                        device,
                        inode,
                        bytes,
                        position,
                    } => {
                        let link =
                            std::fs::read_link(format!("/proc/{}/fd/{descriptor}", task.pid()))
                                .map_err(|error| self.caller_error(error))?;
                        let expected = format!("/proc/{}/maps", task.pid());
                        if link.as_os_str().as_encoded_bytes() != expected.as_bytes()
                            || metadata.dev() != *device
                            || metadata.ino() != *inode
                            || *position != 0
                            || bounded_proc(&expected, 2 * 1024 * 1024)
                                .map_err(|error| self.caller_error(error))?
                                .as_slice()
                                != bytes.as_ref()
                        {
                            return Err(
                                self.caller_error("private proc-maps descriptor target differs")
                            );
                        }
                        kind.clone()
                    }
                    AfterLoaderOwnedDescriptor::ProcFdDirectory { .. } => {
                        return Err(self.caller_error(
                            "proc-fd directory reached the generic open completion path",
                        ));
                    }
                    AfterLoaderOwnedDescriptor::LoaderCache { .. } => {
                        return Err(self.caller_error(
                            "loader-cache descriptor reached the generic open completion path",
                        ));
                    }
                    AfterLoaderOwnedDescriptor::Trampoline { id, size } => {
                        let link =
                            std::fs::read_link(format!("/proc/{}/fd/{descriptor}", task.pid()))
                                .map_err(|error| self.caller_error(error))?;
                        if link.as_os_str().as_encoded_bytes()
                            != b"/memfd:liteinst2-trampoline (deleted)"
                            || size.is_some()
                            || id.is_some()
                            || metadata.len() != 0
                        {
                            return Err(self.caller_error("private trampoline memfd name differs"));
                        }
                        let file = crate::after_loader::FileIdentity::from_metadata(&metadata);
                        let id = self
                            .after_loader_private_state_mut()?
                            .next_trampoline_id(file)
                            .ok_or(Errno::EOVERFLOW)?;
                        AfterLoaderOwnedDescriptor::Trampoline {
                            id: Some(id),
                            size: None,
                        }
                    }
                };
                if descriptor_position(task.pid(), descriptor)
                    .map_err(|error| self.caller_error(error))?
                    != 0
                {
                    return Err(self.caller_error("new private descriptor has a nonzero position"));
                }
                let alias_binding = match &stored {
                    AfterLoaderOwnedDescriptor::LoaderCacheAlias {
                        image, raw_path, ..
                    } => Some((raw_path.clone(), *image)),
                    _ => None,
                };
                let state = self.after_loader_private_state_mut()?;
                if state.owned_descriptors.insert(descriptor, stored).is_some() {
                    return Err(Errno::EPROTO.into());
                }
                if let Some((raw_path, image)) = alias_binding
                    && !state
                        .loader_cache
                        .as_mut()
                        .and_then(|cache| cache.aliases.get_mut(&raw_path))
                        .is_some_and(|lifecycle| lifecycle.complete_open(descriptor, image))
                {
                    return Err(self.caller_error(
                        "loader-cache alias open lifecycle changed before completion",
                    ));
                }
            }
            AfterLoaderSyscallEffect::Close(descriptor) => {
                if !private_close_result_is_exact(raw_result) {
                    return Err(self.caller_error("private close did not return exact zero"));
                }
                let closing = self
                    .after_loader_private_state()?
                    .owned_descriptors
                    .get(descriptor)
                    .cloned()
                    .ok_or_else(|| self.caller_error("private close completion lost ownership"))?;
                if let AfterLoaderOwnedDescriptor::Trampoline {
                    id: Some(trampoline),
                    size: Some(size),
                } = &closing
                {
                    let state = self.after_loader_private_state()?;
                    let identity = state.trampoline_mapping(*trampoline);
                    let close_shape = state
                        .trampoline_close_shape(*descriptor, *trampoline, *size)
                        .ok_or_else(|| {
                            self.caller_error(
                                "trampoline descriptor closed with a partial alias/reservation set",
                            )
                        })?;
                    let maps = guest_maps(task.pid()).ok_or(Errno::EPROTO)?;
                    match close_shape {
                        TrampolineCloseShape::Complete => {
                            if !state.trampoline_seals_added.contains(descriptor)
                                || !state.sealed_trampolines.contains(trampoline)
                            {
                                return Err(self.caller_error(
                                    "retained trampoline was closed before exact seal verification",
                                ));
                            }
                            let identity = identity.ok_or_else(|| {
                                self.caller_error(
                                    "retained trampoline mapping identity was never captured",
                                )
                            })?;
                            let state = self.after_loader_private_state()?;
                            let aliases = state
                                .owned_mappings
                                .iter()
                                .filter(|mapping| {
                                    mapping.purpose
                                        == AfterLoaderMappingPurpose::Trampoline {
                                            trampoline: *trampoline,
                                        }
                                })
                                .collect::<Vec<_>>();
                            let reservations = state
                                .owned_mappings
                                .iter()
                                .filter(|mapping| {
                                    mapping.purpose
                                        == (AfterLoaderMappingPurpose::SharedReservation {
                                            trampoline: *trampoline,
                                        })
                                })
                                .collect::<Vec<_>>();
                            let reservation_binding =
                                state.shared_reservations.get(trampoline).ok_or_else(|| {
                                    self.caller_error(
                                        "retained trampoline reservation identity disappeared",
                                    )
                                })?;
                            let reservation_changed = !maps.iter().any(|mapping| {
                                mapping.start == reservations[0].start
                                    && mapping.end == reservations[0].end
                                    && mapping.offset == 0
                                    && mapping.readable
                                    && mapping.writable
                                    && !mapping.executable
                                    && mapping.shared
                                    && exact_shared_reservation_identity(mapping).as_ref()
                                        == Some(&reservation_binding.1)
                            });
                            if reservation_changed
                                || aliases.iter().any(|alias| {
                                    !maps.iter().any(|mapping| {
                                        mapping.start == alias.start
                                            && mapping.end == alias.end
                                            && mapping.offset == 0
                                            && mapping.readable
                                            && mapping.writable == alias.writable
                                            && mapping.executable == alias.executable
                                            && mapping.shared
                                            && mapping.mapping_identity() == identity
                                            && mapping.path.as_ref().is_some_and(|path| {
                                                path.as_os_str().as_encoded_bytes()
                                                    == b"/memfd:liteinst2-trampoline (deleted)"
                                            })
                                    })
                                })
                            {
                                return Err(self.caller_error(
                                    "trampoline aliases changed before descriptor close completed",
                                ));
                            }
                        }
                        TrampolineCloseShape::Abandoned => {
                            if state.trampoline_seals_added.contains(descriptor)
                                || state.sealed_trampolines.contains(trampoline)
                            {
                                return Err(self.caller_error(
                                    "abandoned trampoline carried partial seal state",
                                ));
                            }
                            if identity.is_some_and(|identity| {
                                maps.iter()
                                    .any(|mapping| mapping.mapping_identity() == identity)
                            }) {
                                return Err(self.caller_error(
                                    "abandoned trampoline still has a live mapping",
                                ));
                            }
                            if self
                                .after_loader_private_state_mut()?
                                .trampoline_mappings
                                .remove(trampoline)
                                != identity
                            {
                                return Err(self.caller_error(
                                    "abandoned trampoline mapping identity changed during close",
                                ));
                            }
                        }
                    }
                } else if matches!(&closing, AfterLoaderOwnedDescriptor::Trampoline { .. }) {
                    return Err(self.caller_error(
                        "trampoline descriptor closed before exact aliases were bound",
                    ));
                }
                if !procfs_path_is_absent(format!("/proc/{}/fd/{descriptor}", task.pid()))
                    .map_err(|error| self.caller_error(error))?
                {
                    return Err(self
                        .caller_error("private descriptor remained live after successful close"));
                }
                let alias_completion = match &closing {
                    AfterLoaderOwnedDescriptor::LoaderCacheAlias {
                        image, raw_path, ..
                    } => {
                        let alias = config
                            .loader_cache_alias_for_path(raw_path.as_os_str().as_bytes())
                            .ok_or_else(|| {
                                self.caller_error(
                                    "closed loader-cache alias left the bound cache profile",
                                )
                            })?;
                        if !target_loader_cache_alias_matches(task.pid(), alias)
                            .map_err(|error| self.caller_error(error))?
                        {
                            return Err(self.caller_error(
                                "loader-cache alias target-root binding changed during close",
                            ));
                        }
                        let geometry = self.resolve_loader_cache_alias_pre_relro_geometry(
                            task,
                            alias.dependency(),
                            *image,
                        )?;
                        self.authenticate_after_loader_image_backing(
                            task,
                            alias.dependency(),
                            geometry,
                        )?;
                        let maps = guest_maps(task.pid()).ok_or(Errno::EPROTO)?;
                        if !self
                            .after_loader_private_state()?
                            .has_causal_image_mapping(geometry)
                            || !exact_image_mapping_paths_are_literal(
                                alias.dependency(),
                                geometry,
                                &maps,
                            )
                        {
                            return Err(self.caller_error(
                                "loader-cache alias close lacks its causal canonical-path geometry",
                            ));
                        }
                        let plan = loader_cache_alias_mmap_plan(&alias.dependency().bytes)
                            .map_err(|reason| self.caller_error(reason))?;
                        Some((raw_path.clone(), *image, geometry, plan))
                    }
                    _ => None,
                };
                if self
                    .after_loader_private_state_mut()?
                    .owned_descriptors
                    .remove(descriptor)
                    .is_none()
                {
                    return Err(self.caller_error("private close completion lost ownership"));
                }
                self.after_loader_private_state_mut()?
                    .trampoline_seals_added
                    .remove(descriptor);
                if let Some((raw_path, image, geometry, plan)) = alias_completion
                    && !self
                        .after_loader_private_state_mut()?
                        .loader_cache
                        .as_mut()
                        .and_then(|cache| cache.aliases.get_mut(&raw_path))
                        .is_some_and(|lifecycle| {
                            lifecycle.complete_close(&plan, *descriptor, image, geometry)
                        })
                {
                    return Err(self.caller_error(
                        "loader-cache alias close lifecycle changed before completion",
                    ));
                }
            }
            AfterLoaderSyscallEffect::Map {
                requested,
                raw_length,
                protection,
                flags,
                descriptor,
                offset,
                purpose,
            } => {
                let start = raw_result as u64;
                let (start, end) = checked_page_effect_range(start, *raw_length)?;
                let alias_owned = descriptor.and_then(|descriptor| {
                    self.after_loader_private_state()
                        .ok()?
                        .owned_descriptors
                        .get(&descriptor)
                        .filter(|owned| {
                            matches!(owned, AfterLoaderOwnedDescriptor::LoaderCacheAlias { .. })
                        })
                        .cloned()
                });
                let alias_binding = match (descriptor, &alias_owned) {
                    (
                        Some(descriptor),
                        Some(AfterLoaderOwnedDescriptor::LoaderCacheAlias {
                            image,
                            raw_path,
                            canonical_path,
                            bytes,
                            ..
                        }),
                    ) => {
                        self.authenticate_loader_cache_alias_descriptor(task, config, *descriptor)?;
                        let plan = loader_cache_alias_mmap_plan(bytes)
                            .map_err(|reason| self.caller_error(reason))?;
                        Some((
                            *descriptor,
                            raw_path.clone(),
                            *image,
                            canonical_path.clone(),
                            bytes.clone(),
                            plan,
                        ))
                    }
                    _ => None,
                };
                let state = self.after_loader_private_state()?;
                if (*requested != 0 && start != *requested)
                    || start % PAGE != 0
                    || state.refuses_original_overlap((start, end))
                {
                    return Err(
                        self.caller_error("private mmap result overlaps original target memory")
                    );
                }
                if let Some(descriptor) = descriptor {
                    let owned = state.owned_descriptors.get(descriptor).ok_or_else(|| {
                        self.caller_error("private mmap descriptor ownership disappeared")
                    })?;
                    // The file-byte bound is intentionally expressed using the
                    // raw length. Linux permits the rounded final page of a file
                    // mapping even when only a prefix is backed by file bytes.
                    if let AfterLoaderOwnedDescriptor::Trampoline {
                        size: Some(size), ..
                    } = owned
                        && offset
                            .checked_add(*raw_length)
                            .is_none_or(|end| end > *size)
                    {
                        return Err(self.caller_error("trampoline mapping exceeds its memfd"));
                    }
                }
                let maps = guest_maps(task.pid()).ok_or(Errno::EPROTO)?;
                let mapped = maps
                    .iter()
                    .find(|mapping| {
                        mapping.start <= start
                            && end <= mapping.end
                            && mapping.readable == (*protection & libc::PROT_READ != 0)
                            && mapping.writable == (*protection & libc::PROT_WRITE != 0)
                            && mapping.executable == (*protection & libc::PROT_EXEC != 0)
                            && mapping.shared == (*flags & libc::MAP_SHARED != 0)
                    })
                    .ok_or_else(|| {
                        self.caller_error(
                            "private mmap result is absent or has different permissions",
                        )
                    })?;
                if mapped.offset.checked_add(start - mapped.start) != Some(*offset) {
                    return Err(self.caller_error("private mmap result offset differs"));
                }
                if let Some((_, _, image, canonical_path, bytes, _)) = &alias_binding {
                    let file_offset = usize::try_from(*offset).map_err(|_| Errno::EOVERFLOW)?;
                    let raw_length = usize::try_from(*raw_length).map_err(|_| Errno::EOVERFLOW)?;
                    let file_bytes = bytes.get(file_offset..).ok_or_else(|| {
                        self.caller_error("loader-cache alias mmap begins beyond the bound file")
                    })?;
                    let amount = raw_length.min(file_bytes.len());
                    if amount == 0
                        || mapped.path.as_deref() != Some(canonical_path.as_path())
                        || mapped.mapping_identity()
                            != target_bound_image_mapping_identity(
                                task.pid(),
                                dlopen_graph_images(&config.provider, &config.dependencies)
                                    .find(|candidate| {
                                        self.after_loader_private_state()
                                            .is_ok_and(|state| state.image_id(candidate) == *image)
                                    })
                                    .ok_or_else(|| {
                                        self.caller_error(
                                            "loader-cache alias image left the bound graph",
                                        )
                                    })?,
                            )
                            .map_err(|error| self.caller_error(error))?
                            .ok_or_else(|| {
                                self.caller_error(
                                    "loader-cache alias canonical mapping identity disappeared",
                                )
                            })?
                    {
                        return Err(self.caller_error(
                            "loader-cache alias mmap path, offset or mapping identity differs",
                        ));
                    }
                    let mut observed = vec![0_u8; amount];
                    task.read_exact(start as usize, &mut observed)?;
                    if observed.as_slice() != &file_bytes[..amount]
                        || raw_length > amount
                            && !target_range_is_zero(
                                task,
                                start.checked_add(amount as u64).ok_or(Errno::EOVERFLOW)?,
                                (raw_length - amount) as u64,
                            )?
                    {
                        return Err(
                            self.caller_error("loader-cache alias mmap complete file bytes differ")
                        );
                    }
                }
                if let Some(descriptor) = descriptor {
                    let owned = self
                        .after_loader_private_state()?
                        .owned_descriptors
                        .get(descriptor)
                        .cloned()
                        .ok_or(Errno::EPROTO)?;
                    let metadata =
                        std::fs::metadata(format!("/proc/{}/fd/{descriptor}", task.pid()))
                            .map_err(|error| self.caller_error(error))?;
                    match (&owned, purpose) {
                        (
                            AfterLoaderOwnedDescriptor::SealedRuntime { .. },
                            AfterLoaderMappingPurpose::Image { image },
                        ) if *image
                            == self
                                .after_loader_private_state()?
                                .image_id(&config.sealed_runtime.image) =>
                        {
                            self.caller_sealed_runtime(task, config, *descriptor)?;
                        }
                        (
                            AfterLoaderOwnedDescriptor::BoundImage { image: owned, .. },
                            AfterLoaderMappingPurpose::Image { image },
                        ) if owned == image
                            && metadata.dev() == image.file.device
                            && metadata.ino() == image.file.inode => {}
                        (
                            AfterLoaderOwnedDescriptor::LoaderCacheAlias {
                                image: owned,
                                stamp,
                                ..
                            },
                            AfterLoaderMappingPurpose::Image { image },
                        ) if owned == image
                            && metadata.dev() == image.file.device
                            && metadata.ino() == image.file.inode
                            && stable_backing_stamp(&metadata) == *stamp => {}
                        (
                            AfterLoaderOwnedDescriptor::Trampoline {
                                id: Some(owned),
                                size: Some(size),
                            },
                            AfterLoaderMappingPurpose::Trampoline { trampoline },
                        ) if owned == trampoline
                            && metadata.dev() == trampoline.file.device
                            && metadata.ino() == trampoline.file.inode
                            && metadata.len() == *size => {}
                        _ => {
                            return Err(self.caller_error(
                                "private mmap descriptor authority or file identity changed",
                            ));
                        }
                    }
                } else {
                    let identity_matches = match purpose {
                        AfterLoaderMappingPurpose::SharedReservation { .. } => {
                            exact_shared_reservation_identity(mapped).is_some()
                        }
                        _ => mapped.inode == 0 && mapped.path.is_none(),
                    };
                    if !identity_matches {
                        return Err(self.caller_error(
                            "private anonymous mapping acquired a different kernel identity",
                        ));
                    }
                }
                if matches!(
                    purpose,
                    AfterLoaderMappingPurpose::Controller
                        | AfterLoaderMappingPurpose::CallbackStack
                        | AfterLoaderMappingPurpose::ImageZeroFill { .. }
                        | AfterLoaderMappingPurpose::SharedReservation { .. }
                        | AfterLoaderMappingPurpose::Trampoline { .. }
                ) && !target_range_is_zero(task, start, end - start)?
                {
                    return Err(self.caller_error(
                        "new private anonymous mapping was not exactly zero-filled",
                    ));
                }
                if matches!(
                    purpose,
                    AfterLoaderMappingPurpose::CallbackStack
                        | AfterLoaderMappingPurpose::Trampoline { .. }
                        | AfterLoaderMappingPurpose::SharedReservation { .. }
                ) && (mapped.start != start || mapped.end != end)
                {
                    return Err(self.caller_error("trampoline mapping merged with another mapping"));
                }
                if let AfterLoaderMappingPurpose::SharedReservation { trampoline } = purpose {
                    let state = self.after_loader_private_state()?;
                    if state.shared_reservations.contains_key(trampoline)
                        || state.trampoline_awaiting_shared_reservation() != Some(*trampoline)
                    {
                        return Err(self
                            .caller_error("shared reservation lost its unique trampoline owner"));
                    }
                }
                let alias_zero_fill =
                    if let AfterLoaderMappingPurpose::ImageZeroFill { image } = purpose {
                        let matching = self
                            .after_loader_private_state()?
                            .loader_cache
                            .as_ref()
                            .into_iter()
                            .flat_map(|cache| cache.aliases.iter())
                            .filter_map(|(raw_path, lifecycle)| match lifecycle {
                                LoaderCacheAliasLifecycle::Mapped {
                                    image: alias_image, ..
                                } if alias_image == image => Some((raw_path.clone(), *image)),
                                _ => None,
                            })
                            .collect::<Vec<_>>();
                        match matching.as_slice() {
                            [] => None,
                            [(raw_path, image)] => {
                                let alias = config
                                    .loader_cache_alias_for_path(raw_path.as_os_str().as_bytes())
                                    .ok_or_else(|| {
                                        self.caller_error(
                                            "loader-cache alias zero-fill left its bound profile",
                                        )
                                    })?;
                                let plan = loader_cache_alias_mmap_plan(&alias.dependency().bytes)
                                    .map_err(|reason| self.caller_error(reason))?;
                                Some((raw_path.clone(), *image, plan))
                            }
                            _ => {
                                return Err(self.caller_error(
                                    "loader-cache alias zero-fill authority is not unique",
                                ));
                            }
                        }
                    } else {
                        None
                    };
                let state = self.after_loader_private_state_mut()?;
                match purpose {
                    AfterLoaderMappingPurpose::Image { image } => {
                        if !state.bind_image_mapping(*image, mapped.mapping_identity()) {
                            return Err(Error::runtime(
                                task.pid(),
                                "bind private image mapping identity",
                                "mapping identity changed, collided or left its generation",
                            ));
                        }
                    }
                    AfterLoaderMappingPurpose::ImageZeroFill { image } => {
                        if state.image_mappings.get(image).is_none() {
                            return Err(Error::runtime(
                                task.pid(),
                                "bind private image zero-fill mapping",
                                "zero-fill mapping preceded its file identity",
                            ));
                        }
                    }
                    AfterLoaderMappingPurpose::Trampoline { trampoline } => {
                        if !state.bind_trampoline_mapping(*trampoline, mapped.mapping_identity()) {
                            return Err(Error::runtime(
                                task.pid(),
                                "bind private trampoline mapping identity",
                                "trampoline mapping identity changed, collided or left its generation",
                            ));
                        }
                    }
                    AfterLoaderMappingPurpose::Controller
                    | AfterLoaderMappingPurpose::CallbackStack
                    | AfterLoaderMappingPurpose::SharedReservation { .. } => {}
                    AfterLoaderMappingPurpose::LoaderCache => {
                        return Err(Error::runtime(
                            task.pid(),
                            "bind private mapping",
                            "loader cache reached the generic mmap completion path",
                        ));
                    }
                }
                state.remove_owned_range((start, end));
                if let AfterLoaderMappingPurpose::SharedReservation { trampoline } = purpose {
                    let identity = exact_shared_reservation_identity(mapped).ok_or_else(|| {
                        Error::runtime(
                            task.pid(),
                            "bind private shared trampoline reservation",
                            "shared reservation lacks the exact Linux /dev/zero identity",
                        )
                    })?;
                    if state
                        .shared_reservations
                        .insert(*trampoline, ((start, end), identity))
                        .is_some()
                    {
                        return Err(Error::runtime(
                            task.pid(),
                            "bind private shared trampoline reservation",
                            "trampoline already owned a shared reservation",
                        ));
                    }
                }
                let owned_mapping = AfterLoaderOwnedMapping {
                    start,
                    end,
                    readable: *protection & libc::PROT_READ != 0,
                    writable: *protection & libc::PROT_WRITE != 0,
                    executable: *protection & libc::PROT_EXEC != 0,
                    shared: *flags & libc::MAP_SHARED != 0,
                    descriptor: *descriptor,
                    offset: *offset,
                    purpose: *purpose,
                };
                state.owned_mappings.push(owned_mapping);
                if let Some((descriptor, raw_path, image, _, _, plan)) = alias_binding
                    && !state
                        .loader_cache
                        .as_mut()
                        .and_then(|cache| cache.aliases.get_mut(&raw_path))
                        .is_some_and(|lifecycle| {
                            lifecycle.complete_map(
                                &plan,
                                descriptor,
                                image,
                                LoaderCacheAliasMapping {
                                    owned: owned_mapping,
                                    identity: mapped.mapping_identity(),
                                    raw_length: *raw_length,
                                    fixed: *flags & libc::MAP_FIXED != 0,
                                },
                            )
                        })
                {
                    return Err(self.caller_error(
                        "loader-cache alias mmap lifecycle changed before completion",
                    ));
                }
                if let Some((raw_path, image, plan)) = alias_zero_fill
                    && !state
                        .loader_cache
                        .as_mut()
                        .and_then(|cache| cache.aliases.get_mut(&raw_path))
                        .is_some_and(|lifecycle| {
                            lifecycle.complete_zero_fill(&plan, image, *raw_length, owned_mapping)
                        })
                {
                    return Err(self.caller_error(
                        "loader-cache alias zero-fill lifecycle changed before completion",
                    ));
                }
            }
            AfterLoaderSyscallEffect::Protect {
                start,
                raw_length,
                protection,
                loader_cache_alias,
            } => {
                let range = checked_page_effect_range(*start, *raw_length)?;
                {
                    let state = self.after_loader_private_state_mut()?;
                    if state.callback_stack_protect_transition_is_exact(
                        range,
                        *raw_length,
                        *protection,
                    ) {
                        if !state.protect_callback_stack(range) {
                            return Err(Errno::EPROTO.into());
                        }
                    } else {
                        state.protect_owned_range(range, *protection);
                    }
                }
                if let Some((raw_path, image, pre_relro_geometry)) = loader_cache_alias {
                    let alias = config
                        .loader_cache_alias_for_path(raw_path.as_os_str().as_bytes())
                        .ok_or_else(|| {
                            self.caller_error(
                                "completed loader-cache alias RELRO left its bound profile",
                            )
                        })?;
                    let geometry = self.resolve_after_loader_image_geometry(
                        task,
                        alias.dependency(),
                        *image,
                        "loader-cache-alias-post-relro",
                    )?;
                    self.authenticate_after_loader_image_backing(
                        task,
                        alias.dependency(),
                        geometry,
                    )?;
                    let maps = guest_maps(task.pid()).ok_or(Errno::EPROTO)?;
                    if geometry != *pre_relro_geometry
                        || !target_loader_cache_alias_matches(task.pid(), alias)
                            .map_err(|error| self.caller_error(error))?
                        || !exact_image_mapping_paths_are_literal(
                            alias.dependency(),
                            geometry,
                            &maps,
                        )
                        || !self
                            .after_loader_private_state_mut()?
                            .loader_cache
                            .as_mut()
                            .and_then(|cache| cache.aliases.get_mut(raw_path))
                            .is_some_and(|lifecycle| lifecycle.complete_relro(*image, geometry))
                    {
                        return Err(self.caller_error(
                            "loader-cache alias RELRO completion changed its exact geometry",
                        ));
                    }
                }
            }
            AfterLoaderSyscallEffect::Remove { start, raw_length } => {
                let range = checked_page_effect_range(*start, *raw_length)?;
                self.after_loader_private_state_mut()?
                    .remove_owned_range(range);
            }
            AfterLoaderSyscallEffect::Resize { descriptor, length } => {
                let trampoline = match self
                    .after_loader_private_state()?
                    .owned_descriptors
                    .get(descriptor)
                    .ok_or(Errno::EPROTO)?
                {
                    AfterLoaderOwnedDescriptor::Trampoline {
                        id: Some(trampoline),
                        size: None,
                    } => *trampoline,
                    _ => {
                        return Err(self.caller_error("private ftruncate descriptor state changed"));
                    }
                };
                let metadata = std::fs::metadata(format!("/proc/{}/fd/{descriptor}", task.pid()))
                    .map_err(|error| self.caller_error(error))?;
                if metadata.len() != *length
                    || metadata.dev() != trampoline.file.device
                    || metadata.ino() != trampoline.file.inode
                {
                    return Err(self.caller_error("private ftruncate descriptor state changed"));
                }
                *self
                    .after_loader_private_state_mut()?
                    .owned_descriptors
                    .get_mut(descriptor)
                    .ok_or(Errno::EPROTO)? = AfterLoaderOwnedDescriptor::Trampoline {
                    id: Some(trampoline),
                    size: Some(*length),
                };
            }
            AfterLoaderSyscallEffect::AddTrampolineSeals { descriptor } => {
                if raw_result != 0
                    || !self
                        .after_loader_private_state_mut()?
                        .trampoline_seals_added
                        .insert(*descriptor)
                {
                    return Err(
                        self.caller_error("trampoline seal addition did not complete exactly once")
                    );
                }
            }
            AfterLoaderSyscallEffect::VerifyTrampolineSeals {
                descriptor,
                trampoline,
            } => {
                let state = self.after_loader_private_state_mut()?;
                if raw_result != i64::from(TRAMPOLINE_SEALS)
                    || !state.trampoline_seals_added.contains(descriptor)
                    || !matches!(
                        state.owned_descriptors.get(descriptor),
                        Some(AfterLoaderOwnedDescriptor::Trampoline {
                            id: Some(observed),
                            size: Some(TRAMPOLINE_ARENA_SIZE),
                        }) if observed == trampoline
                    )
                    || !state.sealed_trampolines.insert(*trampoline)
                {
                    return Err(self
                        .caller_error("trampoline seal verification did not match exact policy"));
                }
            }
            AfterLoaderSyscallEffect::Identity(expected) => {
                if raw_result != expected.as_raw() as i64 {
                    return Err(self.caller_error("private identity syscall returned another task"));
                }
            }
            AfterLoaderSyscallEffect::Read {
                descriptor,
                destination,
                offset,
                expected,
                advances,
            } => {
                let alias_read = matches!(
                    self.after_loader_private_state()?
                        .owned_descriptors
                        .get(descriptor),
                    Some(AfterLoaderOwnedDescriptor::LoaderCacheAlias { .. })
                );
                let amount = exact_read_prefix_length(raw_result, expected).ok_or_else(|| {
                    self.caller_error(
                        "private read returned zero before EOF or exceeded its exact bound",
                    )
                })?;
                if alias_read && (!*advances || amount != expected.len()) {
                    return Err(self.caller_error(
                        "loader-cache alias header read was short or did not advance exactly",
                    ));
                }
                let expected = &expected[..amount];
                let mut actual = vec![0_u8; amount];
                task.read_exact(*destination as usize, &mut actual)?;
                if actual.as_slice() != expected {
                    return Err(
                        self.caller_error("private read bytes differ from the bound input range")
                    );
                }
                let next = if *advances {
                    offset.checked_add(amount as u64).ok_or(Errno::EOVERFLOW)?
                } else {
                    self.after_loader_private_state()?
                        .owned_descriptors
                        .get(descriptor)
                        .and_then(AfterLoaderOwnedDescriptor::bytes_and_position)
                        .map(|(_, position)| position)
                        .ok_or(Errno::EPROTO)?
                };
                if descriptor_position(task.pid(), *descriptor)
                    .map_err(|error| self.caller_error(error))?
                    != next
                {
                    return Err(self.caller_error(
                        "private read changed the descriptor position unexpectedly",
                    ));
                }
                if *advances
                    && !self
                        .after_loader_private_state_mut()?
                        .owned_descriptors
                        .get_mut(descriptor)
                        .is_some_and(|owned| owned.set_position(next))
                {
                    return Err(
                        self.caller_error("private read lost its tracked descriptor position")
                    );
                }
                if alias_read {
                    let (raw_path, _) =
                        self.authenticate_loader_cache_alias_descriptor(task, config, *descriptor)?;
                    if !self
                        .after_loader_private_state_mut()?
                        .loader_cache
                        .as_mut()
                        .and_then(|cache| cache.aliases.get_mut(&raw_path))
                        .is_some_and(|lifecycle| {
                            lifecycle.complete_read(*descriptor, *offset, next)
                        })
                    {
                        return Err(self.caller_error(
                            "loader-cache alias read lifecycle changed before completion",
                        ));
                    }
                }
            }
            AfterLoaderSyscallEffect::RecordBreak => {
                if raw_result <= 0
                    || self
                        .after_loader_private_state_mut()?
                        .current_break
                        .replace(raw_result as u64)
                        .is_some()
                {
                    return Err(self
                        .caller_error("initial private brk query did not bind one current break"));
                }
            }
            AfterLoaderSyscallEffect::CheckBreak(expected) => {
                if raw_result as u64 != *expected {
                    return Err(self.caller_error("private brk query changed its bound value"));
                }
            }
            AfterLoaderSyscallEffect::AdvanceBreak {
                previous,
                requested,
                before_maps,
            } => {
                let after_maps = guest_maps(task.pid()).ok_or(Errno::EPROTO)?;
                if raw_result as u64 == *previous {
                    if after_maps != *before_maps {
                        return Err(
                            self.caller_error("failed private brk growth changed the target maps")
                        );
                    }
                    return Err(
                        self.caller_error("private brk growth returned the unchanged break")
                    );
                }
                if raw_result < 0 || raw_result as u64 != *requested {
                    return Err(self.caller_error(
                        "private brk growth failed instead of reaching its exact request",
                    ));
                }
                if !private_brk_maps_advance_exactly(
                    before_maps,
                    &after_maps,
                    *previous,
                    *requested,
                ) || !target_range_is_zero(task, *previous, *requested - *previous)?
                {
                    return Err(self.caller_error(
                        "private brk growth changed another VMA or exposed nonzero new bytes",
                    ));
                }
                let state = self.after_loader_private_state_mut()?;
                if !complete_private_brk_growth_state(
                    &mut state.current_break,
                    &mut state.private_brk_growth_consumed,
                    *previous,
                    *requested,
                    raw_result,
                ) {
                    return Err(self.caller_error(
                        "private brk growth completion lost its one-shot prior state",
                    ));
                }
            }
            AfterLoaderSyscallEffect::ExpectedResult(expected) => {
                if raw_result != *expected {
                    return Err(self
                        .caller_error("private metadata query returned a different exact result"));
                }
            }
            AfterLoaderSyscallEffect::FutexWake { address, word } => {
                let mut after = [0_u8; 4];
                task.read_exact(*address as usize, &mut after)?;
                if raw_result != 0 || after != *word {
                    return Err(self.caller_error(
                        "private FUTEX_WAKE_PRIVATE found a participant or changed its word",
                    ));
                }
            }
            AfterLoaderSyscallEffect::StatLoaderCacheAlias {
                descriptor,
                destination,
                fields,
            } => {
                if raw_result != 0 {
                    return Err(self.caller_error("loader-cache alias fstat did not return zero"));
                }
                let (raw_path, _) =
                    self.authenticate_loader_cache_alias_descriptor(task, config, *descriptor)?;
                let metadata = std::fs::metadata(format!("/proc/{}/fd/{descriptor}", task.pid()))
                    .map_err(|error| self.caller_error(error))?;
                if AfterLoaderStatFields::deterministic_loader_cache_alias(&metadata) != *fields {
                    return Err(self.caller_error(
                        "loader-cache alias fstat target changed while the syscall executed",
                    ));
                }
                let expected = fields.exact_x86_64_output();
                let mut observed = [0_u8; 144];
                task.read_exact(*destination as usize, &mut observed)?;
                observed[72..88].copy_from_slice(&expected[72..88]);
                if observed != expected {
                    return Err(self.caller_error(
                        "loader-cache alias fstat non-atime output differs from exact metadata",
                    ));
                }
                self.caller_write(task, *destination, &expected)?;
                self.caller_observe(
                    "loader-cache alias fstat virtualized",
                    "policy=st_atime-equals-st_mtime kernel-non-atime-fields=byte-exact",
                )?;
                if !self
                    .after_loader_private_state_mut()?
                    .loader_cache
                    .as_mut()
                    .and_then(|cache| cache.aliases.get_mut(&raw_path))
                    .is_some_and(|lifecycle| lifecycle.complete_stat(*descriptor))
                {
                    return Err(self.caller_error(
                        "loader-cache alias fstat lifecycle changed before completion",
                    ));
                }
            }
            AfterLoaderSyscallEffect::Stat {
                descriptor,
                destination,
                fields,
            } => {
                if raw_result != 0 {
                    return Err(self.caller_error("private stat did not return zero"));
                }
                let alias_binding = match self
                    .after_loader_private_state()?
                    .owned_descriptors
                    .get(descriptor)
                {
                    Some(AfterLoaderOwnedDescriptor::LoaderCacheAlias { .. }) => Some(
                        self.authenticate_loader_cache_alias_descriptor(task, config, *descriptor)?,
                    ),
                    _ => None,
                };
                let metadata = std::fs::metadata(format!("/proc/{}/fd/{descriptor}", task.pid()))
                    .map_err(|error| self.caller_error(error))?;
                if AfterLoaderStatFields::from_metadata(&metadata) != *fields {
                    return Err(
                        self.caller_error("private stat target changed while the syscall executed")
                    );
                }
                let mut bytes = [0_u8; 144];
                task.read_exact(*destination as usize, &mut bytes)?;
                if !stat_output_matches(*fields, &bytes) {
                    return Err(self.caller_error(
                        "private stat output differs from the exact owned descriptor metadata",
                    ));
                }
                if let Some((raw_path, _)) = alias_binding
                    && !self
                        .after_loader_private_state_mut()?
                        .loader_cache
                        .as_mut()
                        .and_then(|cache| cache.aliases.get_mut(&raw_path))
                        .is_some_and(|lifecycle| lifecycle.complete_stat(*descriptor))
                {
                    return Err(self.caller_error(
                        "loader-cache alias fstat lifecycle changed before completion",
                    ));
                }
            }
            AfterLoaderSyscallEffect::Statx {
                descriptor,
                destination,
                expected,
            } => {
                if raw_result != 0 {
                    return Err(self.caller_error("private statx did not return zero"));
                }
                let state = self.after_loader_private_state()?;
                let (device, inode, opened, position) = match state
                    .owned_descriptors
                    .get(descriptor)
                {
                    Some(AfterLoaderOwnedDescriptor::ProcMaps {
                        device,
                        inode,
                        bytes,
                        position,
                    }) => (*device, *inode, bytes, *position),
                    _ => {
                        return Err(self.caller_error("private statx descriptor authority changed"));
                    }
                };
                let proc_path = format!("/proc/{}/maps", task.pid());
                let metadata =
                    std::fs::metadata(&proc_path).map_err(|error| self.caller_error(error))?;
                let current = bounded_proc(&proc_path, 2 * 1024 * 1024)
                    .map_err(|error| self.caller_error(error))?;
                if metadata.dev() != device
                    || metadata.ino() != inode
                    || current.as_slice() != opened.as_ref()
                    || descriptor_position(task.pid(), *descriptor)
                        .map_err(|error| self.caller_error(error))?
                        != position
                    || descriptor_statx_bytes(task.pid(), *descriptor)
                        .map_err(|error| self.caller_error(error))?
                        != *expected
                {
                    return Err(self.caller_error(
                        "private statx target identity, bytes, position or metadata changed",
                    ));
                }
                let mut actual = [0_u8; STATX_OUTPUT_BYTES];
                task.read_exact(*destination as usize, &mut actual)?;
                if actual != *expected {
                    return Err(
                        self.caller_error("private statx output differs from its exact snapshot")
                    );
                }
            }
        }
        Ok(AfterLoaderSyscallCompletion::KernelResult(raw_result))
    }

    fn after_loader_image_geometry_matches(
        &self,
        task: &Stopped,
        image: &LiteinstCallerImage,
        identity: MappingIdentity,
        elf: &Elf<'_>,
        bias: u64,
        maps: &[GuestMap],
        isolation: Option<&AfterLoaderHelperIsolation>,
        relro_state: ImageRelroState,
    ) -> Result<std::result::Result<(), ImageGeometryMismatch>, Error> {
        let isolation = isolation.filter(|isolation| isolation.applies_to(identity, bias));
        let loads = elf
            .program_headers
            .iter()
            .filter(|header| header.p_type == ph::PT_LOAD)
            .collect::<Vec<_>>();
        let first = loads
            .iter()
            .map(|header| page_down(header.p_vaddr))
            .min()
            .ok_or_else(|| self.caller_error("bound ELF has no PT_LOAD"))?;
        let first_load = loads
            .iter()
            .copied()
            .min_by_key(|header| page_down(header.p_vaddr))
            .ok_or_else(|| self.caller_error("bound ELF has no first PT_LOAD"))?;
        let last = loads
            .iter()
            .map(|header| {
                header
                    .p_vaddr
                    .checked_add(header.p_memsz)
                    .ok_or(Errno::EOVERFLOW)
                    .and_then(page_up)
            })
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .max()
            .ok_or(Errno::EPROTO)?;
        let target_first = bias.checked_add(first).ok_or(Errno::EOVERFLOW)?;
        let target_last = bias.checked_add(last).ok_or(Errno::EOVERFLOW)?;
        if maps.iter().any(|mapping| {
            mapping.mapping_identity() == identity
                && (mapping.start < target_first || target_last < mapping.end)
        }) {
            return Ok(Err(ImageGeometryMismatch::MappingOutsideImageSpan));
        }
        let relro = elf
            .program_headers
            .iter()
            .filter(|header| header.p_type == ph::PT_GNU_RELRO)
            .map(|header| {
                let start = page_down(header.p_vaddr);
                let end = header
                    .p_vaddr
                    .checked_add(header.p_memsz)
                    .ok_or(Errno::EOVERFLOW)
                    .and_then(page_up)?;
                Ok((start, end))
            })
            .collect::<Result<Vec<_>, Errno>>()?;
        if relro.len() > 1 {
            return Err(self.caller_error("bound ELF has multiple PT_GNU_RELRO ranges"));
        }
        let mut isolated_page_seen = false;
        let mut page = first;
        while page < last {
            let owner = loads.iter().copied().rfind(|header| {
                let start = page_down(header.p_vaddr);
                header
                    .p_vaddr
                    .checked_add(header.p_memsz)
                    .and_then(|end| page_up(end).ok())
                    .is_some_and(|end| start <= page && page < end)
            });
            let target = bias.checked_add(page).ok_or(Errno::EOVERFLOW)?;
            let isolated_page = isolation.is_some_and(|isolation| {
                isolation.range.start == target
                    && target.checked_add(PAGE) == Some(isolation.range.end)
            });
            let mapping = maps
                .iter()
                .find(|mapping| mapping.start <= target && target < mapping.end);
            let Some(mapping) = mapping else {
                if elf.header.e_type == header::ET_DYN || owner.is_some() {
                    return Ok(Err(ImageGeometryMismatch::MissingPage));
                }
                page = page.checked_add(PAGE).ok_or(Errno::EOVERFLOW)?;
                continue;
            };
            if mapping.shared {
                return Ok(Err(ImageGeometryMismatch::SharedPage));
            }
            let relro_page = relro.iter().any(|range| range.0 <= page && page < range.1);
            if let Some(owner) = owner {
                let owner_start = page_down(owner.p_vaddr);
                let file_end = owner
                    .p_vaddr
                    .checked_add(owner.p_filesz)
                    .ok_or(Errno::EOVERFLOW)
                    .and_then(page_up)?;
                let readable = owner.p_flags & ph::PF_R != 0;
                let writable = owner.p_flags & ph::PF_W != 0
                    && (!relro_page || relro_state == ImageRelroState::WritableBeforeProtection);
                let executable = owner.p_flags & ph::PF_X != 0;
                let expected_offset = page_down(owner.p_offset)
                    .checked_add(page - owner_start)
                    .ok_or(Errno::EOVERFLOW)?;
                if isolated_page {
                    let isolation = isolation.ok_or(Errno::EPROTO)?;
                    if !readable
                        || writable
                        || !executable
                        || page >= file_end
                        || isolation.file_offset != expected_offset
                        || !isolation.validates_mapping(task, mapping)
                    {
                        return Ok(Err(ImageGeometryMismatch::HelperIsolation));
                    }
                    isolated_page_seen = true;
                } else {
                    if mapping.readable != readable
                        || mapping.writable != writable
                        || mapping.executable != executable
                    {
                        return Ok(Err(ImageGeometryMismatch::LoadPermissions));
                    }
                    if page < file_end {
                        if mapping.mapping_identity() != identity {
                            return Ok(Err(ImageGeometryMismatch::LoadIdentity));
                        }
                        if mapping.offset.checked_add(target - mapping.start)
                            != Some(expected_offset)
                        {
                            return Ok(Err(ImageGeometryMismatch::LoadOffset));
                        }
                    } else if mapping.inode != 0 || mapping.path.is_some() {
                        return Ok(Err(ImageGeometryMismatch::AnonymousBssBacking));
                    }
                }
            } else {
                if isolated_page {
                    return Ok(Err(ImageGeometryMismatch::HelperIsolation));
                }
                if mapping.readable || mapping.writable || mapping.executable {
                    return Ok(Err(ImageGeometryMismatch::HolePermissions));
                }
                if mapping.mapping_identity() != identity {
                    return Ok(Err(ImageGeometryMismatch::HoleIdentity));
                }
                let expected_offset = page_down(first_load.p_offset)
                    .checked_add(page - first)
                    .ok_or(Errno::EOVERFLOW)?;
                if mapping.offset.checked_add(target - mapping.start) != Some(expected_offset) {
                    return Ok(Err(ImageGeometryMismatch::HoleOffset));
                }
            }
            page = page.checked_add(PAGE).ok_or(Errno::EOVERFLOW)?;
        }
        if isolation.is_some() && !isolated_page_seen {
            return Ok(Err(ImageGeometryMismatch::HelperIsolation));
        }

        for load in loads {
            let file_end = load
                .p_offset
                .checked_add(load.p_filesz)
                .ok_or(Errno::EOVERFLOW)?;
            if load.p_filesz > load.p_memsz
                || file_end > image.bytes.len() as u64
                || load.p_flags & !7 != 0
                || load.p_align > 1
                    && (!load.p_align.is_power_of_two()
                        || load.p_vaddr % load.p_align != load.p_offset % load.p_align)
            {
                return Err(self.caller_error("bound ELF PT_LOAD geometry is invalid"));
            }
            if load.p_flags & ph::PF_W == 0 && load.p_filesz != 0 {
                let mut target = bias.checked_add(load.p_vaddr).ok_or(Errno::EOVERFLOW)?;
                let mut offset = usize::try_from(load.p_offset).map_err(|_| Errno::EOVERFLOW)?;
                let end = usize::try_from(file_end).map_err(|_| Errno::EOVERFLOW)?;
                let mut observed = [0_u8; 65536];
                while offset < end {
                    let amount = (end - offset).min(observed.len());
                    read_exact_with_helper_isolation(
                        task,
                        target,
                        &mut observed[..amount],
                        isolation,
                    )?;
                    if !exact_geometry_bytes_match(
                        &observed[..amount],
                        &image.bytes[offset..offset + amount],
                    ) {
                        return Ok(Err(ImageGeometryMismatch::NonwritableBytes {
                            file_offset: offset as u64,
                        }));
                    }
                    offset += amount;
                    target = target.checked_add(amount as u64).ok_or(Errno::EOVERFLOW)?;
                }
            }
        }
        Ok(Ok(()))
    }

    fn resolve_after_loader_image_geometry(
        &self,
        task: &Stopped,
        image: &LiteinstCallerImage,
        image_id: AfterLoaderImageId,
        phase: &'static str,
    ) -> Result<ResolvedImageGeometry, Error> {
        self.resolve_after_loader_image_geometry_with_isolation(task, image, image_id, phase, None)
    }

    fn resolve_after_loader_image_geometry_with_isolation(
        &self,
        task: &Stopped,
        image: &LiteinstCallerImage,
        image_id: AfterLoaderImageId,
        phase: &'static str,
        isolation: Option<&AfterLoaderHelperIsolation>,
    ) -> Result<ResolvedImageGeometry, Error> {
        self.resolve_after_loader_image_geometry_with_isolation_and_relro(
            task,
            image,
            image_id,
            phase,
            isolation,
            ImageRelroState::Protected,
        )
    }

    fn resolve_loader_cache_alias_pre_relro_geometry(
        &self,
        task: &Stopped,
        image: &LiteinstCallerImage,
        image_id: AfterLoaderImageId,
    ) -> Result<ResolvedImageGeometry, Error> {
        self.resolve_after_loader_image_geometry_with_isolation_and_relro(
            task,
            image,
            image_id,
            "loader-cache-alias-pre-relro",
            None,
            ImageRelroState::WritableBeforeProtection,
        )
    }

    fn resolve_after_loader_image_geometry_with_isolation_and_relro(
        &self,
        task: &Stopped,
        image: &LiteinstCallerImage,
        image_id: AfterLoaderImageId,
        phase: &'static str,
        isolation: Option<&AfterLoaderHelperIsolation>,
        relro_state: ImageRelroState,
    ) -> Result<ResolvedImageGeometry, Error> {
        let elf = Elf::parse(&image.bytes)
            .map_err(|error| self.caller_error(format!("bound ELF parse failed: {error}")))?;
        if elf.is_64 == false
            || elf.little_endian == false
            || elf.header.e_machine != header::EM_X86_64
            || !matches!(elf.header.e_type, header::ET_DYN | header::ET_EXEC)
            || elf.program_headers.len() > 128
        {
            return Err(self.caller_error("bound ELF header is outside the fixed x86-64 graph"));
        }
        let loads = elf
            .program_headers
            .iter()
            .filter(|program| program.p_type == ph::PT_LOAD)
            .collect::<Vec<_>>();
        if loads.is_empty() {
            return Err(self.caller_error("bound ELF has no PT_LOAD"));
        }
        for (index, load) in loads.iter().enumerate() {
            let end = load
                .p_vaddr
                .checked_add(load.p_memsz)
                .ok_or(Errno::EOVERFLOW)?;
            if loads[..index].iter().any(|prior| {
                prior
                    .p_vaddr
                    .checked_add(prior.p_memsz)
                    .is_none_or(|prior_end| load.p_vaddr < prior_end && prior.p_vaddr < end)
            }) {
                return Err(self.caller_error("bound ELF PT_LOAD memory ranges overlap"));
            }
        }
        for relro in elf
            .program_headers
            .iter()
            .filter(|program| program.p_type == ph::PT_GNU_RELRO)
        {
            let end = relro
                .p_vaddr
                .checked_add(relro.p_memsz)
                .ok_or(Errno::EOVERFLOW)?;
            if relro.p_memsz == 0
                || !loads.iter().any(|load| {
                    load.p_flags & ph::PF_W != 0
                        && load.p_vaddr <= relro.p_vaddr
                        && load
                            .p_vaddr
                            .checked_add(load.p_memsz)
                            .is_some_and(|load_end| end <= load_end)
                })
            {
                return Err(
                    self.caller_error("bound ELF PT_GNU_RELRO is outside one writable PT_LOAD")
                );
            }
        }
        let maps = guest_maps(task.pid()).ok_or(Errno::EPROTO)?;
        let mut candidates = BTreeSet::new();
        for mapping in maps.iter().filter(|mapping| mapping.inode != 0) {
            for load in &loads {
                let file_start = page_down(load.p_offset);
                let file_end = load
                    .p_offset
                    .checked_add(load.p_filesz)
                    .ok_or(Errno::EOVERFLOW)
                    .and_then(page_up)?;
                if file_start <= mapping.offset && mapping.offset < file_end {
                    let relative = page_down(load.p_vaddr)
                        .checked_add(mapping.offset - file_start)
                        .ok_or(Errno::EOVERFLOW)?;
                    if let Some(bias) = mapping.start.checked_sub(relative) {
                        candidates.insert((mapping.mapping_identity(), bias));
                    }
                }
            }
        }
        if elf.header.e_type == header::ET_EXEC {
            candidates.retain(|(_, bias)| *bias == 0);
        }
        let candidate_count = candidates.len();
        let mut accepted = Vec::new();
        let mut first_rejection = None;
        for (identity, bias) in candidates {
            match self.after_loader_image_geometry_matches(
                task,
                image,
                identity,
                &elf,
                bias,
                &maps,
                isolation,
                relro_state,
            )? {
                Ok(()) => accepted.push((identity, bias)),
                Err(reason) if first_rejection.is_none() => {
                    first_rejection = Some(format!(
                        "mapping={:x}:{:x}:{} bias={bias:#x} reason={reason:?}",
                        identity.device_major, identity.device_minor, identity.inode,
                    ));
                }
                Err(_) => {}
            }
        }
        let Some((mapping, load_bias)) = one_exact_geometry_candidate(&accepted) else {
            return Err(self.caller_error(format!(
                "phase={phase} bound image path={} file_device={} file_inode={} has {} exact retained load geometries among {} map-derived candidates; first_rejection={}",
                image.path.display(),
                image.file_identity.device,
                image.file_identity.inode,
                accepted.len(),
                candidate_count,
                first_rejection.as_deref().unwrap_or("none"),
            )));
        };
        let first = loads
            .iter()
            .map(|load| page_down(load.p_vaddr))
            .min()
            .ok_or(Errno::EPROTO)?;
        let last = loads
            .iter()
            .map(|load| {
                load.p_vaddr
                    .checked_add(load.p_memsz)
                    .ok_or(Errno::EOVERFLOW)
                    .and_then(page_up)
            })
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .max()
            .ok_or(Errno::EPROTO)?;
        let target_first = load_bias.checked_add(first).ok_or(Errno::EOVERFLOW)?;
        let target_last = load_bias.checked_add(last).ok_or(Errno::EOVERFLOW)?;
        if self
            .after_loader_private_state()?
            .owned_mappings
            .iter()
            .any(|mapping| {
                matches!(
                    mapping.purpose,
                    AfterLoaderMappingPurpose::Image { image: mapped }
                        | AfterLoaderMappingPurpose::ImageZeroFill { image: mapped }
                        if mapped == image_id
                ) && (mapping.start < target_first || target_last < mapping.end)
            })
        {
            return Err(self.caller_error(
                "owned image mapping extends outside its exact PT_LOAD reservation",
            ));
        }
        Ok(ResolvedImageGeometry {
            image: image_id,
            mapping,
            load_bias,
            span: (target_first, target_last),
        })
    }

    fn authenticate_after_loader_image_backing(
        &self,
        task: &Stopped,
        image: &LiteinstCallerImage,
        geometry: ResolvedImageGeometry,
    ) -> Result<(), Error> {
        let maps = guest_maps(task.pid()).ok_or(Errno::EPROTO)?;
        if !exact_image_backing_paths_match(task.pid(), image, geometry, &maps)
            .map_err(|error| self.caller_error(error))?
        {
            return Err(self.caller_error(
                "target backing path bytes, identity or maps-domain bridge differs",
            ));
        }
        Ok(())
    }

    fn bind_after_loader_initial_image_geometries(
        &mut self,
        task: &Stopped,
        config: &LiteinstAfterLoaderConfig,
    ) -> Result<(), Error> {
        self.require_after_loader_deferred_images_absent(task, config)?;
        self.bind_after_loader_image_geometries(
            task,
            std::iter::once(&config.executable)
                .chain(std::iter::once(&config.provider))
                .chain(config.initial_dependencies.iter())
                .collect(),
            "initial-entry",
            "bound target image geometry",
            false,
        )
    }

    fn bind_after_loader_deferred_image_geometries(
        &mut self,
        task: &Stopped,
        config: &LiteinstAfterLoaderConfig,
    ) -> Result<(), Error> {
        self.bind_after_loader_image_geometries(
            task,
            config.deferred_dependencies.iter().collect(),
            "post-dlopen-deferred",
            "bound target image geometry",
            true,
        )?;
        let maps = guest_maps(task.pid()).ok_or(Errno::EPROTO)?;
        for alias in config.loader_cache_aliases() {
            let plan = loader_cache_alias_mmap_plan(&alias.dependency().bytes)
                .map_err(|reason| self.caller_error(reason))?;
            let state = self.after_loader_private_state()?;
            let image = state.image_id(alias.dependency());
            let geometry = state.image_geometry(alias.dependency()).ok_or_else(|| {
                self.caller_error("cache-derived deferred image geometry was not bound")
            })?;
            let consumed = state
                .loader_cache
                .as_ref()
                .and_then(|cache| cache.aliases.get(alias.raw_path()))
                .is_some_and(|lifecycle| {
                    matches!(
                        lifecycle,
                        LoaderCacheAliasLifecycle::Consumed {
                            image: consumed_image,
                            geometry: consumed_geometry,
                            mappings,
                            zero_fills,
                        } if *consumed_image == image
                            && *consumed_geometry == geometry
                            && mappings.len() == plan.file_maps.len()
                            && zero_fills.len() == 1
                    )
                });
            if !consumed
                || !target_loader_cache_alias_matches(task.pid(), alias)
                    .map_err(|error| self.caller_error(error))?
                || !exact_image_mapping_paths_are_literal(alias.dependency(), geometry, &maps)
            {
                return Err(self.caller_error(
                    "cache-derived deferred image lost its consumed canonical-path geometry",
                ));
            }
        }
        Ok(())
    }

    fn bind_after_loader_image_geometries<'a>(
        &mut self,
        task: &Stopped,
        selected: Vec<&'a LiteinstCallerImage>,
        phase: &'static str,
        operation: &'static str,
        require_causal_mapping: bool,
    ) -> Result<(), Error> {
        let mut images = BTreeMap::<crate::after_loader::FileIdentity, &LiteinstCallerImage>::new();
        for image in selected {
            if let Some(previous) = images.insert(image.file_identity, image)
                && previous.bytes != image.bytes
            {
                return Err(
                    self.caller_error("bound graph reuses a file identity for different bytes")
                );
            }
        }
        let mut geometries = Vec::with_capacity(images.len());
        for image in images.values() {
            let image_id = self.after_loader_private_state()?.image_id(image);
            let geometry =
                self.resolve_after_loader_image_geometry(task, image, image_id, phase)?;
            self.authenticate_after_loader_image_backing(task, image, geometry)?;
            if require_causal_mapping
                && !self
                    .after_loader_private_state()?
                    .has_causal_image_mapping(geometry)
            {
                return Err(self.caller_error(
                    "deferred image geometry lacked its exact loader-mmap identity",
                ));
            }
            let soname = image
                .dynamic_soname()
                .unwrap_or_else(|| "<executable>".to_owned());
            self.caller_observe(
                "resolved target image geometry",
                format!(
                    "phase={phase} soname={soname} path={} file_device={} file_inode={} mapping_device={:x}:{:x} mapping_inode={} load_bias={:#x} span={:#x}-{:#x}",
                    image.path.display(),
                    image.file_identity.device,
                    image.file_identity.inode,
                    geometry.mapping.device_major,
                    geometry.mapping.device_minor,
                    geometry.mapping.inode,
                    geometry.load_bias,
                    geometry.span.0,
                    geometry.span.1,
                ),
            )?;
            geometries.push((*image, geometry));
        }
        for (_, geometry) in &geometries {
            if !self
                .after_loader_private_state_mut()?
                .bind_image_geometry(*geometry)
            {
                return Err(self.caller_error(
                    "bound graph mapping identities collide or changed while binding",
                ));
            }
        }
        for (image, geometry) in geometries {
            let soname = image
                .dynamic_soname()
                .unwrap_or_else(|| "<executable>".to_owned());
            self.caller_observe(
                operation,
                format!(
                    "phase={phase} soname={soname} path={} file_device={} file_inode={} mapping_device={:x}:{:x} mapping_inode={} load_bias={:#x} span={:#x}-{:#x}",
                    image.path.display(),
                    image.file_identity.device,
                    image.file_identity.inode,
                    geometry.mapping.device_major,
                    geometry.mapping.device_minor,
                    geometry.mapping.inode,
                    geometry.load_bias,
                    geometry.span.0,
                    geometry.span.1,
                ),
            )?;
        }
        Ok(())
    }

    fn require_after_loader_deferred_images_absent(
        &mut self,
        task: &Stopped,
        config: &LiteinstAfterLoaderConfig,
    ) -> Result<(), Error> {
        let maps = guest_maps(task.pid()).ok_or(Errno::EPROTO)?;
        for image in &config.deferred_dependencies {
            let soname = image
                .dynamic_soname()
                .ok_or_else(|| self.caller_error("deferred loader image lost its DT_SONAME"))?;
            let identity = target_bound_image_mapping_identity(task.pid(), image)
                .map_err(|error| self.caller_error(error))?
                .ok_or_else(|| {
                    self.caller_error(
                        "deferred graph backing bytes or file identity changed before dlopen",
                    )
                })?;
            if maps
                .iter()
                .any(|mapping| mapping.mapping_identity() == identity)
            {
                return Err(self.caller_error(format!(
                    "runtime-only dependency was already mapped before dlopen: soname={soname} path={} file_device={} file_inode={} mapping_device={:x}:{:x} mapping_inode={}",
                    image.path.display(),
                    image.file_identity.device,
                    image.file_identity.inode,
                    identity.device_major,
                    identity.device_minor,
                    identity.inode,
                )));
            }
            self.caller_observe(
                "deferred target image absent before dlopen",
                format!(
                    "soname={soname} path={} file_device={} file_inode={} mapping_device={:x}:{:x} mapping_inode={}",
                    image.path.display(),
                    image.file_identity.device,
                    image.file_identity.inode,
                    identity.device_major,
                    identity.device_minor,
                    identity.inode,
                ),
            )?;
        }
        Ok(())
    }

    fn validate_after_loader_retained_resources(
        &self,
        task: &Stopped,
        config: &LiteinstAfterLoaderConfig,
        isolation: Option<&AfterLoaderHelperIsolation>,
    ) -> Result<(), Error> {
        let state = self.after_loader_private_state()?;
        if isolation.is_some_and(|isolation| !isolation.validates_live_page(task)) {
            return Err(self.caller_error(
                "isolated helper page lost its exact VMA, pkey, backing or PTRACE bytes",
            ));
        }
        if !state.owned_descriptors.is_empty() || state.current_break.is_none() {
            return Err(self.caller_error(
                "private descriptors survived or the current break was never bound",
            ));
        }
        if state
            .owned_mappings
            .iter()
            .any(|mapping| matches!(mapping.purpose, AfterLoaderMappingPurpose::Controller))
        {
            return Err(self.caller_error("controller scratch mapping survived restoration"));
        }
        let callback_stack = state.callback_stack_range().ok_or_else(|| {
            self.caller_error("retained callback stack lacks exact guard/RW/guard geometry")
        })?;
        let frame =
            self.liteinst_runtime.lock().unwrap().frame.ok_or_else(|| {
                self.caller_error("retained callback stack has no handshake frame")
            })?;
        if liteinst_callback_stack_range(frame) != Some(callback_stack) {
            return Err(self
                .caller_error("retained callback stack differs from the authenticated handshake"));
        }
        let maps = guest_maps(task.pid()).ok_or(Errno::EPROTO)?;
        for alias in config.loader_cache_aliases() {
            let plan = loader_cache_alias_mmap_plan(&alias.dependency().bytes)
                .map_err(|reason| self.caller_error(reason))?;
            let image = state.image_id(alias.dependency());
            let geometry = state.image_geometry(alias.dependency()).ok_or_else(|| {
                self.caller_error("retained cache-derived image geometry disappeared")
            })?;
            let consumed = state
                .loader_cache
                .as_ref()
                .and_then(|cache| cache.aliases.get(alias.raw_path()))
                .is_some_and(|lifecycle| {
                    matches!(
                        lifecycle,
                        LoaderCacheAliasLifecycle::Consumed {
                            image: consumed_image,
                            geometry: consumed_geometry,
                            mappings,
                            zero_fills,
                        } if *consumed_image == image
                            && *consumed_geometry == geometry
                            && mappings.len() == plan.file_maps.len()
                            && zero_fills.len() == 1
                    )
                });
            if !consumed
                || !target_loader_cache_alias_matches(task.pid(), alias)
                    .map_err(|error| self.caller_error(error))?
                || !exact_image_mapping_paths_are_literal(alias.dependency(), geometry, &maps)
            {
                return Err(self.caller_error(
                    "retained cache-derived image lost its exact source or canonical path",
                ));
            }
            self.authenticate_after_loader_image_backing(task, alias.dependency(), geometry)?;
        }
        for owned in &state.owned_mappings {
            let exact_boundaries = matches!(
                owned.purpose,
                AfterLoaderMappingPurpose::CallbackStack
                    | AfterLoaderMappingPurpose::SharedReservation { .. }
                    | AfterLoaderMappingPurpose::Trampoline { .. }
            );
            let mapped = maps.iter().find(|mapping| {
                (if exact_boundaries {
                    mapping.start == owned.start && mapping.end == owned.end
                } else {
                    mapping.start <= owned.start && owned.end <= mapping.end
                }) && mapping.readable == owned.readable
                    && mapping.writable == owned.writable
                    && mapping.executable == owned.executable
                    && mapping.shared == owned.shared
            });
            let Some(mapped) = mapped else {
                return Err(self.caller_error("retained owned mapping geometry changed"));
            };
            match owned.purpose {
                AfterLoaderMappingPurpose::Image { image } => {
                    if state.image_mappings.get(&image).copied() != Some(mapped.mapping_identity())
                    {
                        return Err(self.caller_error("retained image mapping identity changed"));
                    }
                }
                AfterLoaderMappingPurpose::ImageZeroFill { .. } => {
                    if mapped.inode != 0 || mapped.path.is_some() {
                        return Err(
                            self.caller_error("retained anonymous mapping acquired file identity")
                        );
                    }
                }
                AfterLoaderMappingPurpose::CallbackStack => {
                    if mapped.offset != 0 || mapped.inode != 0 || mapped.path.is_some() {
                        return Err(self.caller_error(
                            "retained callback stack acquired file identity or nonzero offset",
                        ));
                    }
                }
                AfterLoaderMappingPurpose::SharedReservation { trampoline } => {
                    let Some(identity) = exact_shared_reservation_identity(mapped) else {
                        return Err(self.caller_error(
                            "retained shared reservation lost its exact Linux identity",
                        ));
                    };
                    if state.shared_reservations.get(&trampoline)
                        != Some(&((owned.start, owned.end), identity))
                        || owned.end - owned.start != PAGE
                        || !owned.readable
                        || !owned.writable
                        || owned.executable
                        || !owned.shared
                        || owned.descriptor.is_some()
                        || owned.offset != 0
                    {
                        return Err(self.caller_error(
                            "retained shared reservation identity or geometry changed",
                        ));
                    }
                }
                AfterLoaderMappingPurpose::Trampoline { trampoline } => {
                    if state.trampoline_mapping(trampoline) != Some(mapped.mapping_identity())
                        || mapped.offset != 0
                        || owned.end - owned.start != TRAMPOLINE_ARENA_SIZE
                    {
                        return Err(self.caller_error(
                            "retained trampoline alias identity or geometry changed",
                        ));
                    }
                }
                AfterLoaderMappingPurpose::LoaderCache => {
                    return Err(self.caller_error(
                        "loader-cache mapping survived into retained runtime resources",
                    ));
                }
                AfterLoaderMappingPurpose::Controller => unreachable!(),
            }
        }
        let mut trampoline_identities = BTreeSet::new();
        for mapping in &state.owned_mappings {
            if let AfterLoaderMappingPurpose::Trampoline { trampoline } = mapping.purpose {
                trampoline_identities.insert(trampoline);
            }
        }
        if trampoline_identities.is_empty() {
            return Err(self.caller_error("initializer retained no bound trampoline arena"));
        }
        let reservation_identities = state
            .shared_reservations
            .keys()
            .copied()
            .collect::<BTreeSet<_>>();
        if reservation_identities != trampoline_identities {
            return Err(self.caller_error(
                "retained trampoline aliases and shared reservations are not bijective",
            ));
        }
        if state.sealed_trampolines != trampoline_identities {
            return Err(self
                .caller_error("retained trampoline aliases and verified seals are not bijective"));
        }
        for trampoline in trampoline_identities {
            let aliases = state
                .owned_mappings
                .iter()
                .filter(|mapping| {
                    mapping.purpose == AfterLoaderMappingPurpose::Trampoline { trampoline }
                })
                .collect::<Vec<_>>();
            let reservations = state
                .owned_mappings
                .iter()
                .filter(|mapping| {
                    mapping.purpose == (AfterLoaderMappingPurpose::SharedReservation { trampoline })
                })
                .collect::<Vec<_>>();
            if aliases.len() != 2
                || aliases.iter().filter(|mapping| mapping.writable).count() != 1
                || aliases.iter().filter(|mapping| mapping.executable).count() != 1
                || aliases.iter().any(|mapping| {
                    !mapping.shared
                        || mapping.offset != 0
                        || mapping.end - mapping.start != TRAMPOLINE_ARENA_SIZE
                })
                || ranges_overlap(
                    (aliases[0].start, aliases[0].end),
                    (aliases[1].start, aliases[1].end),
                )
                || reservations.len() != 1
                || ranges_overlap(
                    (aliases[0].start, aliases[0].end),
                    (reservations[0].start, reservations[0].end),
                )
                || ranges_overlap(
                    (aliases[1].start, aliases[1].end),
                    (reservations[0].start, reservations[0].end),
                )
            {
                return Err(self.caller_error("retained trampoline resource set changed"));
            }
        }

        let mut images = BTreeMap::<crate::after_loader::FileIdentity, &LiteinstCallerImage>::new();
        for image in std::iter::once(&config.executable)
            .chain(std::iter::once(&config.provider))
            .chain(config.dependencies.iter())
        {
            if let Some(previous) = images.insert(image.file_identity, image)
                && previous.bytes != image.bytes
            {
                return Err(self.caller_error("bound graph reuses an identity for different bytes"));
            }
        }
        if let Some(previous) = images.insert(
            config.sealed_runtime.image.file_identity,
            &config.sealed_runtime.image,
        ) && previous.bytes != config.sealed_runtime.image.bytes
        {
            return Err(
                self.caller_error("sealed runtime identity is reused for different bound bytes")
            );
        }
        for image in images.into_values() {
            let image_id = state.image_id(image);
            let image_isolation = isolation.filter(|isolation| isolation.image == image_id);
            let geometry = self.resolve_after_loader_image_geometry_with_isolation(
                task,
                image,
                image_id,
                "final-retained",
                image_isolation,
            )?;
            if state.image_geometry(image) != Some(geometry) {
                return Err(self.caller_error(
                    "bound image mapping identity or exact retained geometry changed",
                ));
            }
            if image_id != state.image_id(&config.sealed_runtime.image) {
                self.authenticate_after_loader_image_backing(task, image, geometry)?;
            }
        }
        let runtime = state.image_id(&config.sealed_runtime.image);
        if isolation.is_some_and(|isolation| isolation.image != runtime) {
            return Err(
                self.caller_error("isolated helper authority names a different retained image")
            );
        }
        if !state.owned_mappings.iter().any(|mapping| {
            matches!(
                mapping.purpose,
                AfterLoaderMappingPurpose::Image { image }
                    | AfterLoaderMappingPurpose::ImageZeroFill { image }
                    if image == runtime
            )
        }) {
            return Err(self.caller_error("sealed runtime retained no owned load mapping"));
        }
        Ok(())
    }

    pub(super) fn arm_after_loader_syscall_permit(
        &mut self,
        task: &Stopped,
        purpose: AfterLoaderSyscallPurpose,
        number: i64,
        args: [u64; 6],
        instruction_pointer: u64,
    ) -> Result<(), TraceError> {
        self.arm_after_loader_syscall_permit_with_instruction(
            task,
            purpose,
            number,
            args,
            instruction_pointer,
            [0x0f, 0x05, 0x0f, 0x0b],
        )
    }

    pub(super) fn arm_after_loader_syscall_permit_with_instruction(
        &mut self,
        task: &Stopped,
        purpose: AfterLoaderSyscallPurpose,
        number: i64,
        args: [u64; 6],
        instruction_pointer: u64,
        expected_instruction: [u8; 4],
    ) -> Result<(), TraceError> {
        if self.after_loader_config().is_none() {
            return Ok(());
        }
        if self.liteinst_after_loader_syscall_permit.is_some()
            || self.liteinst_after_loader_syscall_inflight.is_some()
            || self.liteinst_after_loader_private_call.is_some()
        {
            return Err(Errno::EALREADY.into());
        }
        let mut instruction = [0; 4];
        task.read_exact(instruction_pointer as usize, &mut instruction)?;
        if instruction != expected_instruction || instruction[..2] != [0x0f, 0x05] {
            return Err(Errno::EPROTO.into());
        }
        let resume_pointer = instruction_pointer.checked_add(2).ok_or(Errno::EOVERFLOW)?;
        let image = self
            .after_loader_identity(task)
            .map_err(|_| TraceError::from(Errno::EPROTO))?;
        let output_spans = syscall_output_spans(number, args)?;
        self.liteinst_after_loader_syscall_permit = Some(AfterLoaderSyscallPermit {
            image: Some(image),
            tid: task.pid(),
            generation: image.generation,
            physical_generation: Some(task.physical_event_generation()),
            origin_status: task.physical_status_id(),
            admission_status: None,
            purpose,
            number,
            args,
            executed_args: args,
            instruction_pointer,
            resume_pointer,
            instruction,
            instruction_length: 4,
            output_spans,
            effect: AfterLoaderSyscallEffect::None,
        });
        Ok(())
    }

    pub(super) fn consume_after_loader_syscall_permit(
        &mut self,
        task: &Stopped,
    ) -> Result<Option<AfterLoaderSyscallPermit>, TraceError> {
        if self.after_loader_config().is_none() {
            return Ok(None);
        }
        if self.liteinst_after_loader_syscall_inflight.is_some() {
            return Err(Errno::EALREADY.into());
        }
        let mut permit = self
            .liteinst_after_loader_syscall_permit
            .take()
            .ok_or(Errno::EPROTO)?;
        let generation = self.liteinst_runtime.lock().unwrap().generation;
        let identity = match permit.image {
            Some(_) => Some(
                self.after_loader_identity(task)
                    .map_err(|_| TraceError::from(Errno::EPROTO))?,
            ),
            None => None,
        };
        let regs = task.getregs()?;
        let mut instruction = [0; 4];
        let instruction_length = usize::from(permit.instruction_length);
        task.read_exact(
            permit.instruction_pointer as usize,
            &mut instruction[..instruction_length],
        )?;
        let admission_status = task.physical_status_id();
        let origin_status = permit.origin_status.ok_or(Errno::EPROTO)?;
        let admission_status = admission_status.ok_or(Errno::EPROTO)?;
        if identity != permit.image
            || task.pid() != permit.tid
            || generation != permit.generation
            || Some(task.physical_event_generation()) != permit.physical_generation
            || origin_status == admission_status
            || permit
                .admission_status
                .is_some_and(|expected| expected != admission_status)
            || !after_loader_permit_argument_binding_is_exact(&permit)
            || !after_loader_syscall_registers_match(permit.number, permit.args, &regs)
            || regs.rip != permit.resume_pointer
            || instruction[..instruction_length] != permit.instruction[..instruction_length]
        {
            return Err(Errno::EPROTO.into());
        }
        permit.admission_status = Some(admission_status);
        if let Some(config) = self.after_loader_config() {
            config.diagnostics.record(
                "private syscall permit consumed",
                self.timer.diagnostic_clock(),
                format!(
                    "tid={} generation={} origin_status={:?} admission_status={:?} purpose={:?} nr={} logical_args={:?} executed_args={:?} rip={:#x} outputs={:?}",
                    task.pid(),
                    permit.generation,
                    permit.origin_status,
                    permit.admission_status,
                    permit.purpose,
                    permit.number,
                    permit.args,
                    permit.executed_args,
                    permit.instruction_pointer,
                    permit.output_spans,
                ),
            ).map_err(|_| Errno::EOVERFLOW)?;
        }
        self.liteinst_after_loader_syscall_inflight = Some(permit.clone());
        Ok(Some(permit))
    }

    fn take_after_loader_syscall_inflight(
        &mut self,
        task: &Stopped,
        syscall_completion: bool,
    ) -> Result<Option<AfterLoaderSyscallPermit>, TraceError> {
        if self.after_loader_config().is_none() {
            return Ok(None);
        }
        let permit = self
            .liteinst_after_loader_syscall_inflight
            .take()
            .ok_or(Errno::EPROTO)?;
        let origin_status = permit.origin_status.ok_or(Errno::EPROTO)?;
        let admission_status = permit.admission_status.ok_or(Errno::EPROTO)?;
        let completion_status = task.physical_status_id().ok_or(Errno::EPROTO)?;
        if completion_status == origin_status || completion_status == admission_status {
            return Err(Errno::EPROTO.into());
        }
        let generation = self.liteinst_runtime.lock().unwrap().generation;
        let identity = match permit.image {
            Some(_) => Some(
                self.after_loader_identity(task)
                    .map_err(|_| TraceError::from(Errno::EPROTO))?,
            ),
            None => None,
        };
        if task.pid() != permit.tid
            || generation != permit.generation
            || Some(task.physical_event_generation()) != permit.physical_generation
            || identity != permit.image
        {
            return Err(Errno::EPROTO.into());
        }
        if syscall_completion {
            let regs = task.getregs()?;
            let instruction_length = usize::from(permit.instruction_length);
            let mut instruction = [0; 4];
            task.read_exact(
                permit.instruction_pointer as usize,
                &mut instruction[..instruction_length],
            )?;
            if !after_loader_permit_argument_binding_is_exact(&permit)
                || !after_loader_syscall_registers_match(permit.number, permit.executed_args, &regs)
                || regs.rip != permit.resume_pointer
                || instruction[..instruction_length] != permit.instruction[..instruction_length]
            {
                return Err(Errno::EPROTO.into());
            }
        }
        Ok(Some(permit))
    }

    pub(super) fn complete_after_loader_syscall_inflight(
        &mut self,
        task: &Stopped,
    ) -> Result<Option<AfterLoaderSyscallPermit>, TraceError> {
        self.take_after_loader_syscall_inflight(task, true)
    }

    pub(super) fn complete_after_loader_syscall_successor(
        &mut self,
        task: &Stopped,
    ) -> Result<Option<AfterLoaderSyscallPermit>, TraceError> {
        self.take_after_loader_syscall_inflight(task, false)
    }

    pub(super) fn classify_after_loader_trace_only_syscall(
        &self,
        task: &Stopped,
    ) -> Result<Option<AfterLoaderTraceOnlySyscall>, TraceError> {
        if self.after_loader_config().is_none() {
            return Ok(None);
        }
        if self.liteinst_after_loader_private_state.is_some()
            || self.liteinst_after_loader_syscall_permit.is_some()
            || self.liteinst_after_loader_syscall_inflight.is_some()
            || self.liteinst_after_loader_forward_inflight.is_some()
            || self.liteinst_after_loader_private_call.is_some()
        {
            return Err(Errno::EPROTO.into());
        }
        let regs = task.getregs()?;
        let (raw, known) = classify_trace_only_syscall_number(regs.orig_rax)?;
        let subscribed = known.is_some_and(|number| {
            number != Sysno::rt_sigreturn
                && self
                    .global_state
                    .subscriptions
                    .iter_syscalls()
                    .any(|candidate| candidate == number)
        });
        if subscribed {
            return Ok(None);
        }
        let runtime = self.liteinst_runtime.lock().unwrap();
        let purpose = match runtime.phase {
            LiteinstRuntimePhase::PreExec => {
                if !self.command_bootstrap {
                    return Err(Errno::EPROTO.into());
                }
                if known == Some(Sysno::execve) {
                    let config = self.after_loader_config().ok_or(Errno::EPROTO)?;
                    self.validate_after_loader_command_execve(task, &config, &regs)
                        .map_err(|_| TraceError::from(Errno::EPROTO))?;
                }
                AfterLoaderSyscallPurpose::TraceePreinit
            }
            LiteinstRuntimePhase::Waiting => AfterLoaderSyscallPurpose::TraceePreinit,
            LiteinstRuntimePhase::Ready
                if runtime.ready_generation == Some(runtime.generation)
                    && runtime.after_loader_reference.is_some() =>
            {
                AfterLoaderSyscallPurpose::OrdinaryForward
            }
            LiteinstRuntimePhase::Bootstrap | LiteinstRuntimePhase::Ready => {
                return Err(Errno::EPROTO.into());
            }
        };
        Ok(Some(AfterLoaderTraceOnlySyscall {
            purpose,
            number: raw,
            args: [regs.rdi, regs.rsi, regs.rdx, regs.r10, regs.r8, regs.r9],
            known,
        }))
    }

    fn begin_after_loader_forward(
        &mut self,
        task: &Stopped,
        number: i64,
        args: [u64; 6],
        kind: AfterLoaderForwardKind,
    ) -> Result<(), Error> {
        if self.liteinst_after_loader_forward_inflight.is_some() {
            return Err(self.caller_error("trace-only syscall forwarding is already in flight"));
        }
        let registers = task.getregs()?;
        let instruction_pointer = registers.rip.checked_sub(2).ok_or(Errno::EPROTO)?;
        let mut instruction = [0; 2];
        task.read_exact(instruction_pointer as usize, &mut instruction)?;
        let generation = self.liteinst_runtime.lock().unwrap().generation;
        let image = if generation == 0 {
            None
        } else {
            Some(self.after_loader_identity(task)?)
        };
        let admission_status = task.physical_status_id().ok_or_else(|| {
            self.caller_error("trace-only syscall admission has no physical status")
        })?;
        if instruction != [0x0f, 0x05]
            || registers.orig_rax as i64 != number
            || [
                registers.rdi,
                registers.rsi,
                registers.rdx,
                registers.r10,
                registers.r8,
                registers.r9,
            ] != args
        {
            return Err(self.caller_error("trace-only syscall changed at forwarding admission"));
        }
        self.liteinst_after_loader_forward_inflight = Some(AfterLoaderForwardInFlight {
            tid: task.pid(),
            generation,
            image,
            physical_generation: task.physical_event_generation(),
            observed_statuses: BTreeSet::from([admission_status]),
            instruction_pointer,
            resume_pointer: registers.rip,
            instruction,
            number,
            args,
            kind,
        });
        Ok(())
    }

    fn validate_after_loader_forward_stop(
        &self,
        state: &mut AfterLoaderForwardInFlight,
        task: &Stopped,
    ) -> Result<(), Error> {
        let status = task.physical_status_id().ok_or_else(|| {
            self.caller_error("trace-only forwarding stop has no physical status")
        })?;
        let current_generation = self.liteinst_runtime.lock().unwrap().generation;
        let image = match state.image {
            Some(_) => Some(self.after_loader_identity(task)?),
            None => None,
        };
        if task.pid() != state.tid
            || task.physical_event_generation() != state.physical_generation
            || current_generation != state.generation
            || image != state.image
            || !state.observed_statuses.insert(status)
        {
            return Err(
                self.caller_error("trace-only forwarding reused a stop or changed task identity")
            );
        }
        Ok(())
    }

    fn validate_after_loader_forward_completion(
        &self,
        state: &AfterLoaderForwardInFlight,
        task: &Stopped,
    ) -> Result<libc::user_regs_struct, Error> {
        let registers = task.getregs()?;
        let mut instruction = [0; 2];
        task.read_exact(state.instruction_pointer as usize, &mut instruction)?;
        if registers.orig_rax as i64 != state.number
            || [
                registers.rdi,
                registers.rsi,
                registers.rdx,
                registers.r10,
                registers.r8,
                registers.r9,
            ] != state.args
            || registers.rip != state.resume_pointer
            || instruction != state.instruction
        {
            return Err(self.caller_error("trace-only syscall completion identity changed"));
        }
        Ok(registers)
    }

    pub(super) async fn route_after_loader_forward_inflight(
        &mut self,
        task: Stopped,
        event: &Event,
    ) -> Result<AfterLoaderForwardRoute, Error> {
        let mut state = self
            .liteinst_after_loader_forward_inflight
            .take()
            .ok_or_else(|| self.caller_error("trace-only forwarding state is absent"))?;
        self.validate_after_loader_forward_stop(&mut state, &task)?;
        if matches!(event, Event::Signal(_)) {
            self.liteinst_after_loader_forward_inflight = Some(state);
            return Ok(AfterLoaderForwardRoute::Continue(task));
        }
        if matches!(event, Event::Exec(_))
            && matches!(
                &state.kind,
                AfterLoaderForwardKind::TraceOnly(AfterLoaderTraceOnlySyscall {
                    purpose: AfterLoaderSyscallPurpose::TraceePreinit,
                    known: Some(Sysno::execve),
                    ..
                })
            )
        {
            self.caller_observe(
                "trace-only initial exec completed",
                format!("nr={} args={:?}", state.number, state.args),
            )?;
            return Ok(AfterLoaderForwardRoute::Continue(task));
        }
        if !matches!(event, Event::Syscall) {
            return Err(self.caller_error(format!(
                "trace-only syscall produced an unexpected in-flight event: {}",
                after_loader_event_summary(event),
            )));
        }
        let completed = self.validate_after_loader_forward_completion(&state, &task)?;
        let AfterLoaderForwardKind::TraceOnly(operation) = state.kind;
        let raw_result = completed.rax as i64;
        if let Some(number) = operation.known {
            let args = SyscallArgs::new(
                operation.args[0] as usize,
                operation.args[1] as usize,
                operation.args[2] as usize,
                operation.args[3] as usize,
                operation.args[4] as usize,
                operation.args[5] as usize,
            );
            self.observe_liteinst_mapping_result(
                number,
                args,
                Errno::from_ret(completed.rax as usize).map(|value| value as i64),
            );
        }
        self.caller_observe(
            "trace-only syscall completed",
            format!(
                "purpose={:?} nr={} raw_result={raw_result}",
                operation.purpose, operation.number
            ),
        )?;
        let signal = self.take_pending_signal_for_resume(
            &task,
            LiteinstActivationOperation::ResumeInjectedSyscall,
        )?;
        let wait = self.resume_stopped(task, signal)?.next_state().await?;
        self.arm_liteinst_wait(&wait)?;
        Ok(AfterLoaderForwardRoute::Completed(wait))
    }

    pub(super) async fn forward_after_loader_trace_only_syscall(
        &mut self,
        task: Stopped,
        operation: AfterLoaderTraceOnlySyscall,
    ) -> Result<Wait, Error> {
        let repeated = self
            .classify_after_loader_trace_only_syscall(&task)?
            .ok_or(Errno::EPROTO)?;
        if repeated != operation {
            return Err(self.caller_error("trace-only syscall changed before admission"));
        }
        self.observe_after_loader_stopped_event(&task, &Event::Seccomp)?;

        if let Some(number) = operation.known {
            let args = SyscallArgs::new(
                operation.args[0] as usize,
                operation.args[1] as usize,
                operation.args[2] as usize,
                operation.args[3] as usize,
                operation.args[4] as usize,
                operation.args[5] as usize,
            );
            self.validate_liteinst_mapping_execution(number, args)?;
        }

        self.caller_observe(
            "trace-only syscall admitted",
            format!(
                "purpose={:?} nr={} args={:?}",
                operation.purpose, operation.number, operation.args
            ),
        )?;
        if operation.known.is_some_and(is_task_creating_syscall) {
            self.deopt_liteinst_hooks_quiescent(
                &task,
                LiteinstDeoptProgramCounter::TranslateGenerated,
            )?;
        }
        if operation.known == Some(Sysno::rt_sigreturn) {
            // Linux consumes the original signal frame and resumes at the
            // restored context. There is no syscall-return stop to wait for,
            // so retire every displaced site before that arbitrary RIP can
            // execute.
            self.deopt_liteinst_hooks_quiescent(
                &task,
                LiteinstDeoptProgramCounter::TranslateGenerated,
            )?;
            let wait = self.resume_stopped(task, None)?.next_state().await?;
            self.arm_liteinst_wait(&wait)?;
            self.caller_observe(
                "trace-only rt_sigreturn tail completed",
                format!("purpose={:?}", operation.purpose),
            )?;
            return Ok(wait);
        }
        self.begin_after_loader_forward(
            &task,
            operation.number,
            operation.args,
            AfterLoaderForwardKind::TraceOnly(operation),
        )?;
        let wait = self.syscall_stopped(task, None)?.next_state().await?;
        self.arm_liteinst_wait(&wait)?;
        Ok(wait)
    }

    pub(super) fn observe_after_loader_stopped_event(
        &self,
        task: &Stopped,
        event: &Event,
    ) -> Result<(), TraceError> {
        let Some(config) = self.after_loader_config() else {
            return Ok(());
        };
        let mut runtime = self.liteinst_runtime.lock().unwrap();
        if runtime.phase != LiteinstRuntimePhase::Ready
            || runtime.after_loader_guest_observed
            || runtime.after_loader_reference.is_none()
        {
            return Ok(());
        }
        let regs = task.getregs()?;
        config
            .diagnostics
            .record(
                "first ordinary guest event after restoration",
                self.timer.diagnostic_clock(),
                format!(
                    "tid={} generation={} physical_status={:?} event={} rip={:#x} syscall={}",
                    task.pid(),
                    runtime.generation,
                    task.physical_status_id(),
                    after_loader_event_summary(event),
                    regs.rip,
                    regs.orig_rax
                ),
            )
            .map_err(|_| Errno::EOVERFLOW)?;
        runtime.after_loader_guest_observed = true;
        Ok(())
    }

    pub(super) fn observe_after_loader_terminal_event(
        &self,
        pid: Pid,
        exit_status: ExitStatus,
    ) -> Result<(), Error> {
        let Some(config) = self.after_loader_config() else {
            return Ok(());
        };
        let mut runtime = self.liteinst_runtime.lock().unwrap();
        if runtime.phase != LiteinstRuntimePhase::Ready
            || runtime.after_loader_guest_observed
            || runtime.after_loader_reference.is_none()
        {
            return Ok(());
        }
        config
            .diagnostics
            .record(
                "first ordinary guest event after restoration",
                self.timer.diagnostic_clock(),
                format!(
                    "tid={pid} generation={} event=Exited({exit_status:?}) registers=unavailable",
                    runtime.generation
                ),
            )
            .map_err(|error| self.caller_error(error))?;
        runtime.after_loader_guest_observed = true;
        Ok(())
    }

    pub(super) fn observe_after_loader_tool_callback(&self, callback: &str) -> Result<(), Error> {
        let Some(context) = self.after_loader_tool_callback_context() else {
            return Ok(());
        };
        context.record(callback)
    }

    pub(super) fn after_loader_tool_callback_context(
        &self,
    ) -> Option<AfterLoaderToolCallbackContext> {
        let config = self.after_loader_config()?;
        let runtime = self.liteinst_runtime.lock().unwrap();
        Some(AfterLoaderToolCallbackContext {
            diagnostics: config.diagnostics,
            raw_clock: self.timer.diagnostic_clock(),
            tid: self.tid(),
            generation: runtime.generation,
            phase: runtime.phase,
            physical_status: self
                .active_tool_stop
                .as_ref()
                .and_then(Stopped::physical_status_id),
        })
    }

    pub(super) fn after_loader_identity(&self, task: &Stopped) -> Result<ImageIdentity, Error> {
        let tid = task.pid();
        if tid != self.tid() {
            return Err(self.caller_error("stopped TID changed"));
        }
        let bytes = bounded_proc(format!("/proc/{tid}/stat"), 64 * 1024)
            .map_err(|e| self.caller_error(e))?;
        let text = std::str::from_utf8(&bytes).map_err(|e| self.caller_error(e))?;
        let close = text.rfind(')').ok_or(Errno::EPROTO)?;
        let fields: Vec<_> = text[close + 1..].split_whitespace().collect();
        let start_ticks = fields
            .get(19)
            .ok_or(Errno::EPROTO)?
            .parse::<u64>()
            .map_err(|e| self.caller_error(e))?;
        if text.split_whitespace().next() != Some(tid.as_raw().to_string().as_str()) {
            return Err(self.caller_error("proc stat TID disagrees"));
        }
        let executable =
            std::fs::metadata(format!("/proc/{tid}/exe")).map_err(|e| self.caller_error(e))?;
        Ok(ImageIdentity {
            tid: tid.as_raw(),
            start_ticks,
            generation: self.liteinst_runtime.lock().unwrap().generation,
            executable_device: executable.dev(),
            executable_inode: executable.ino(),
            at_entry: guest_auxv_entry(tid, libc::AT_ENTRY).ok_or(Errno::EPROTO)?,
            at_phdr: guest_auxv_entry(tid, libc::AT_PHDR).ok_or(Errno::EPROTO)?,
        })
    }

    fn caller_quiescent(&self, task: &Stopped, image: ImageIdentity) -> Result<(), Error> {
        if self.after_loader_identity(task)? != image {
            return Err(self.caller_error("image changed"));
        }
        let runtime = self
            .global_state
            .liteinst_runtime
            .as_ref()
            .ok_or(Errno::EPROTO)?;
        if runtime.root_tid.get() != Some(&task.pid())
            || runtime.multi_task.load(Ordering::SeqCst)
            || !runtime.newborn_tracees.lock().unwrap().is_empty()
            || self.pending_signal.is_some()
        {
            return Err(
                self.caller_error("caller requires the sole recorded task and no pending signal")
            );
        }
        for _ in 0..2 {
            let mut tids = std::fs::read_dir(format!("/proc/{}/task", task.pid()))
                .map_err(|e| self.caller_error(e))?
                .map(|entry| entry.map(|e| e.file_name()))
                .collect::<io::Result<Vec<_>>>()
                .map_err(|e| self.caller_error(e))?;
            tids.sort();
            if tids != [std::ffi::OsString::from(task.pid().as_raw().to_string())] {
                return Err(self.caller_error("task population changed"));
            }
        }
        signal_state(task.pid()).map_err(|e| self.caller_error(e))?;
        Ok(())
    }

    fn caller_trap(&self, task: &Stopped) -> Result<TrapObservation, Error> {
        let regs = task.getregs()?;
        let info = task.getsiginfo()?;
        Ok(TrapObservation {
            identity: self.after_loader_identity(task)?,
            signal: info.si_signo,
            si_code: info.si_code,
            rip: regs.rip,
            rsp: regs.rsp,
            r10: regs.r10,
        })
    }

    async fn caller_wait(&self, running: Running, operation: &str) -> Result<Wait, Error> {
        // The sole wait consumer owns this result and arms the cleanup lease
        // before inspecting, reporting or rejecting it. Never forge Stopped.
        let wait = running.next_state().await?;
        self.arm_liteinst_wait(&wait)?;
        // `Wait`'s derived Debug includes the complete notifier generation and
        // its bounded physical-observer storage. Retaining that implementation
        // graph once per private stop duplicates hundreds of KiB and can hide
        // the later, semantically useful observations behind the diagnostic
        // byte bound. Record every transition identity explicitly instead;
        // the observer remains the lossless physical partition evidence.
        self.caller_observe(operation, after_loader_wait_summary(&wait))?;
        if let Wait::Stopped(task, event) = &wait {
            let regs = task.getregs()?;
            self.caller_observe(
                "private held stop registers",
                format!(
                    "tid={} event={} regs={:?}",
                    task.pid(),
                    after_loader_event_summary(event),
                    register_words(&regs)
                ),
            )?;
            if matches!(event, Event::Signal(_)) {
                let info = task.getsiginfo()?;
                self.caller_observe(
                    "private held stop siginfo",
                    format!(
                        "tid={} signo={} code={} errno={}",
                        task.pid(),
                        info.si_signo,
                        info.si_code,
                        info.si_errno
                    ),
                )?;
            }
        }
        Ok(wait)
    }

    async fn caller_syscall(
        &mut self,
        task: Stopped,
        image: ImageIdentity,
        nr: Sysno,
        args: [u64; 6],
    ) -> Result<(Stopped, u64), Error> {
        self.caller_quiescent(&task, image)?;
        let mut private_stub = [0; 4];
        task.read_exact(cp::PRIVATE_PAGE_OFFSET, &mut private_stub)?;
        if private_stub != [0x0f, 0x05, 0x0f, 0x0b] {
            return Err(self.caller_error("private syscall stub changed before execution"));
        }
        let old = task.getregs()?;
        let mut regs = old;
        regs.rax = nr as u64;
        regs.orig_rax = nr as u64;
        regs.set_args((args[0], args[1], args[2], args[3], args[4], args[5]));
        regs.rip = cp::PRIVATE_PAGE_OFFSET as u64;
        let config = self.after_loader_config().ok_or(Errno::EPROTO)?;
        let effect = self.admit_after_loader_controller_syscall(&task, &config, nr as i64, args)?;
        self.arm_after_loader_syscall_permit(
            &task,
            AfterLoaderSyscallPurpose::PrivateSetup,
            nr as i64,
            args,
            cp::PRIVATE_PAGE_OFFSET as u64,
        )?;
        self.bind_after_loader_syscall_effect(effect)?;
        task.setregs(&regs)?;
        let running = self.step_stopped(task, None)?;
        let wait = self.caller_wait(running, "private syscall stop").await?;
        let task = match wait {
            Wait::Stopped(task, Event::Seccomp) => task,
            other => {
                return Err(self.caller_error(format!(
                    "unexpected private syscall event: {}",
                    after_loader_wait_summary(&other),
                )));
            }
        };
        self.caller_quiescent(&task, image)?;
        let permit = self
            .consume_after_loader_syscall_permit(&task)?
            .ok_or(Errno::EPROTO)?;
        if permit.purpose != AfterLoaderSyscallPurpose::PrivateSetup
            || permit.number != nr as i64
            || permit.args != args
        {
            return Err(self.caller_error("private syscall consumed a different permit"));
        }
        let wait = self
            .caller_wait(
                self.syscall_stopped(task, None)?,
                "private syscall completion",
            )
            .await?;
        let mut task = match wait {
            Wait::Stopped(task, Event::Syscall) => task,
            other => {
                return Err(self.caller_error(format!(
                    "unexpected private syscall completion: {}",
                    after_loader_wait_summary(&other),
                )));
            }
        };
        self.caller_quiescent(&task, image)?;
        let completed_permit = self
            .complete_after_loader_syscall_inflight(&task)?
            .ok_or(Errno::EPROTO)?;
        if completed_permit != permit {
            return Err(self.caller_error("private syscall completion authority changed"));
        }
        let completed = task.getregs()?;
        if completed.orig_rax as i64 != nr as i64
            || [
                completed.rdi,
                completed.rsi,
                completed.rdx,
                completed.r10,
                completed.r8,
                completed.r9,
            ] != args
            || completed.rip != cp::PRIVATE_PAGE_OFFSET as u64 + 2
        {
            return Err(self.caller_error("private syscall completion identity changed"));
        }
        let value = completed.rax;
        let completion = self.complete_after_loader_private_syscall(
            &mut task,
            &config,
            &completed_permit,
            value as i64,
        )?;
        self.caller_observe(
            "private syscall result",
            format!("{nr:?} args={args:?} result={value:#x}"),
        )?;
        task.setregs(&old)?;
        if register_words(&task.getregs()?) != register_words(&old) {
            return Err(self.caller_error("private syscall register restoration differs"));
        }
        if !caller_private_syscall_result_is_accepted(completion) {
            return Err(self.caller_error(format!(
                "private syscall {nr:?} failed with {}",
                value as i64
            )));
        }
        Ok((task, value))
    }

    fn caller_write(&self, task: &mut Stopped, address: u64, bytes: &[u8]) -> Result<(), Error> {
        task.write_exact(
            AddrMut::from_raw(address as usize).ok_or(Errno::EFAULT)?,
            bytes,
        )?;
        let mut observed = vec![0; bytes.len()];
        task.read_exact(address as usize, &mut observed)?;
        if observed != bytes {
            return Err(self.caller_error("private write readback differs"));
        }
        Ok(())
    }

    async fn caller_signal_actions(
        &mut self,
        mut task: Stopped,
        image: ImageIdentity,
        data: u64,
        label: &str,
    ) -> Result<(Stopped, Vec<[u8; 32]>, [u8; 8]), Error> {
        // x86-64 kernel sigaction is handler, flags, restorer and 64-bit mask:
        // four contiguous u64 values. Query every kernel signal, including
        // glibc's reserved realtime numbers. No libc wrapper or handler runs.
        let mut actions = Vec::with_capacity(PRIVATE_SIGNAL_ACTION_COUNT as usize);
        for signal in 1..=PRIVATE_SIGNAL_ACTION_COUNT {
            let destination =
                data + PRIVATE_SIGNAL_ACTIONS_OFFSET + (signal - 1) * PRIVATE_SIGNAL_ACTION_BYTES;
            let (next, _) = self
                .caller_syscall(
                    task,
                    image,
                    Sysno::rt_sigaction,
                    [signal, 0, destination, 8, 0, 0],
                )
                .await?;
            task = next;
            let mut action = [0; 32];
            task.read_exact(destination as usize, &mut action)?;
            actions.push(action);
        }
        let (task, _) = self
            .caller_syscall(
                task,
                image,
                Sysno::rt_sigprocmask,
                [libc::SIG_SETMASK as u64, 0, data + 352, 8, 0, 0],
            )
            .await?;
        let mut mask = [0; 8];
        task.read_exact((data + 352) as usize, &mut mask)?;
        self.caller_observe(
            label,
            format!("kernel_sigactions={actions:?} kernel_mask={mask:?}"),
        )?;
        Ok((task, actions, mask))
    }

    fn caller_sealed_runtime(
        &self,
        task: &Stopped,
        config: &LiteinstAfterLoaderConfig,
        fd: u64,
    ) -> Result<(), Error> {
        let sealed = &config.sealed_runtime;
        if unsafe { libc::fcntl(sealed.file.as_raw_fd(), libc::F_GET_SEALS) }
            != crate::after_loader::RUNTIME_SEALS
        {
            return Err(self.caller_error("controller runtime seals changed"));
        }
        let path = format!("/proc/{}/fd/{fd}", task.pid());
        let mut file = std::fs::File::open(path).map_err(|e| self.caller_error(e))?;
        let metadata = file.metadata().map_err(|e| self.caller_error(e))?;
        let mut bytes = Vec::new();
        (&mut file)
            .take(crate::after_loader::MAX_RUNTIME_FILE as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| self.caller_error(e))?;
        if metadata.dev() != sealed.image.file_identity.device
            || metadata.ino() != sealed.image.file_identity.inode
            || bytes.as_slice() != sealed.image.bytes.as_ref()
            || unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GET_SEALS) }
                != crate::after_loader::RUNTIME_SEALS
        {
            return Err(self.caller_error("target runtime descriptor differs from sealed source"));
        }
        Ok(())
    }

    fn caller_errno(&self, task: &Stopped, pointer: u64) -> Result<[u8; 4], Error> {
        let range = GuestRange::new(pointer, 4).ok_or(Errno::EFAULT)?;
        if !guest_maps(task.pid()).is_some_and(|maps| {
            maps.iter()
                .any(|map| map.readable && map.writable && !map.shared && map.contains_range(range))
        }) {
            return Err(
                self.caller_error("errno pointer is not private readable/writable target memory")
            );
        }
        let mut bytes = [0; 4];
        task.read_exact(pointer as usize, &mut bytes)?;
        Ok(bytes)
    }

    fn caller_snapshot(
        &self,
        task: &Stopped,
        label: &str,
        stack: u64,
        length: usize,
        random: u64,
    ) -> Result<(), Error> {
        if length > MAX_STACK_SNAPSHOT {
            return Err(self.caller_error("snapshot stack bound"));
        }
        let regs = task.getregs()?;
        let xstate = task.get_x86_extended_state()?;
        let mut stack_bytes = vec![0; length];
        task.read_exact(stack as usize, &mut stack_bytes)?;
        let mut random_bytes = [0; 16];
        task.read_exact(random as usize, &mut random_bytes)?;
        let mut canary = [0; 8];
        task.read_exact(
            regs.fs_base.checked_add(0x28).ok_or(Errno::EPROTO)? as usize,
            &mut canary,
        )?;
        self.caller_observe(
            &format!("{label} registers and XSTATE"),
            format!("regs={:?} xstate={xstate:?}", register_words(&regs)),
        )?;
        self.caller_observe(
            &format!("{label} original stack"),
            format!("address={stack:#x} bytes={stack_bytes:?}"),
        )?;
        self.caller_observe(
            &format!("{label} random and canary"),
            format!("AT_RANDOM={random_bytes:?} canary={canary:?}"),
        )?;
        self.caller_observe(
            &format!("{label} signals and descriptors"),
            format!(
                "signals={:?} descriptors={:?}",
                signal_state(task.pid()).map_err(|e| self.caller_error(e))?,
                descriptor_state(task.pid()).map_err(|e| self.caller_error(e))?
            ),
        )?;
        let maps = bounded_proc(format!("/proc/{}/maps", task.pid()), 2 * 1024 * 1024)
            .map_err(|e| self.caller_error(e))?;
        self.caller_observe(
            &format!("{label} maps"),
            String::from_utf8(maps).map_err(|e| self.caller_error(e))?,
        )
    }

    fn caller_private_syscall(
        &self,
        task: &Stopped,
        config: &LiteinstAfterLoaderConfig,
    ) -> Result<(), Error> {
        let r = task.getregs()?;
        // Authenticate the syscall instruction and its bound loader/runtime
        // image before the separate function-specific admission proves the
        // exact arguments and owned descriptor, mapping, futex or break state.
        let profiled_entropy = r.orig_rax as i64 == libc::SYS_getrandom;
        if !private_syscall_allowed(r.orig_rax as i64) && !profiled_entropy {
            return Err(self.caller_error(format!("unexpected private syscall {}", r.orig_rax)));
        }
        let ip = r.rip.checked_sub(2).ok_or(Errno::EPROTO)?;
        let maps = guest_maps(task.pid()).ok_or(Errno::EPROTO)?;
        let map = maps
            .iter()
            .find(|m| {
                m.readable
                    && m.executable
                    && !m.writable
                    && !m.shared
                    && m.contains(ip)
                    && m.contains(r.rip - 1)
            })
            .ok_or(Errno::EPROTO)?;
        let images = std::iter::once(&config.provider)
            .chain(std::iter::once(&config.sealed_runtime.image))
            .chain(config.dependencies.iter());
        let state = self.after_loader_private_state()?;
        let expected = images
            .into_iter()
            .find(|image| {
                state
                    .image_mapping_identity(image)
                    .is_some_and(|identity| mapping_identity_matches(identity, map))
            })
            .ok_or_else(|| self.caller_error("private syscall came from unbound or guest code"))?;
        if profiled_entropy {
            let provider_geometry = state
                .image_geometry(&config.provider)
                .ok_or_else(|| self.caller_error("bootstrap entropy provider is unbound"))?;
            let expected_ip = provider_geometry
                .load_bias
                .checked_add(PTMALLOC_BOOTSTRAP_SYSCALL_RVA)
                .ok_or(Errno::EOVERFLOW)?;
            if state.image_id(expected) != state.image_id(&config.provider)
                || expected.path != config.provider.path
                || expected.bytes.as_ref() != config.provider.bytes.as_ref()
                || ip != expected_ip
            {
                return Err(self
                    .caller_error("bootstrap entropy syscall is outside the exact provider site"));
            }
        }
        let offset = map
            .offset
            .checked_add(ip - map.start)
            .ok_or(Errno::EPROTO)? as usize;
        let wanted = expected
            .bytes
            .get(offset..offset + 2)
            .ok_or(Errno::EPROTO)?;
        let mut actual = [0; 2];
        task.read_exact(ip as usize, &mut actual)?;
        if actual != [0x0f, 0x05] || actual != wanted {
            return Err(self.caller_error("private syscall bytes differ from bound image"));
        }
        self.caller_observe(
            "private loader syscall",
            format!(
                "ip={ip:#x} nr={} image={}",
                r.orig_rax,
                expected.path.display()
            ),
        )
    }

    fn begin_after_loader_private_call(
        &mut self,
        task: &Stopped,
        image: ImageIdentity,
        calls: &Calls,
        code: &CallCode,
        arguments: [u64; 2],
        function: CallerFunction,
    ) -> Result<(), Error> {
        if self.liteinst_after_loader_private_state.is_none()
            || self.liteinst_after_loader_syscall_permit.is_some()
            || self.liteinst_after_loader_syscall_inflight.is_some()
            || self.liteinst_after_loader_private_call.is_some()
        {
            return Err(self.caller_error("private call authority was not empty at call origin"));
        }
        let origin_status = task
            .physical_status_id()
            .ok_or_else(|| self.caller_error("private call origin has no physical status"))?;
        if self.after_loader_identity(task)? != image {
            return Err(self.caller_error("private call image changed at call origin"));
        }
        let expected_phase = match function {
            CallerFunction::ErrnoLocation => {
                matches!(
                    calls.phase(),
                    CallsPhase::EntryHeld | CallsPhase::InitializerReturned
                )
            }
            CallerFunction::Dlopen => calls.phase() == CallsPhase::Dlopen,
            CallerFunction::Initializer => calls.phase() == CallsPhase::Initializing,
        };
        let regs = task.getregs()?;
        let mut code_bytes = [0; 30];
        task.read_exact(code.entry as usize, &mut code_bytes)?;
        if !expected_phase
            || code_bytes != code.bytes
            || regs.rip != code.entry
            || regs.rsp != code.call_stack_top
            || regs.rdi != arguments[0]
            || regs.rsi != arguments[1]
            || regs.orig_rax != u64::MAX
        {
            return Err(self.caller_error("private call origin differs from the bound call"));
        }
        self.liteinst_after_loader_private_call = Some(AfterLoaderPrivateCall {
            image,
            function,
            origin_status,
            entry: code.entry,
            return_rip: code.return_rip,
            call_stack_top: code.call_stack_top,
            code_bytes,
            arguments,
            calls_phase: calls.phase(),
        });
        Ok(())
    }

    fn validate_after_loader_private_call(
        &self,
        task: &Stopped,
        image: ImageIdentity,
        calls: &Calls,
        code: &CallCode,
        function: CallerFunction,
    ) -> Result<AfterLoaderPrivateCall, Error> {
        let active = self
            .liteinst_after_loader_private_call
            .as_ref()
            .ok_or_else(|| self.caller_error("private call has no origin authority"))?
            .clone();
        let phase_matches = match active.function {
            CallerFunction::ErrnoLocation | CallerFunction::Dlopen => {
                calls.phase() == active.calls_phase
            }
            CallerFunction::Initializer => {
                active.calls_phase == CallsPhase::Initializing
                    && matches!(
                        calls.phase(),
                        CallsPhase::Initializing
                            | CallsPhase::BeginObserved
                            | CallsPhase::ReadyObserved
                    )
            }
        };
        let mut code_bytes = [0; 30];
        task.read_exact(active.entry as usize, &mut code_bytes)?;
        if active.image != image
            || self.after_loader_identity(task)? != image
            || active.function != function
            || active.entry != code.entry
            || active.return_rip != code.return_rip
            || active.call_stack_top != code.call_stack_top
            || active.code_bytes != code.bytes
            || code_bytes != active.code_bytes
            || !phase_matches
        {
            return Err(self.caller_error("active private call authority changed"));
        }
        Ok(active)
    }

    fn arm_after_loader_private_call_syscall(
        &mut self,
        task: &Stopped,
        image: ImageIdentity,
        calls: &Calls,
        code: &CallCode,
        function: CallerFunction,
    ) -> Result<AfterLoaderSyscallPermit, Error> {
        if self.liteinst_after_loader_syscall_permit.is_some()
            || self.liteinst_after_loader_syscall_inflight.is_some()
        {
            return Err(self.caller_error("private syscall authority was not empty at admission"));
        }
        let active = self.validate_after_loader_private_call(task, image, calls, code, function)?;
        let admission_status = task
            .physical_status_id()
            .ok_or_else(|| self.caller_error("private syscall admission has no physical status"))?;
        if admission_status == active.origin_status {
            return Err(
                self.caller_error("private syscall admission reused its call-origin status")
            );
        }
        let regs = task.getregs()?;
        let instruction_pointer = regs.rip.checked_sub(2).ok_or(Errno::EPROTO)?;
        let mut instruction = [0; 4];
        task.read_exact(instruction_pointer as usize, &mut instruction[..2])?;
        if instruction[..2] != [0x0f, 0x05] {
            return Err(self.caller_error("private syscall instruction changed at admission"));
        }
        let args = [regs.rdi, regs.rsi, regs.rdx, regs.r10, regs.r8, regs.r9];
        Ok(AfterLoaderSyscallPermit {
            image: Some(image),
            tid: task.pid(),
            generation: image.generation,
            physical_generation: Some(task.physical_event_generation()),
            origin_status: Some(active.origin_status),
            admission_status: Some(admission_status),
            purpose: AfterLoaderSyscallPurpose::PrivateSetup,
            number: regs.orig_rax as i64,
            args,
            executed_args: args,
            instruction_pointer,
            resume_pointer: regs.rip,
            instruction,
            instruction_length: 2,
            output_spans: syscall_output_spans(regs.orig_rax as i64, args)?,
            effect: AfterLoaderSyscallEffect::None,
        })
    }

    fn finish_after_loader_private_call(
        &mut self,
        task: &Stopped,
        image: ImageIdentity,
        calls: &Calls,
        code: &CallCode,
        function: CallerFunction,
        trap: &TrapObservation,
    ) -> Result<(), Error> {
        if self.liteinst_after_loader_syscall_permit.is_some()
            || self.liteinst_after_loader_syscall_inflight.is_some()
        {
            return Err(self.caller_error("private syscall authority survived to call return"));
        }
        let active = self.validate_after_loader_private_call(task, image, calls, code, function)?;
        let return_status = task
            .physical_status_id()
            .ok_or_else(|| self.caller_error("private call return has no physical status"))?;
        if return_status == active.origin_status {
            return Err(self.caller_error("private call return reused its origin status"));
        }
        code.authenticate_return(trap, &active.code_bytes)
            .map_err(|error| self.caller_error(format!("{error:?}")))?;
        if function == CallerFunction::Dlopen {
            self.validate_loader_cache_retired(
                task,
                &self.after_loader_config().ok_or(Errno::EPROTO)?,
            )?;
        }
        let cleared = self
            .liteinst_after_loader_private_call
            .take()
            .ok_or(Errno::EPROTO)?;
        if cleared != active {
            return Err(self.caller_error("private call authority changed while clearing return"));
        }
        Ok(())
    }

    fn validate_loader_cache_resources_absent(
        &self,
        task: &Stopped,
        config: &LiteinstAfterLoaderConfig,
        descriptor: u64,
        mapping: LoaderCacheMapping,
    ) -> Result<(), Error> {
        let evidence = self.loader_cache_artifact_evidence(config)?;
        let state = self.after_loader_private_state()?;
        let maps = guest_maps(task.pid()).ok_or(Errno::EPROTO)?;
        let descriptor_absent =
            procfs_path_is_absent(format!("/proc/{}/fd/{descriptor}", task.pid()))
                .map_err(|error| self.caller_error(error))?;
        if mapping.identity != evidence.mapping
            || mapping.raw_length != evidence.length
            || state.loader_cache.as_ref().is_none_or(|cache| {
                !cache
                    .aliases
                    .values()
                    .all(LoaderCacheAliasLifecycle::is_consumed)
            })
            || state.owned_descriptors.values().any(|owned| {
                matches!(
                    owned,
                    AfterLoaderOwnedDescriptor::LoaderCache { .. }
                        | AfterLoaderOwnedDescriptor::LoaderCacheAlias { .. }
                )
            })
            || state
                .owned_mappings
                .iter()
                .any(|owned| owned.purpose == AfterLoaderMappingPurpose::LoaderCache)
            || !descriptor_absent
            || maps.iter().any(|candidate| {
                ranges_overlap(
                    (candidate.start, candidate.end),
                    (mapping.start, mapping.end),
                ) || candidate.mapping_identity() == mapping.identity
            })
        {
            return Err(
                self.caller_error("retired loader-cache descriptor or physical mapping reappeared")
            );
        }
        Ok(())
    }

    fn validate_loader_cache_retired(
        &self,
        task: &Stopped,
        config: &LiteinstAfterLoaderConfig,
    ) -> Result<(), Error> {
        let cache = self.authenticate_loader_cache_scratch(task)?;
        let LoaderCacheLifecycle::Retired {
            descriptor,
            mapping,
        } = cache.lifecycle
        else {
            return Err(self.caller_error(
                "private dlopen returned before the loader-cache lifecycle retired",
            ));
        };
        self.validate_loader_cache_resources_absent(task, config, descriptor, mapping)
    }

    fn prepare_loader_cache_scratch_release(
        &mut self,
        task: &Stopped,
        config: &LiteinstAfterLoaderConfig,
        scratch_base: u64,
    ) -> Result<(), Error> {
        self.validate_loader_cache_retired(task, config)?;
        let cache = self.authenticate_loader_cache_scratch(task)?;
        let LoaderCacheLifecycle::Retired {
            descriptor,
            mapping,
        } = cache.lifecycle
        else {
            return Err(Errno::EPROTO.into());
        };
        if cache.scratch_reservation
            != (
                scratch_base,
                scratch_base.checked_add(3 * PAGE).ok_or(Errno::EOVERFLOW)?,
            )
        {
            return Err(self.caller_error("loader-cache scratch release names another reservation"));
        }
        if !self
            .after_loader_private_state_mut()?
            .loader_cache
            .as_mut()
            .ok_or(Errno::EPROTO)?
            .lifecycle
            .arm_scratch_release(descriptor, mapping)
        {
            return Err(self.caller_error(
                "loader-cache scratch retirement attestation changed before release",
            ));
        }
        Ok(())
    }

    fn complete_loader_cache_scratch_release(
        &mut self,
        task: &Stopped,
        config: &LiteinstAfterLoaderConfig,
        scratch_base: u64,
    ) -> Result<(), Error> {
        let cache = self
            .after_loader_private_state()?
            .loader_cache
            .clone()
            .ok_or(Errno::EPROTO)?;
        let LoaderCacheLifecycle::ScratchReleaseArmed {
            descriptor,
            mapping,
        } = cache.lifecycle
        else {
            return Err(self.caller_error(
                "loader-cache scratch release completed without exact retirement authority",
            ));
        };
        let reservation = cache.scratch_reservation;
        let maps = guest_maps(task.pid()).ok_or(Errno::EPROTO)?;
        if reservation
            != (
                scratch_base,
                scratch_base.checked_add(3 * PAGE).ok_or(Errno::EOVERFLOW)?,
            )
            || self
                .after_loader_private_state()?
                .owned_mappings
                .iter()
                .any(|candidate| ranges_overlap((candidate.start, candidate.end), reservation))
            || maps
                .iter()
                .any(|candidate| ranges_overlap((candidate.start, candidate.end), reservation))
        {
            return Err(self.caller_error(
                "released loader-cache reservation retained a logical or physical owner",
            ));
        }
        self.validate_loader_cache_resources_absent(task, config, descriptor, mapping)?;
        if !self
            .after_loader_private_state_mut()?
            .loader_cache
            .as_mut()
            .ok_or(Errno::EPROTO)?
            .lifecycle
            .complete_scratch_release(descriptor, mapping)
        {
            return Err(Errno::EPROTO.into());
        }
        Ok(())
    }

    fn validate_loader_cache_released(
        &self,
        task: &Stopped,
        config: &LiteinstAfterLoaderConfig,
    ) -> Result<(), Error> {
        let cache = self
            .after_loader_private_state()?
            .loader_cache
            .clone()
            .ok_or(Errno::EPROTO)?;
        let LoaderCacheLifecycle::Released {
            descriptor,
            mapping,
        } = cache.lifecycle
        else {
            return Err(
                self.caller_error("Ready publication preceded exact loader-cache scratch release")
            );
        };
        let reservation = cache.scratch_reservation;
        let maps = guest_maps(task.pid()).ok_or(Errno::EPROTO)?;
        if !loader_cache_scratch_metadata_is_exact(reservation, cache.scratch, cache.scratch_owner)
            || self
                .after_loader_private_state()?
                .owned_mappings
                .iter()
                .any(|candidate| ranges_overlap((candidate.start, candidate.end), reservation))
            || maps
                .iter()
                .any(|candidate| ranges_overlap((candidate.start, candidate.end), reservation))
        {
            return Err(self.caller_error(
                "released loader-cache scratch reservation reappeared before Ready publication",
            ));
        }
        self.validate_loader_cache_resources_absent(task, config, descriptor, mapping)
    }

    fn prepare_loader_cache_redirect(
        &self,
        task: &mut Stopped,
        permit: &AfterLoaderSyscallPermit,
    ) -> Result<(), Error> {
        let AfterLoaderSyscallEffect::OpenLoaderCache { redirect, .. } = &permit.effect else {
            return Ok(());
        };
        let expected_executed = loader_cache_redirect_arguments(permit.args, redirect)
            .ok_or_else(|| self.caller_error("loader-cache redirect binding is malformed"))?;
        let cache = self.authenticate_loader_cache_scratch(task)?;
        if permit.executed_args != expected_executed
            || cache.scratch != redirect.scratch
            || cache.scratch_preimage != redirect.preimage
        {
            return Err(self.caller_error("loader-cache executed arguments changed"));
        }
        let mut expected_scratch = redirect.preimage.clone();
        expected_scratch[..redirect.path.len()].copy_from_slice(&redirect.path);
        self.caller_write(task, redirect.scratch.0, &redirect.path)?;
        let mut actual_scratch = vec![0_u8; LOADER_CACHE_SCRATCH_BYTES];
        task.read_exact(redirect.scratch.0 as usize, &mut actual_scratch)?;
        if actual_scratch != expected_scratch {
            return Err(self.caller_error("loader-cache redirect scratch readback differs"));
        }
        let logical_regs = task.getregs()?;
        if register_words(&logical_regs) != redirect.admission_registers
            || task.get_x86_extended_state()? != redirect.admission_xstate
        {
            return Err(self
                .caller_error("loader-cache logical registers or XSTATE changed before rewrite"));
        }
        let mut executed_regs = logical_regs;
        executed_regs.rsi = permit.executed_args[1];
        task.setregs(&executed_regs)?;
        if !loader_cache_redirect_register_words_match(
            redirect.admission_registers,
            permit.executed_args[1],
            register_words(&task.getregs()?),
        ) || task.get_x86_extended_state()? != redirect.admission_xstate
        {
            return Err(
                self.caller_error("loader-cache executed register or XSTATE rewrite differs")
            );
        }
        Ok(())
    }

    fn restore_loader_cache_redirect(
        &self,
        task: &mut Stopped,
        permit: &AfterLoaderSyscallPermit,
        raw_result: i64,
    ) -> Result<Vec<u8>, Error> {
        let AfterLoaderSyscallEffect::OpenLoaderCache { redirect, .. } = &permit.effect else {
            return Err(self.caller_error("non-cache effect reached cache redirect restoration"));
        };
        let cache = self.authenticate_loader_cache_scratch_owner(task)?;
        if cache.scratch != redirect.scratch || cache.scratch_preimage != redirect.preimage {
            return Err(
                self.caller_error("loader-cache scratch authority changed while openat executed")
            );
        }
        let mut expected_scratch = redirect.preimage.clone();
        expected_scratch[..redirect.path.len()].copy_from_slice(&redirect.path);
        let mut actual_scratch = vec![0_u8; LOADER_CACHE_SCRATCH_BYTES];
        task.read_exact(redirect.scratch.0 as usize, &mut actual_scratch)?;
        if actual_scratch != expected_scratch {
            return Err(self.caller_error("loader-cache scratch changed while openat executed"));
        }
        let executed_regs = task.getregs()?;
        if !loader_cache_completion_register_words_match(
            redirect.admission_registers,
            permit.executed_args[1],
            raw_result,
            register_words(&executed_regs),
        ) || task.get_x86_extended_state()? != redirect.admission_xstate
        {
            return Err(
                self.caller_error("loader-cache kernel completion registers or XSTATE changed")
            );
        }
        self.caller_write(task, redirect.scratch.0, &redirect.preimage)?;
        let mut restored_scratch = vec![0_u8; LOADER_CACHE_SCRATCH_BYTES];
        task.read_exact(redirect.scratch.0 as usize, &mut restored_scratch)?;
        if restored_scratch != redirect.preimage {
            return Err(self.caller_error("loader-cache scratch restoration readback differs"));
        }
        let mut logical_regs = executed_regs;
        logical_regs.rsi = permit.args[1];
        task.setregs(&logical_regs)?;
        if !loader_cache_completion_register_words_match(
            redirect.admission_registers,
            permit.args[1],
            raw_result,
            register_words(&task.getregs()?),
        ) || task.get_x86_extended_state()? != redirect.admission_xstate
        {
            return Err(
                self.caller_error("loader-cache logical register or XSTATE restoration differs")
            );
        }
        Ok(redirect.preimage.clone())
    }

    fn complete_ptmalloc_bootstrap_entropy(
        &mut self,
        task: &mut Stopped,
        config: &LiteinstAfterLoaderConfig,
        permit: &AfterLoaderSyscallPermit,
        admission_regs: &libc::user_regs_struct,
        admission_xstate: &safeptrace::X86ExtendedState,
    ) -> Result<AfterLoaderEmulatedCompletion, Error> {
        let (destination, before) = match &permit.effect {
            AfterLoaderSyscallEffect::PtmallocBootstrapEntropy {
                destination,
                before,
            } => (*destination, *before),
            _ => {
                return Err(
                    self.caller_error("non-entropy effect reached bootstrap entropy completion")
                );
            }
        };
        if admission_regs.rax as i64 != -(libc::ENOSYS as i64)
            || register_words(&task.getregs()?) != register_words(admission_regs)
            || task.get_x86_extended_state()? != *admission_xstate
        {
            return Err(self.caller_error("skipped bootstrap entropy changed registers or XSTATE"));
        }
        let mut instruction = [0_u8; 2];
        task.read_exact(permit.instruction_pointer as usize, &mut instruction)?;
        if instruction != permit.instruction[..2] {
            return Err(self.caller_error("bootstrap entropy instruction changed before emulation"));
        }
        let profile = self
            .after_loader_private_state()?
            .ptmalloc_bootstrap
            .ok_or_else(|| self.caller_error("bootstrap entropy profile disappeared"))?;
        let Some((load_bias, current)) = self.observe_ptmalloc_bootstrap(task, config)? else {
            return Err(self.caller_error("bootstrap entropy provider disappeared"));
        };
        if !exact_ptmalloc_bootstrap_admission_state(profile, load_bias, current)
            || destination
                != profile
                    .load_bias
                    .checked_add(PTMALLOC_BOOTSTRAP_TCACHE_KEY_RVA)
                    .ok_or(Errno::EOVERFLOW)?
            || current.tcache_key != before
            || before != [0; 8]
        {
            return Err(self
                .caller_error("bootstrap entropy state changed between admission and completion"));
        }
        let process_state = self.process_state.clone();
        let entropy = process_state
            .handle_backend_bootstrap_entropy(
                &mut self.thread_state,
                BackendBootstrapEntropy::PtmallocTcacheKey,
            )
            .map_err(|error| {
                self.caller_error(format!(
                    "Tool refused profiled ptmalloc bootstrap entropy: {error}"
                ))
            })?;
        self.caller_write(task, destination, &entropy)?;
        let mut completed_regs = *admission_regs;
        completed_regs.rax = 8;
        task.setregs(&completed_regs)?;
        if !ptmalloc_bootstrap_completion_registers_match(admission_regs, &task.getregs()?)
            || task.get_x86_extended_state()? != *admission_xstate
        {
            return Err(
                self.caller_error("bootstrap entropy completion changed state other than RAX")
            );
        }
        let Some((completed_bias, completed)) = self.observe_ptmalloc_bootstrap(task, config)?
        else {
            return Err(self.caller_error("bootstrap entropy provider disappeared after write"));
        };
        if completed_bias != profile.load_bias
            || completed.initialized != 1
            || completed.tcache_key != entropy
        {
            return Err(self.caller_error("bootstrap entropy writeback differs"));
        }
        if self.after_loader_private_state()?.ptmalloc_bootstrap != Some(profile) {
            return Err(self.caller_error("bootstrap entropy profile changed before retirement"));
        }
        let retained = self
            .after_loader_private_state_mut()?
            .ptmalloc_bootstrap
            .as_mut()
            .ok_or(Errno::EPROTO)?;
        retained.entropy_consumed = true;
        retained.injected_key = Some(entropy);
        self.caller_observe(
            "profiled ptmalloc bootstrap entropy emulated",
            format!(
                "length=8 flags={} tcache_key_sha256={}",
                libc::GRND_NONBLOCK,
                sha256_hex(&entropy),
            ),
        )?;
        Ok(AfterLoaderEmulatedCompletion::PtmallocBootstrapEntropy)
    }

    async fn caller_function(
        &mut self,
        task: Stopped,
        image: ImageIdentity,
        config: &LiteinstAfterLoaderConfig,
        calls: &mut Calls,
        code: &CallCode,
        arguments: [u64; 2],
        function: CallerFunction,
    ) -> Result<(Stopped, u64), Error> {
        // Error terminality is part of the target allocator's safety contract:
        // every Err consumes the sole stopped capability. The outer SIGTRAP
        // path must tear down the trace session and must never recreate or
        // resume this task, because a controller refusal can bypass target-side
        // RAII that clears its process-global preparation/install flags.
        self.caller_quiescent(&task, image)?;
        let mut regs = task.getregs()?;
        regs.rip = code.entry;
        regs.rsp = code.call_stack_top;
        regs.rdi = arguments[0];
        regs.rsi = arguments[1];
        regs.rax = 0;
        regs.orig_rax = u64::MAX;
        regs.eflags = liteinst_helper_entry_rflags(regs.eflags);
        task.setregs(&regs)?;
        self.begin_after_loader_private_call(&task, image, calls, code, arguments, function)?;
        let mut wait = self
            .caller_wait(self.resume_stopped(task, None)?, "private function stop")
            .await?;
        loop {
            match wait {
                Wait::Stopped(mut stopped, Event::Seccomp) => {
                    if function == CallerFunction::ErrnoLocation {
                        return Err(self.caller_error("errno accessor attempted a syscall"));
                    }
                    self.caller_quiescent(&stopped, image)?;
                    let mut permit = self.arm_after_loader_private_call_syscall(
                        &stopped, image, calls, code, function,
                    )?;
                    self.caller_private_syscall(&stopped, config)?;
                    let effect = self
                        .admit_after_loader_function_syscall(&stopped, config, function, &permit)?;
                    permit.effect = effect;
                    if let AfterLoaderSyscallEffect::OpenLoaderCache { redirect, .. } =
                        &permit.effect
                    {
                        permit.executed_args =
                            loader_cache_redirect_arguments(permit.args, redirect).ok_or_else(
                                || {
                                    self.caller_error(
                                "loader-cache redirect did not produce exact executed arguments",
                            )
                                },
                            )?;
                    }
                    self.liteinst_after_loader_syscall_permit = Some(permit.clone());
                    let consumed = self
                        .consume_after_loader_syscall_permit(&stopped)?
                        .ok_or(Errno::EPROTO)?;
                    if consumed != permit {
                        return Err(self.caller_error("private function syscall permit changed"));
                    }
                    self.prepare_loader_cache_redirect(&mut stopped, &permit)?;
                    if matches!(
                        &permit.effect,
                        AfterLoaderSyscallEffect::PtmallocBootstrapEntropy { .. }
                    ) {
                        let admission_regs = stopped.getregs()?;
                        let admission_xstate = stopped.get_x86_extended_state()?;
                        let mut successor = self.skip_seccomp_syscall(stopped).await?;
                        self.caller_quiescent(&successor, image)?;
                        let completed_permit = self
                            .complete_after_loader_syscall_successor(&successor)?
                            .ok_or(Errno::EPROTO)?;
                        if completed_permit != permit {
                            return Err(
                                self.caller_error("bootstrap entropy successor authority changed")
                            );
                        }
                        let completion = self.complete_ptmalloc_bootstrap_entropy(
                            &mut successor,
                            config,
                            &completed_permit,
                            &admission_regs,
                            &admission_xstate,
                        )?;
                        if completion != AfterLoaderEmulatedCompletion::PtmallocBootstrapEntropy {
                            return Err(self.caller_error(
                                "bootstrap entropy returned a different emulated completion",
                            ));
                        }
                        wait = self
                            .caller_wait(
                                self.resume_stopped(successor, None)?,
                                "private function stop",
                            )
                            .await?;
                        continue;
                    }
                    let exit = self
                        .caller_wait(
                            self.syscall_stopped(stopped, None)?,
                            "private loader syscall exit",
                        )
                        .await?;
                    let mut stopped = match exit {
                        Wait::Stopped(stopped, Event::Syscall) => stopped,
                        other => {
                            return Err(self.caller_error(format!(
                                "unexpected syscall completion: {}",
                                after_loader_wait_summary(&other),
                            )));
                        }
                    };
                    self.caller_quiescent(&stopped, image)?;
                    let completed_permit = self
                        .complete_after_loader_syscall_inflight(&stopped)?
                        .ok_or(Errno::EPROTO)?;
                    if completed_permit != permit {
                        return Err(self.caller_error(
                            "private function syscall completion authority changed",
                        ));
                    }
                    let completed = stopped.getregs()?;
                    if completed.orig_rax as i64 != permit.number
                        || [
                            completed.rdi,
                            completed.rsi,
                            completed.rdx,
                            completed.r10,
                            completed.r8,
                            completed.r9,
                        ] != permit.executed_args
                        || completed.rip != permit.resume_pointer
                    {
                        return Err(self
                            .caller_error("private function syscall completion identity changed"));
                    }
                    let completion = self.complete_after_loader_private_syscall(
                        &mut stopped,
                        config,
                        &completed_permit,
                        completed.rax as i64,
                    )?;
                    if !matches!(completion, AfterLoaderSyscallCompletion::KernelResult(_)) {
                        return Err(self.caller_error(
                            "private function accepted a controller-only syscall result",
                        ));
                    }
                    self.caller_observe(
                        "private loader syscall result",
                        format!(
                            "nr={} logical_args={:?} executed_args={:?} result={:#x} outputs={:?}",
                            permit.number,
                            permit.args,
                            permit.executed_args,
                            completed.rax,
                            permit.output_spans,
                        ),
                    )?;
                    wait = self
                        .caller_wait(self.resume_stopped(stopped, None)?, "private function stop")
                        .await?;
                }
                Wait::Stopped(stopped, Event::Signal(Signal::SIGTRAP)) => {
                    self.caller_quiescent(&stopped, image)?;
                    let trap = self.caller_trap(&stopped)?;
                    let regs = stopped.getregs()?;
                    let mut bytes = [0; 30];
                    stopped.read_exact(code.entry as usize, &mut bytes)?;
                    if trap.rip == code.return_rip {
                        if function == CallerFunction::Initializer
                            && self.after_loader_private_state()?.proc_fd_audit
                                != ProcFdAuditLifecycle::Complete
                        {
                            return Err(self.caller_error(
                                "initializer returned before the proc-fd audit completed",
                            ));
                        }
                        self.finish_after_loader_private_call(
                            &stopped, image, calls, code, function, &trap,
                        )?;
                        match function {
                            CallerFunction::Initializer => {
                                calls.initializer_returned(code, &trap, &bytes, regs.rax as u32)
                            }
                            CallerFunction::Dlopen => {
                                calls.dlopen_returned(code, &trap, &bytes, regs.rax)
                            }
                            CallerFunction::ErrnoLocation => {
                                calls.errno_returned(code, &trap, &bytes)
                            }
                        }
                        .map_err(|e| self.caller_error(format!("{e:?}")))?;
                        return Ok((stopped, regs.rax));
                    }
                    if function != CallerFunction::Initializer || !matches!(trap.si_code, 1 | 128) {
                        return Err(self.caller_error("unexpected private trap before initializer"));
                    }
                    self.validate_after_loader_private_call(
                        &stopped, image, calls, code, function,
                    )?;
                    match self.classify_liteinst_trap(&stopped, &regs) {
                        Some(LiteinstTrap::HandshakeBegin) => {
                            if self.after_loader_private_state()?.proc_fd_audit
                                != ProcFdAuditLifecycle::Complete
                            {
                                return Err(self.caller_error(
                                    "initializer reached Begin before the proc-fd audit completed",
                                ));
                            }
                            calls.begin()
                        }
                        Some(LiteinstTrap::HandshakeReady) => calls.ready(),
                        _ => {
                            return Err(self.caller_error(
                                "private trap is not the exact Begin/Ready handshake",
                            ));
                        }
                    }
                    .map_err(|e| self.caller_error(format!("{e:?}")))?;
                    self.caller_observe("initializer handshake", format!("{:?}", calls.phase()))?;
                    wait = self
                        .caller_wait(self.resume_stopped(stopped, None)?, "private function stop")
                        .await?;
                }
                other => {
                    return Err(self.caller_error(format!(
                        "unexpected signal, timer, callback, clone or exec: {}",
                        after_loader_wait_summary(&other),
                    )));
                }
            }
        }
    }

    fn plan_after_loader_vdso(
        &self,
        task: &Stopped,
    ) -> Result<Option<crate::vdso::StoppedVdsoPlan>, Error> {
        if !crate::vdso::stopped_patch_required(&self.global_state.subscriptions) {
            return Ok(None);
        }
        let maps = guest_maps(task.pid()).ok_or(Errno::EPROTO)?;
        let mut vdso = maps
            .iter()
            .filter(|mapping| mapping.path.as_deref() == Some(std::path::Path::new("[vdso]")));
        let mapping = vdso
            .next()
            .ok_or_else(|| self.caller_error("target has no special vDSO mapping"))?;
        if vdso.next().is_some()
            || !mapping.readable
            || mapping.writable
            || !mapping.executable
            || mapping.shared
            || mapping.offset != 0
            || mapping.inode != 0
            || mapping.start & (PAGE - 1) != 0
        {
            return Err(self.caller_error("target vDSO mapping identity or protection differs"));
        }
        let len = usize::try_from(
            mapping
                .end
                .checked_sub(mapping.start)
                .ok_or(Errno::EOVERFLOW)?,
        )
        .map_err(|_| Errno::EOVERFLOW)?;
        if len == 0 || len > 64 * 1024 {
            return Err(self.caller_error("target vDSO mapping length is outside bounds"));
        }
        let mut image = vec![0; len];
        task.read_exact(mapping.start as usize, &mut image)?;
        let plan = crate::vdso::plan_stopped_vdso(
            &image,
            mapping.start,
            &self.global_state.subscriptions,
        )?;
        if plan.mapping_start() != mapping.start || plan.mapping_len() != len as u64 {
            return Err(self.caller_error("stopped vDSO plan changed its source mapping"));
        }
        Ok(Some(plan))
    }

    fn restore_after_loader_vdso_words(
        &self,
        task: &mut Stopped,
        plan: &crate::vdso::StoppedVdsoPlan,
        words: &[crate::vdso::StoppedVdsoWordPatch],
        attempted: usize,
    ) -> Result<(), Error> {
        for word in words[..attempted].iter().rev() {
            let read_address = Addr::from_raw(word.address as usize).ok_or(Errno::EFAULT)?;
            let write_address = AddrMut::from_raw(word.address as usize).ok_or(Errno::EFAULT)?;
            let observed: u64 = task.read_value(read_address)?;
            if observed == word.replacement {
                task.write_value(write_address, &word.expected)?;
            } else if observed != word.expected {
                return Err(self.caller_error(format!(
                    "stopped vDSO rollback found an unknown word at {:#x}",
                    word.address
                )));
            }
            let restored: u64 = task.read_value(read_address)?;
            if restored != word.expected {
                return Err(self.caller_error(format!(
                    "stopped vDSO rollback readback differs at {:#x}",
                    word.address
                )));
            }
        }
        let mut restored = vec![0; plan.expected_image().len()];
        task.read_exact(plan.mapping_start() as usize, &mut restored)?;
        if restored != plan.expected_image() {
            return Err(self.caller_error(
                "stopped vDSO rollback did not restore the complete original image",
            ));
        }
        Ok(())
    }

    fn fail_after_loader_vdso_publication(
        &self,
        task: &mut Stopped,
        plan: &crate::vdso::StoppedVdsoPlan,
        words: &[crate::vdso::StoppedVdsoWordPatch],
        attempted: usize,
        original: Error,
    ) -> Error {
        match self.restore_after_loader_vdso_words(task, plan, words, attempted) {
            Ok(()) => original,
            Err(rollback) => self.caller_error(format!(
                "stopped vDSO publication failed: {original}; inverse restoration failed: {rollback}"
            )),
        }
    }

    fn install_after_loader_vdso(
        &self,
        mut task: Stopped,
        plan: &crate::vdso::StoppedVdsoPlan,
    ) -> Result<Stopped, Error> {
        let words = plan.publication_words()?;
        let expected_image = plan.expected_published_image()?;
        let mut attempted = 0;
        for (index, word) in words.iter().enumerate() {
            let read_address = Addr::from_raw(word.address as usize).ok_or(Errno::EFAULT)?;
            let write_address = AddrMut::from_raw(word.address as usize).ok_or(Errno::EFAULT)?;
            let observed: u64 = match task.read_value(read_address) {
                Ok(observed) => observed,
                Err(error) => {
                    let original = self.caller_error(format!(
                        "stopped vDSO word read failed at {:#x}: {error}",
                        word.address
                    ));
                    return Err(self.fail_after_loader_vdso_publication(
                        &mut task, plan, &words, attempted, original,
                    ));
                }
            };
            if observed != word.expected {
                let original = self.caller_error(format!(
                    "stopped vDSO word changed before publication at {:#x}",
                    word.address
                ));
                return Err(self.fail_after_loader_vdso_publication(
                    &mut task, plan, &words, attempted, original,
                ));
            }
            attempted = index + 1;
            if let Err(error) = task.write_value(write_address, &word.replacement) {
                let original = self.caller_error(format!(
                    "stopped vDSO word write failed at {:#x}: {error}",
                    word.address
                ));
                return Err(self.fail_after_loader_vdso_publication(
                    &mut task, plan, &words, attempted, original,
                ));
            }
            let published: u64 = match task.read_value(read_address) {
                Ok(published) => published,
                Err(error) => {
                    let original = self.caller_error(format!(
                        "stopped vDSO word readback failed at {:#x}: {error}",
                        word.address
                    ));
                    return Err(self.fail_after_loader_vdso_publication(
                        &mut task, plan, &words, attempted, original,
                    ));
                }
            };
            if published != word.replacement {
                let original = self.caller_error(format!(
                    "stopped vDSO word readback differs at {:#x}",
                    word.address
                ));
                return Err(self.fail_after_loader_vdso_publication(
                    &mut task, plan, &words, attempted, original,
                ));
            }
        }

        let mut published_image = vec![0; expected_image.len()];
        if let Err(error) = task.read_exact(plan.mapping_start() as usize, &mut published_image) {
            let original = self.caller_error(format!(
                "stopped vDSO complete published image read failed: {error}"
            ));
            return Err(self
                .fail_after_loader_vdso_publication(&mut task, plan, &words, attempted, original));
        }
        if published_image.as_slice() != expected_image.as_ref() {
            let original =
                self.caller_error("stopped vDSO complete published image readback differs");
            return Err(self
                .fail_after_loader_vdso_publication(&mut task, plan, &words, attempted, original));
        }
        if let Err(original) = self.caller_observe(
            "stopped vDSO publication verified",
            format!(
                "mapping={:#x}+{:#x} aligned_words={} legacy_symbols={} getrandom={}",
                plan.mapping_start(),
                plan.mapping_len(),
                words.len(),
                plan.legacy_patch_count(),
                plan.has_getrandom(),
            ),
        ) {
            return Err(self
                .fail_after_loader_vdso_publication(&mut task, plan, &words, attempted, original));
        }
        Ok(task)
    }

    pub(super) async fn run_after_loader(&mut self, mut task: Stopped) -> Result<Stopped, Error> {
        let config = self.after_loader_config().ok_or(Errno::EPROTO)?;
        let image = self.after_loader_identity(&task)?;
        self.caller_quiescent(&task, image)?;
        if image.generation != 1 {
            return Err(self.caller_error("only the first image is supported"));
        }
        let executable = bounded_proc(
            format!("/proc/{}/exe", task.pid()),
            crate::after_loader::MAX_CALLER_FILE,
        )
        .map_err(|e| self.caller_error(e))?;
        if executable.as_slice() != config.executable.bytes.as_ref()
            || image.executable_device != config.executable.file_identity.device
            || image.executable_inode != config.executable.file_identity.inode
        {
            return Err(self.caller_error("executable differs from fixed fixture"));
        }
        let environment = bounded_proc(format!("/proc/{}/environ", task.pid()), 1024 * 1024)
            .map_err(|e| self.caller_error(e))?;
        let actual_environment = environment_map(&environment).map_err(|e| self.caller_error(e))?;
        config
            .validate_environment(&actual_environment)
            .map_err(|e| self.caller_error(e))?;
        for item in environment.split(|b| *b == 0) {
            if item.starts_with(b"LD_PRELOAD=") || item.starts_with(b"LD_AUDIT=") {
                return Err(self.caller_error("first fixture does not support preload/audit callbacks; environment left unchanged"));
            }
        }
        // Retain the complete untouched image before the first private resume.
        // The ordinary tracee-preinit vDSO path is deliberately deferred for
        // after-loader activation, so every later word expectation is rooted in
        // bytes observed at this exact AT_ENTRY stop.
        let vdso_plan = self.plan_after_loader_vdso(&task)?;
        let guard = self
            .liteinst_after_loader_guard
            .take()
            .ok_or(Errno::EPROTO)?;
        let mut word = [0; 8];
        task.read_exact(image.at_entry as usize, &mut word)?;
        let trap = self.caller_trap(&task)?;
        let mut calls =
            Calls::at_entry(guard, &trap, word).map_err(|e| self.caller_error(format!("{e:?}")))?;
        let authenticated_entry = calls
            .take_authenticated_entry()
            .map_err(|error| self.caller_error(format!("{error:?}")))?;
        if authenticated_entry.identity() != image {
            return Err(self.caller_error("authenticated entry identity changed"));
        }
        let mut saved_regs = task.getregs()?;
        saved_regs.rip = image.at_entry;
        let saved_xstate = task.get_x86_extended_state()?;
        let saved_signals = signal_state(task.pid()).map_err(|e| self.caller_error(e))?;
        let saved_descriptors = descriptor_state(task.pid()).map_err(|e| self.caller_error(e))?;
        if self.liteinst_after_loader_private_state.is_some()
            || self.liteinst_after_loader_syscall_permit.is_some()
            || self.liteinst_after_loader_syscall_inflight.is_some()
            || self.liteinst_after_loader_forward_inflight.is_some()
            || self.liteinst_after_loader_private_call.is_some()
        {
            return Err(self.caller_error("private operation state was already present"));
        }
        self.liteinst_after_loader_private_state =
            Some(AfterLoaderPrivateState::new(&task, image)?);
        self.with_restored_liteinst_entry_guard(
            &mut task,
            authenticated_entry,
            |this, stopped| this.bind_after_loader_initial_image_geometries(stopped, &config),
        )?;
        self.initialize_ptmalloc_bootstrap_state(&task, &config)?;
        let timer_suspension = match self.timer.begin_suspend_for_private_execution() {
            Ok(suspension) => suspension,
            Err(error) => {
                return Err(self.caller_error(format!(
                    "suspend deterministic timer for private after-loader execution: {error}"
                )));
            }
        };
        let frozen_timer_clock = timer_suspension.frozen_clock();
        self.liteinst_after_loader_private_state
            .as_mut()
            .ok_or(Errno::EPROTO)?
            .timer_suspension = Some(timer_suspension);
        let complete_timer_suspension = {
            let timer_suspension = self
                .liteinst_after_loader_private_state
                .as_ref()
                .and_then(|state| state.timer_suspension.as_ref())
                .ok_or(Errno::EPROTO)?;
            self.timer
                .complete_suspend_for_private_execution(timer_suspension)
        };
        if let Err(error) = complete_timer_suspension {
            return Err(self.caller_error(format!(
                "complete deterministic timer suspension for private after-loader execution: {error}"
            )));
        }
        let (next, current_break) = self.caller_syscall(task, image, Sysno::brk, [0; 6]).await?;
        task = next;
        self.caller_observe(
            "initial target break",
            format!("address={current_break:#x}"),
        )?;
        let maps = guest_maps(task.pid()).ok_or(Errno::EPROTO)?;
        let stack = maps
            .iter()
            .find(|m| m.readable && m.writable && m.contains(saved_regs.rsp))
            .ok_or(Errno::EPROTO)?;
        let stack_len = usize::try_from(stack.end - stack.start).map_err(|_| Errno::EPROTO)?;
        if stack_len > MAX_STACK_SNAPSHOT {
            return Err(self.caller_error("initial stack exceeds fixture bound"));
        }
        let stack_address = stack.start;
        let mut saved_stack = vec![0; stack_len];
        task.read_exact(stack_address as usize, &mut saved_stack)?;
        let random_address = guest_auxv_entry(task.pid(), libc::AT_RANDOM).ok_or(Errno::EPROTO)?;
        let mut saved_random = [0; 16];
        task.read_exact(random_address as usize, &mut saved_random)?;
        let mut saved_canary = [0; 8];
        task.read_exact(
            saved_regs.fs_base.checked_add(0x28).ok_or(Errno::EPROTO)? as usize,
            &mut saved_canary,
        )?;
        self.caller_observe(
            "entry held",
            format!("image={image:?} regs={:?}", register_words(&saved_regs)),
        )?;
        self.caller_snapshot(&task, "entry", stack_address, stack_len, random_address)?;

        let flags = (libc::MAP_PRIVATE | libc::MAP_ANONYMOUS) as u64;
        let (task, stack_base) = self
            .caller_syscall(
                task,
                image,
                Sysno::mmap,
                [
                    0,
                    STACK_SIZE + 2 * PAGE,
                    libc::PROT_NONE as u64,
                    flags,
                    u64::MAX,
                    0,
                ],
            )
            .await?;
        let (task, _) = self
            .caller_syscall(
                task,
                image,
                Sysno::mprotect,
                [
                    stack_base + PAGE,
                    STACK_SIZE,
                    (libc::PROT_READ | libc::PROT_WRITE) as u64,
                    0,
                    0,
                    0,
                ],
            )
            .await?;
        let (task, data) = self
            .caller_syscall(
                task,
                image,
                Sysno::mmap,
                [
                    0,
                    PAGE,
                    (libc::PROT_READ | libc::PROT_WRITE) as u64,
                    flags,
                    u64::MAX,
                    0,
                ],
            )
            .await?;
        let (task, code_base) = self
            .caller_syscall(
                task,
                image,
                Sysno::mmap,
                [
                    0,
                    PAGE,
                    (libc::PROT_READ | libc::PROT_WRITE) as u64,
                    flags,
                    u64::MAX,
                    0,
                ],
            )
            .await?;
        // Keep the redirect scratch in one physically exact RW anonymous page.
        // The two PROT_NONE guard pages prevent neighboring controller mappings
        // from merging with it while the cache lifecycle is armed.
        let (task, loader_cache_scratch_base) = self
            .caller_syscall(
                task,
                image,
                Sysno::mmap,
                [0, 3 * PAGE, libc::PROT_NONE as u64, flags, u64::MAX, 0],
            )
            .await?;
        let loader_cache_scratch_page = loader_cache_scratch_base
            .checked_add(PAGE)
            .ok_or(Errno::EOVERFLOW)?;
        let (mut task, _) = self
            .caller_syscall(
                task,
                image,
                Sysno::mprotect,
                [
                    loader_cache_scratch_page,
                    PAGE,
                    (libc::PROT_READ | libc::PROT_WRITE) as u64,
                    0,
                    0,
                    0,
                ],
            )
            .await?;
        self.initialize_loader_cache_consumption(&task, &config, loader_cache_scratch_base)?;
        let policy_scratch_address = data
            .checked_add(PRIVATE_POLICY_SCRATCH_OFFSET)
            .ok_or(Errno::EOVERFLOW)?;
        let mut policy_scratch = [0_u8; PRIVATE_POLICY_SCRATCH_BYTES];
        task.read_exact(policy_scratch_address as usize, &mut policy_scratch)?;
        let mut helper_saved_state = LiteinstHelperSavedState {
            cpuid_policy: LiteinstCpuidPolicy::Unsupported,
            tsc_policy: LiteinstTscPolicy::Unsupported,
            regs: task.getregs()?,
            xstate: task.get_x86_extended_state()?,
            stack_address: policy_scratch_address as usize,
            stack_value: u64::from_ne_bytes(policy_scratch),
        };
        let (next, cpuid_policy) = self
            .prepare_liteinst_helper_cpuid(task)
            .await
            .map_err(Error::Internal)?;
        task = next;
        helper_saved_state.cpuid_policy = match cpuid_policy {
            Ok(policy) => policy,
            Err(message) => {
                let original = self.caller_error(format!(
                    "enable native CPUID for private runtime calls: {message}"
                ));
                return Err(self
                    .rollback_liteinst_helper_error(task, &helper_saved_state, original)
                    .await);
            }
        };
        let (next, tsc_policy) = self
            .prepare_liteinst_helper_tsc(task, policy_scratch_address as usize)
            .await
            .map_err(Error::Internal)?;
        task = next;
        helper_saved_state.tsc_policy = match tsc_policy {
            Ok(policy) => policy,
            Err(message) => {
                let original = self.caller_error(format!(
                    "enable native TSC for private runtime calls: {message}"
                ));
                return Err(self
                    .rollback_liteinst_helper_error(task, &helper_saved_state, original)
                    .await);
            }
        };
        // The query itself is an owned, counted private syscall. Never infer
        // active CET state from an ELF GNU property note.
        let (next, _) = self
            .caller_syscall(
                task,
                image,
                Sysno::arch_prctl,
                [ARCH_SHSTK_STATUS, data + 128, 0, 0, 0, 0],
            )
            .await?;
        task = next;
        let mut cet = [0; 8];
        task.read_exact((data + 128) as usize, &mut cet)?;
        if u64::from_ne_bytes(cet) != 0 {
            return Err(self.caller_error("CET active in fixed caller fixture"));
        }
        let (next, _) = self
            .caller_syscall(task, image, Sysno::sigaltstack, [0, data + 160, 0, 0, 0, 0])
            .await?;
        task = next;
        let mut altstack = [0; 24];
        task.read_exact((data + 160) as usize, &mut altstack)?;
        let (next, saved_actions, saved_mask) = self
            .caller_signal_actions(task, image, data, "initial target signal actions")
            .await?;
        task = next;
        let mut path = config
            .sealed_runtime
            .image
            .path
            .as_os_str()
            .as_encoded_bytes()
            .to_vec();
        if path.contains(&0) || path.len() > 2048 {
            return Err(self.caller_error("runtime path outside data bound"));
        }
        path.push(0);
        self.caller_write(&mut task, data, &path)?;
        let (next, runtime_fd) = self
            .caller_syscall(
                task,
                image,
                Sysno::openat,
                [
                    libc::AT_FDCWD as i64 as u64,
                    data,
                    (libc::O_RDONLY | libc::O_CLOEXEC | libc::O_LARGEFILE) as u64,
                    0,
                    0,
                    0,
                ],
            )
            .await?;
        let (next, seals) = self
            .caller_syscall(
                next,
                image,
                Sysno::fcntl,
                [runtime_fd, libc::F_GET_SEALS as u64, 0, 0, 0, 0],
            )
            .await?;
        task = next;
        if seals != crate::after_loader::RUNTIME_SEALS as u64 {
            return Err(self.caller_error("target runtime seals differ; staging unavailable"));
        }
        self.caller_sealed_runtime(&task, &config, runtime_fd)?;
        self.caller_observe(
            "target runtime descriptor opened",
            format!(
                "fd={runtime_fd} device={} inode={} seals={seals:#x}",
                config.sealed_runtime.image.file_identity.device,
                config.sealed_runtime.image.file_identity.inode,
            ),
        )?;
        let path = format!("/proc/self/fd/{runtime_fd}\0");
        self.caller_write(&mut task, data, path.as_bytes())?;
        self.caller_write(
            &mut task,
            data + PRIVATE_HOST_CONFIG_OFFSET,
            &crate::entry_call::host_config(),
        )?;

        let errno_resolver =
            crate::target_loader::resolve_errno_location(&task, &config.provider.bytes)
                .map_err(|e| self.caller_error(e))?;
        let provider_geometry = self
            .after_loader_private_state()?
            .image_geometry(&config.provider)
            .ok_or_else(|| self.caller_error("provider geometry was not bound at entry"))?;
        if errno_resolver.tid != image.tid
            || errno_resolver.start_ticks != image.start_ticks
            || errno_resolver.executable_phdr != image.at_phdr
            || !loader_resolution_matches_geometry(
                errno_resolver.mapping_identity,
                errno_resolver.load_bias,
                provider_geometry,
            )
        {
            return Err(self.caller_error("errno provider identity differs"));
        }
        let errno_code = CallCode::prepare(
            calls.guard(),
            code_base,
            stack_base + PAGE + STACK_SIZE,
            errno_resolver.address,
            RETURN_MARKER + 2,
        )
        .map_err(|e| self.caller_error(format!("{e:?}")))?;
        self.caller_write(&mut task, code_base, &errno_code.bytes)?;
        let (task, _) = self
            .caller_syscall(
                task,
                image,
                Sysno::mprotect,
                [
                    code_base,
                    PAGE,
                    (libc::PROT_READ | libc::PROT_EXEC) as u64,
                    0,
                    0,
                    0,
                ],
            )
            .await?;
        if crate::target_loader::resolve_errno_location(&task, &config.provider.bytes)
            .map_err(|e| self.caller_error(e))?
            != errno_resolver
        {
            return Err(self.caller_error("errno provider changed during preparation"));
        }
        let active_environment = crate::target_loader::observe_environment_before(
            &task,
            &config.provider.bytes,
            &config.environment,
        )
        .map_err(|error| self.caller_error(error))?;
        if active_environment.0.bookkeeping.is_none() {
            return Err(self.caller_error("fixed libc environment bookkeeping is unavailable"));
        }
        self.caller_observe(
            "before errno accessor",
            format!("address={:#x}", errno_resolver.address),
        )?;
        let (task, errno_pointer) = self
            .caller_function(
                task,
                image,
                &config,
                &mut calls,
                &errno_code,
                [0, 0],
                CallerFunction::ErrnoLocation,
            )
            .await?;
        active_environment
            .compare_exact(
                &crate::target_loader::observe_environment_after(
                    &task,
                    &config.provider.bytes,
                    &config.environment,
                )
                .map_err(|error| self.caller_error(error))?,
            )
            .map_err(|error| self.caller_error(error))?;
        let saved_errno = self.caller_errno(&task, errno_pointer)?;
        self.caller_observe(
            "after errno accessor",
            format!("pointer={errno_pointer:#x} bytes={saved_errno:?}"),
        )?;
        let (mut task, _) = self
            .caller_syscall(
                task,
                image,
                Sysno::mprotect,
                [
                    code_base,
                    PAGE,
                    (libc::PROT_READ | libc::PROT_WRITE) as u64,
                    0,
                    0,
                    0,
                ],
            )
            .await?;

        // Final address resolution must follow every preparatory target resume.
        let resolver =
            crate::target_loader::resolve_dlopen(&task, &config.provider.bytes, "GLIBC_2.34")
                .map_err(|e| self.caller_error(e))?;
        if resolver.tid != image.tid
            || resolver.start_ticks != image.start_ticks
            || resolver.executable_phdr != image.at_phdr
            || resolver.mapping_identity != errno_resolver.mapping_identity
            || resolver.link_map != errno_resolver.link_map
            || resolver.load_bias != errno_resolver.load_bias
        {
            return Err(self.caller_error("dlopen identity differs"));
        }
        let code = CallCode::prepare(
            calls.guard(),
            code_base,
            stack_base + PAGE + STACK_SIZE,
            resolver.address,
            RETURN_MARKER,
        )
        .map_err(|e| self.caller_error(format!("{e:?}")))?;
        self.caller_write(&mut task, code_base, &code.bytes)?;
        let (task, _) = self
            .caller_syscall(
                task,
                image,
                Sysno::mprotect,
                [
                    code_base,
                    PAGE,
                    (libc::PROT_READ | libc::PROT_EXEC) as u64,
                    0,
                    0,
                    0,
                ],
            )
            .await?;
        // mprotect resumed the task; revalidate the target provider coordinates.
        let renewed =
            crate::target_loader::resolve_dlopen(&task, &config.provider.bytes, "GLIBC_2.34")
                .map_err(|e| self.caller_error(e))?;
        if renewed != resolver {
            return Err(self.caller_error("dlopen coordinates changed during preparation"));
        }
        self.caller_sealed_runtime(&task, &config, runtime_fd)?;
        self.verify_ptmalloc_bootstrap_before_dlopen(&task, &config)?;
        calls
            .start_dlopen()
            .map_err(|e| self.caller_error(format!("{e:?}")))?;
        self.caller_observe("before dlopen", format!("address={:#x}", resolver.address))?;
        let (task, handle) = self
            .caller_function(
                task,
                image,
                &config,
                &mut calls,
                &code,
                [data, 2],
                CallerFunction::Dlopen,
            )
            .await?;
        self.verify_ptmalloc_bootstrap_retained(&task, &config)?;
        self.bind_after_loader_deferred_image_geometries(&task, &config)?;
        let runtime_id = self
            .after_loader_private_state()?
            .image_id(&config.sealed_runtime.image);
        let runtime_geometry = self.resolve_after_loader_image_geometry(
            &task,
            &config.sealed_runtime.image,
            runtime_id,
            "post-dlopen-runtime",
        )?;
        if !self
            .after_loader_private_state()?
            .has_causal_image_mapping(runtime_geometry)
            || !self
                .after_loader_private_state_mut()?
                .bind_image_geometry(runtime_geometry)
        {
            return Err(self.caller_error(
                "runtime mapping identity or exact geometry differs from its owned mmap",
            ));
        }
        active_environment
            .compare_exact(
                &crate::target_loader::observe_environment_after(
                    &task,
                    &config.provider.bytes,
                    &config.environment,
                )
                .map_err(|error| self.caller_error(error))?,
            )
            .map_err(|error| self.caller_error(error))?;
        self.caller_sealed_runtime(&task, &config, runtime_fd)?;
        if self.caller_errno(&task, errno_pointer)? != saved_errno {
            return Err(self.caller_error("dlopen changed guest errno"));
        }
        self.caller_observe("after dlopen", format!("handle={handle:#x}"))?;

        let (mut task, _) = self
            .caller_syscall(
                task,
                image,
                Sysno::mprotect,
                [
                    code_base,
                    PAGE,
                    (libc::PROT_READ | libc::PROT_WRITE) as u64,
                    0,
                    0,
                    0,
                ],
            )
            .await?;
        let initializer =
            crate::target_loader::resolve_host_initializer(&task, &config.runtime.bytes)
                .map_err(|e| self.caller_error(e))?;
        if initializer.tid != image.tid
            || initializer.start_ticks != image.start_ticks
            || initializer.executable_phdr != image.at_phdr
            || !loader_resolution_matches_geometry(
                initializer.mapping_identity,
                initializer.load_bias,
                runtime_geometry,
            )
        {
            return Err(self.caller_error("initializer identity differs"));
        }
        self.caller_observe(
            "bound runtime image geometry",
            format!(
                "file_device={} file_inode={} mapping_device={:x}:{:x} mapping_inode={} load_bias={:#x} span={:#x}-{:#x}",
                config.sealed_runtime.image.file_identity.device,
                config.sealed_runtime.image.file_identity.inode,
                runtime_geometry.mapping.device_major,
                runtime_geometry.mapping.device_minor,
                runtime_geometry.mapping.inode,
                runtime_geometry.load_bias,
                runtime_geometry.span.0,
                runtime_geometry.span.1,
            ),
        )?;
        let code = CallCode::prepare(
            calls.guard(),
            code_base,
            stack_base + PAGE + STACK_SIZE,
            initializer.address,
            RETURN_MARKER + 1,
        )
        .map_err(|e| self.caller_error(format!("{e:?}")))?;
        self.caller_write(&mut task, code_base, &code.bytes)?;
        let (task, _) = self
            .caller_syscall(
                task,
                image,
                Sysno::mprotect,
                [
                    code_base,
                    PAGE,
                    (libc::PROT_READ | libc::PROT_EXEC) as u64,
                    0,
                    0,
                    0,
                ],
            )
            .await?;
        let renewed = crate::target_loader::resolve_host_initializer(&task, &config.runtime.bytes)
            .map_err(|e| self.caller_error(e))?;
        if renewed != initializer {
            return Err(self.caller_error("initializer coordinates changed"));
        }
        calls
            .start_initializer()
            .map_err(|e| self.caller_error(format!("{e:?}")))?;
        self.caller_observe(
            "before initializer",
            format!("address={:#x}", initializer.address),
        )?;
        self.caller_sealed_runtime(&task, &config, runtime_fd)?;
        let (task, _) = self
            .caller_function(
                task,
                image,
                &config,
                &mut calls,
                &code,
                [data + PRIVATE_HOST_CONFIG_OFFSET, 0],
                CallerFunction::Initializer,
            )
            .await?;
        self.verify_ptmalloc_bootstrap_retained(&task, &config)?;
        active_environment
            .compare_exact(
                &crate::target_loader::observe_environment_after(
                    &task,
                    &config.provider.bytes,
                    &config.environment,
                )
                .map_err(|error| self.caller_error(error))?,
            )
            .map_err(|error| self.caller_error(error))?;
        self.caller_observe("after initializer", format!("{:?}", calls.phase()))?;
        self.caller_snapshot(
            &task,
            "initializer returned",
            stack_address,
            stack_len,
            random_address,
        )?;
        self.caller_sealed_runtime(&task, &config, runtime_fd)?;
        if self.caller_errno(&task, errno_pointer)? != saved_errno {
            return Err(self.caller_error("initializer changed guest errno"));
        }
        let (mut task, _) = self
            .caller_syscall(
                task,
                image,
                Sysno::mprotect,
                [
                    code_base,
                    PAGE,
                    (libc::PROT_READ | libc::PROT_WRITE) as u64,
                    0,
                    0,
                    0,
                ],
            )
            .await?;
        let errno_code = CallCode::prepare(
            calls.guard(),
            code_base,
            stack_base + PAGE + STACK_SIZE,
            errno_resolver.address,
            RETURN_MARKER + 3,
        )
        .map_err(|e| self.caller_error(format!("{e:?}")))?;
        self.caller_write(&mut task, code_base, &errno_code.bytes)?;
        let (task, _) = self
            .caller_syscall(
                task,
                image,
                Sysno::mprotect,
                [
                    code_base,
                    PAGE,
                    (libc::PROT_READ | libc::PROT_EXEC) as u64,
                    0,
                    0,
                    0,
                ],
            )
            .await?;
        if crate::target_loader::resolve_errno_location(&task, &config.provider.bytes)
            .map_err(|e| self.caller_error(e))?
            != errno_resolver
        {
            return Err(self.caller_error("errno provider changed across dlopen"));
        }
        self.caller_observe(
            "before second errno accessor",
            format!("address={:#x}", errno_resolver.address),
        )?;
        let (mut task, after_errno_pointer) = self
            .caller_function(
                task,
                image,
                &config,
                &mut calls,
                &errno_code,
                [0, 0],
                CallerFunction::ErrnoLocation,
            )
            .await?;
        active_environment
            .compare_exact(
                &crate::target_loader::observe_environment_after(
                    &task,
                    &config.provider.bytes,
                    &config.environment,
                )
                .map_err(|error| self.caller_error(error))?,
            )
            .map_err(|error| self.caller_error(error))?;
        if after_errno_pointer != errno_pointer
            || self.caller_errno(&task, after_errno_pointer)? != saved_errno
        {
            return Err(self.caller_error("errno accessor pointer or sentinel changed"));
        }
        self.caller_write(&mut task, errno_pointer, &saved_errno)?;
        self.caller_observe(
            "after second errno accessor",
            format!("pointer={after_errno_pointer:#x} bytes={saved_errno:?}"),
        )?;

        if let Some(plan) = vdso_plan.as_ref() {
            task = self.install_after_loader_vdso(task, plan)?;
        }

        let (mut restored_task, policy_failures) = self
            .restore_liteinst_helper_state(task, &helper_saved_state)
            .await
            .map_err(Error::Internal)?;
        if !policy_failures.is_empty() {
            return Err(self.caller_error(format!(
                "private runtime CPUID/TSC or machine-state restoration failed: {}",
                policy_failures.join("; ")
            )));
        }
        self.caller_write(&mut restored_task, policy_scratch_address, &policy_scratch)?;
        let mut restored_scratch = [0_u8; PRIVATE_POLICY_SCRATCH_BYTES];
        restored_task.read_exact(policy_scratch_address as usize, &mut restored_scratch)?;
        if restored_scratch != policy_scratch {
            return Err(self.caller_error("owned policy scratch restoration differs"));
        }
        task = restored_task;
        // Both controller and target descriptors remain alive through complete
        // provider reauthentication. Closing our target fd removes only that
        // added resource; the dlopen reference and mappings remain retained.
        if crate::target_loader::resolve_host_initializer(&task, &config.runtime.bytes)
            .map_err(|e| self.caller_error(e))?
            != initializer
        {
            return Err(self.caller_error("runtime provider changed after initializer"));
        }
        self.caller_sealed_runtime(&task, &config, runtime_fd)?;
        let (task, _) = self
            .caller_syscall(task, image, Sysno::close, [runtime_fd, 0, 0, 0, 0, 0])
            .await?;
        if !procfs_path_is_absent(format!("/proc/{}/fd/{runtime_fd}", task.pid()))
            .map_err(|error| self.caller_error(error))?
        {
            return Err(self.caller_error("target runtime descriptor survived close"));
        }
        self.caller_observe(
            "target runtime descriptor closed",
            format!("fd={runtime_fd} handle={handle:#x}"),
        )?;
        if descriptor_state(task.pid()).map_err(|e| self.caller_error(e))? != saved_descriptors {
            return Err(self.caller_error("private calls changed original descriptor state"));
        }

        let (task, after_actions, after_mask) = self
            .caller_signal_actions(task, image, data, "final target signal actions")
            .await?;
        if after_actions != saved_actions || after_mask != saved_mask {
            return Err(
                self.caller_error("private calls changed exact target signal actions or mask")
            );
        }
        let (task, _) = self
            .caller_syscall(task, image, Sysno::sigaltstack, [0, data + 192, 0, 0, 0, 0])
            .await?;
        let mut after_altstack = [0; 24];
        task.read_exact((data + 192) as usize, &mut after_altstack)?;
        self.caller_observe(
            "alternate signal stack",
            format!("before={altstack:?} after={after_altstack:?}"),
        )?;
        if altstack_fields(&after_altstack) != altstack_fields(&altstack)
            || signal_state(task.pid()).map_err(|e| self.caller_error(e))? != saved_signals
        {
            return Err(self.caller_error("initializer changed signal state"));
        }
        let mut after_random = [0; 16];
        task.read_exact(random_address as usize, &mut after_random)?;
        let mut after_canary = [0; 8];
        task.read_exact((saved_regs.fs_base + 0x28) as usize, &mut after_canary)?;
        let mut after_stack = vec![0; saved_stack.len()];
        task.read_exact(stack_address as usize, &mut after_stack)?;
        if after_random != saved_random
            || after_canary != saved_canary
            || after_stack != saved_stack
        {
            return Err(
                self.caller_error("initializer changed guest random, canary or original stack")
            );
        }
        if bounded_proc(format!("/proc/{}/environ", task.pid()), 1024 * 1024)
            .map_err(|e| self.caller_error(e))?
            != environment
        {
            return Err(self.caller_error("initializer changed original environment bytes"));
        }
        self.prepare_loader_cache_scratch_release(&task, &config, loader_cache_scratch_base)?;
        let (task, _) = self
            .caller_syscall(
                task,
                image,
                Sysno::munmap,
                [loader_cache_scratch_base, 3 * PAGE, 0, 0, 0, 0],
            )
            .await?;
        self.complete_loader_cache_scratch_release(&task, &config, loader_cache_scratch_base)?;
        let (task, _) = self
            .caller_syscall(task, image, Sysno::munmap, [code_base, PAGE, 0, 0, 0, 0])
            .await?;
        let (task, _) = self
            .caller_syscall(task, image, Sysno::munmap, [data, PAGE, 0, 0, 0, 0])
            .await?;
        let (mut task, _) = self
            .caller_syscall(
                task,
                image,
                Sysno::munmap,
                [stack_base, STACK_SIZE + 2 * PAGE, 0, 0, 0, 0],
            )
            .await?;
        self.restore_liteinst_entry_guard(&mut task)?;
        task.set_x86_extended_state(&saved_xstate)?;
        task.setregs(&saved_regs)?;
        let mut restored_word = [0; 8];
        task.read_exact(image.at_entry as usize, &mut restored_word)?;
        if restored_word != calls.guard().original()
            || task.get_x86_extended_state()? != saved_xstate
            || register_words(&task.getregs()?) != register_words(&saved_regs)
        {
            return Err(self.caller_error("entry or complete machine state readback differs"));
        }
        self.caller_quiescent(&task, image)?;
        if self.caller_errno(&task, errno_pointer)? != saved_errno {
            return Err(self.caller_error("restored errno readback differs"));
        }
        self.caller_snapshot(
            &task,
            "restored entry",
            stack_address,
            stack_len,
            random_address,
        )?;
        active_environment
            .compare_exact(
                &crate::target_loader::observe_environment_after(
                    &task,
                    &config.provider.bytes,
                    &config.environment,
                )
                .map_err(|error| self.caller_error(error))?,
            )
            .map_err(|error| self.caller_error(error))?;
        if self.liteinst_after_loader_syscall_permit.is_some()
            || self.liteinst_after_loader_syscall_inflight.is_some()
            || self.liteinst_after_loader_forward_inflight.is_some()
            || self.liteinst_after_loader_private_call.is_some()
        {
            return Err(self.caller_error("private syscall or call authority survived restoration"));
        }
        self.validate_after_loader_retained_resources(&task, &config, None)?;
        self.caller_quiescent(&task, image)?;
        let (prepared_arenas, prepared_reservations) = self
            .after_loader_private_state()?
            .prepared_liteinst_controls()
            .ok_or_else(|| {
                self.caller_error("retained LiteInst arena controls are not exact and bijective")
            })?;
        let frame = self
            .liteinst_runtime
            .lock()
            .unwrap()
            .frame
            .ok_or(Errno::EPROTO)?;
        let callback_stack = liteinst_callback_stack_allocation(frame).ok_or_else(|| {
            self.caller_error("authenticated callback-stack allocation is invalid")
        })?;
        let helper_code = bind_liteinst_helper_code(&task, frame).ok_or_else(|| {
            self.caller_error("retained LiteInst helper page could not be bound exactly")
        })?;
        let helper_isolation = self
            .after_loader_private_state()?
            .bind_helper_isolation(&config.sealed_runtime.image, &initializer, &helper_code)
            .ok_or_else(|| {
                self.caller_error(
                    "retained helper page does not equal its exact sealed-runtime file page",
                )
            })?;
        if !liteinst_helper_code_has_protection(
            &task,
            &helper_code,
            libc::PROT_READ | libc::PROT_EXEC,
        ) || !liteinst_helper_code_bytes_match(&task, &helper_code)
        {
            return Err(self.caller_error(
                "retained LiteInst helper page is not the exact sealed-runtime RX page",
            ));
        }
        {
            let runtime = self.liteinst_runtime.lock().unwrap();
            if !after_loader_liteinst_ready_is_publishable(
                &runtime,
                image.generation,
                frame,
                &prepared_arenas,
                &prepared_reservations,
                &helper_code,
                callback_stack,
                &initializer,
            ) {
                return Err(self.caller_error(
                    "initializer runtime state is not an empty Bootstrap publication",
                ));
            }
        }

        let (next, helper_protection_result) = self
            .set_liteinst_internal_protection(task, helper_code.range, libc::PROT_NONE)
            .await
            .map_err(Error::Internal)?;
        task = next;
        task.set_x86_extended_state(&saved_xstate)?;
        task.setregs(&saved_regs)?;
        self.caller_quiescent(&task, image)?;
        if helper_protection_result != Ok(0)
            || !liteinst_helper_code_has_protection(&task, &helper_code, libc::PROT_NONE)
            || !liteinst_helper_code_bytes_match(&task, &helper_code)
            || !helper_isolation.validates_live_page(&task)
        {
            return Err(self.caller_error(format!(
                "isolate retained LiteInst helper page: mprotect result {helper_protection_result:?} or exact map/byte readback differed"
            )));
        }
        let mut post_isolation_entry = [0_u8; 8];
        task.read_exact(image.at_entry as usize, &mut post_isolation_entry)?;
        let mut post_isolation_private_stub = [0_u8; 4];
        task.read_exact(cp::PRIVATE_PAGE_OFFSET, &mut post_isolation_private_stub)?;
        if post_isolation_entry != calls.guard().original()
            || post_isolation_private_stub != [0x0f, 0x05, 0x0f, 0x0b]
            || task.get_x86_extended_state()? != saved_xstate
            || register_words(&task.getregs()?) != register_words(&saved_regs)
        {
            return Err(self.caller_error(
                "helper-page isolation changed the entry word, private stub or complete machine state",
            ));
        }
        self.liteinst_after_loader_private_state
            .as_mut()
            .ok_or(Errno::EPROTO)?
            .protect_owned_range(
                (helper_isolation.range.start, helper_isolation.range.end),
                libc::PROT_NONE,
            );
        let (next, arena_aliases_isolated) = self
            .set_liteinst_arena_writable_protection(task, &prepared_arenas, libc::PROT_NONE)
            .await
            .map_err(Error::Internal)?;
        task = next;
        task.set_x86_extended_state(&saved_xstate)?;
        task.setregs(&saved_regs)?;
        self.caller_quiescent(&task, image)?;
        let restored_xstate = task.get_x86_extended_state().map_err(|error| {
            self.caller_error(format!(
                "read restored x86 extended state after final arena-alias isolation: {error}"
            ))
        })?;
        let restored_regs = task.getregs().map_err(|error| {
            self.caller_error(format!(
                "read restored general registers after final arena-alias isolation: {error}"
            ))
        })?;
        if restored_xstate != saved_xstate
            || register_words(&restored_regs) != register_words(&saved_regs)
        {
            return Err(self.caller_error(
                "final arena-alias isolation changed the restored x86 extended state or general registers",
            ));
        }
        if !arena_aliases_isolated {
            return Err(self.caller_error(
                "isolate retained LiteInst arena writable aliases: PROT_NONE transition or exact map readback differed",
            ));
        }
        for arena in &prepared_arenas {
            self.liteinst_after_loader_private_state
                .as_mut()
                .ok_or(Errno::EPROTO)?
                .protect_owned_range((arena.writable.start, arena.writable.end), libc::PROT_NONE);
        }
        self.validate_after_loader_retained_resources(&task, &config, Some(&helper_isolation))?;
        self.verify_ptmalloc_bootstrap_retained(&task, &config)?;
        let projection = helper_isolation.target_loader_projection().ok_or_else(|| {
            self.caller_error("isolated helper page cannot form an exact target-loader projection")
        })?;
        let renewed_initializer =
            crate::target_loader::resolve_host_initializer_with_isolated_page(
                &task,
                &config.runtime.bytes,
                projection,
                |address, bytes| {
                    read_exact_with_helper_isolation(&task, address, bytes, Some(&helper_isolation))
                        .map_err(|error| io::Error::other(error.to_string()))
                },
            )
            .map_err(|error| self.caller_error(error))?;
        if renewed_initializer != initializer {
            return Err(self.caller_error(
                "sealed-runtime initializer coordinates changed after helper-page isolation",
            ));
        }
        let initializer = renewed_initializer;
        self.caller_quiescent(&task, image)?;
        if self.timer.diagnostic_clock() != Some(frozen_timer_clock) {
            return Err(
                self.caller_error("deterministic clock changed during final helper-page isolation")
            );
        }
        self.caller_observe(
            "guest machine state restored and helper/arena writers isolated",
            format!(
                "retained dlopen handle={handle:#x} helper={:#x}-{:#x} arenas={} reservations={}",
                helper_code.range.start,
                helper_code.range.end,
                prepared_arenas.len(),
                prepared_reservations.len(),
            ),
        )?;
        calls
            .restored()
            .map_err(|e| self.caller_error(format!("{e:?}")))?;
        let restore_timer = {
            let timer_suspension = self
                .liteinst_after_loader_private_state
                .as_ref()
                .and_then(|state| state.timer_suspension.as_ref())
                .ok_or(Errno::EPROTO)?;
            self.timer.restore_after_private_execution(timer_suspension)
        };
        let timer_ownership = {
            let state = self
                .liteinst_after_loader_private_state
                .as_mut()
                .ok_or(Errno::EPROTO)?;
            settle_private_timer_restore(&mut state.timer_suspension, restore_timer)
        };
        match timer_ownership {
            Ok(()) => {}
            Err(PrivateTimerRestoreOwnershipError::Restore(error)) => {
                return Err(self.caller_error(format!(
                    "restore deterministic timer after private after-loader execution: {error}"
                )));
            }
            Err(PrivateTimerRestoreOwnershipError::MissingToken) => {
                return Err(Errno::EPROTO.into());
            }
        }
        if self.timer.diagnostic_clock() != Some(frozen_timer_clock) {
            return Err(self.caller_error(
                "deterministic clock changed across private after-loader execution",
            ));
        }
        self.verify_ptmalloc_bootstrap_retained(&task, &config)?;
        self.validate_loader_cache_released(&task, &config)?;
        let mut runtime = self.liteinst_runtime.lock().unwrap();
        if !after_loader_liteinst_ready_is_publishable(
            &runtime,
            image.generation,
            frame,
            &prepared_arenas,
            &prepared_reservations,
            &helper_code,
            callback_stack,
            &initializer,
        ) {
            return Err(self.caller_error(
                "initializer runtime state changed before atomic Ready publication",
            ));
        }
        if self
            .liteinst_after_loader_private_state
            .as_ref()
            .is_none_or(|state| state.timer_suspension.is_some())
        {
            return Err(
                self.caller_error("Ready publication retained consumed timer suspension authority")
            );
        }
        let validated_state = self
            .liteinst_after_loader_private_state
            .take()
            .ok_or(Errno::EPROTO)?;
        debug_assert!(validated_state.timer_suspension.is_none());
        drop(validated_state);
        commit_after_loader_liteinst_ready(
            &mut runtime,
            image.generation,
            prepared_arenas,
            prepared_reservations,
            helper_code,
            callback_stack,
            handle,
            initializer,
        );
        Ok(task)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn zeroed_test_xstate() -> safeptrace::X86ExtendedState {
        // SAFETY: libc's x86 floating-point register type is an integer/array
        // storage image, so the all-zero bit pattern is valid test data.
        safeptrace::X86ExtendedState::Fxsave64(Box::new(unsafe { core::mem::zeroed() }))
    }

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    enum PostRestoreFailure {
        DiagnosticClock,
        ReadyPublication,
    }

    #[test]
    fn timer_restore_ownership_is_consumed_before_post_restore_failures() {
        for failure in [
            PostRestoreFailure::DiagnosticClock,
            PostRestoreFailure::ReadyPublication,
        ] {
            let mut token = Some(41_u64);
            assert_eq!(
                settle_private_timer_restore(&mut token, Ok::<_, ()>(())),
                Ok(())
            );
            let post_restore_check: Result<(), PostRestoreFailure> = Err(failure);
            assert_eq!(post_restore_check, Err(failure));
            assert!(
                token.is_none(),
                "{failure:?} retained stale timer authority"
            );
            assert!(
                token.take().is_none(),
                "terminal cleanup could acquire a second retirement token after {failure:?}",
            );
        }
    }

    #[test]
    fn timer_restore_failure_retains_terminal_retirement_authority() {
        let mut token = Some(73_u64);
        assert_eq!(
            settle_private_timer_restore(&mut token, Err("restore failed")),
            Err(PrivateTimerRestoreOwnershipError::Restore("restore failed")),
        );
        assert_eq!(token, Some(73));
    }

    fn mapping_state(generation: u64) -> AfterLoaderPrivateState {
        AfterLoaderPrivateState {
            image: ImageIdentity {
                tid: 17,
                start_ticks: 23,
                generation,
                executable_device: 29,
                executable_inode: 31,
                at_entry: 0x401000,
                at_phdr: 0x400040,
            },
            original_mappings: Vec::new(),
            original_descriptors: BTreeSet::new(),
            owned_descriptors: BTreeMap::new(),
            proc_fd_audit: ProcFdAuditLifecycle::AwaitingOpen,
            owned_mappings: Vec::new(),
            current_break: None,
            private_brk_growth_consumed: false,
            shared_reservations: BTreeMap::new(),
            protected_ranges: Vec::new(),
            image_mappings: BTreeMap::new(),
            image_geometries: BTreeMap::new(),
            trampoline_mappings: BTreeMap::new(),
            trampoline_seals_added: BTreeSet::new(),
            sealed_trampolines: BTreeSet::new(),
            next_trampoline_serial: 0,
            ptmalloc_bootstrap: None,
            loader_cache: None,
            timer_suspension: None,
        }
    }

    fn controller_mapping(start: u64, end: u64) -> AfterLoaderOwnedMapping {
        AfterLoaderOwnedMapping {
            start,
            end,
            readable: true,
            writable: true,
            executable: false,
            shared: false,
            descriptor: None,
            offset: 0,
            purpose: AfterLoaderMappingPurpose::Controller,
        }
    }

    fn callback_stack_mapping(start: u64, usable_len: u64) -> AfterLoaderOwnedMapping {
        AfterLoaderOwnedMapping {
            start,
            end: start + usable_len + 2 * PAGE,
            readable: false,
            writable: false,
            executable: false,
            shared: false,
            descriptor: None,
            offset: 0,
            purpose: AfterLoaderMappingPurpose::CallbackStack,
        }
    }

    #[test]
    fn callback_stack_transition_requires_exact_raw_shape_and_preserves_anonymous_offsets() {
        let start = 0x70_0000;
        let usable_len = CALLBACK_STACK_MIN_USABLE_BYTES;
        let mapping_len = usable_len + 2 * PAGE;
        let usable = (start + PAGE, start + PAGE + usable_len);
        let flags = libc::MAP_PRIVATE | libc::MAP_ANONYMOUS | libc::MAP_STACK;
        let mmap_args = [
            0,
            mapping_len,
            libc::PROT_NONE as u64,
            flags as u64,
            u64::MAX,
            0,
        ];
        assert!(exact_callback_stack_mmap_shape(
            mmap_args,
            libc::PROT_NONE,
            flags,
        ));
        for changed in [
            [1, mmap_args[1], mmap_args[2], mmap_args[3], mmap_args[4], 0],
            [
                0,
                mapping_len - 1,
                mmap_args[2],
                mmap_args[3],
                mmap_args[4],
                0,
            ],
            [0, mmap_args[1], mmap_args[2], mmap_args[3], 0, 0],
            [
                0,
                mmap_args[1],
                mmap_args[2],
                mmap_args[3],
                mmap_args[4],
                PAGE,
            ],
        ] {
            assert!(!exact_callback_stack_mmap_shape(
                changed,
                libc::PROT_NONE,
                flags,
            ));
        }
        assert!(!exact_callback_stack_mmap_shape(
            mmap_args,
            libc::PROT_READ,
            flags,
        ));
        assert!(!exact_callback_stack_mmap_shape(
            mmap_args,
            libc::PROT_NONE,
            flags ^ libc::MAP_STACK,
        ));
        let mut maximum = mmap_args;
        maximum[1] = CALLBACK_STACK_MAX_USABLE_BYTES + 2 * PAGE;
        assert!(exact_callback_stack_mmap_shape(
            maximum,
            libc::PROT_NONE,
            flags,
        ));
        for refused_length in [
            CALLBACK_STACK_MIN_USABLE_BYTES - PAGE,
            CALLBACK_STACK_MIN_USABLE_BYTES + 1,
            CALLBACK_STACK_MAX_USABLE_BYTES + PAGE,
        ] {
            let mut refused = mmap_args;
            refused[1] = refused_length + 2 * PAGE;
            assert!(!exact_callback_stack_mmap_shape(
                refused,
                libc::PROT_NONE,
                flags,
            ));
        }
        let mut state = mapping_state(1);
        state
            .owned_mappings
            .push(callback_stack_mapping(start, usable_len));

        assert!(state.callback_stack_protect_transition_is_exact(
            usable,
            usable_len,
            libc::PROT_READ | libc::PROT_WRITE,
        ));
        for raw_length in [usable_len - 1, usable_len + 1] {
            assert!(
                !state.callback_stack_protect_transition_is_exact(
                    usable,
                    raw_length,
                    libc::PROT_READ | libc::PROT_WRITE,
                ),
                "page-rounded raw-length near miss was accepted"
            );
        }
        for range in [(usable.0 - PAGE, usable.1), (usable.0, usable.1 + PAGE)] {
            assert!(!state.callback_stack_protect_transition_is_exact(
                range,
                usable_len,
                libc::PROT_READ | libc::PROT_WRITE,
            ));
        }
        assert!(!state.callback_stack_protect_transition_is_exact(
            usable,
            usable_len,
            libc::PROT_READ,
        ));

        assert!(state.protect_callback_stack(usable));
        assert_eq!(
            state.callback_stack_range(),
            GuestRange::new(usable.0, usable_len)
        );
        let mut segments = state
            .owned_mappings
            .iter()
            .filter(|mapping| mapping.purpose == AfterLoaderMappingPurpose::CallbackStack)
            .collect::<Vec<_>>();
        segments.sort_by_key(|mapping| mapping.start);
        assert_eq!(segments.len(), 3);
        assert!(segments.iter().all(|mapping| mapping.offset == 0));
        assert!(!state.protect_callback_stack(usable));

        let mut changed = state.clone();
        changed.owned_mappings[1].executable = true;
        assert!(changed.callback_stack_range().is_none());
        let mut missing = state;
        missing.owned_mappings.pop();
        assert!(missing.callback_stack_range().is_none());
    }

    #[test]
    fn helper_isolation_binding_refuses_inexact_owned_provenance() {
        let source = ["/usr/bin/true", "/bin/true"]
            .into_iter()
            .map(Path::new)
            .find(|path| path.is_file())
            .unwrap();
        let runtime = LiteinstCallerImage::read(source).unwrap();
        assert!(runtime.bytes.len() >= 2 * PAGE as usize);

        let mut state = mapping_state(11);
        let image = state.image_id(&runtime);
        let mapping = MappingIdentity {
            device_major: 8,
            device_minor: 1,
            inode: 37,
        };
        let load_bias = 0x70_0000;
        let geometry = ResolvedImageGeometry {
            image,
            mapping,
            load_bias,
            span: (load_bias, load_bias + 4 * PAGE),
        };
        assert!(state.bind_image_geometry(geometry));

        let original_mapping = GuestMap {
            start: geometry.span.0,
            end: geometry.span.1,
            offset: 0,
            device_major: mapping.device_major,
            device_minor: mapping.device_minor,
            readable: true,
            writable: false,
            executable: true,
            shared: false,
            inode: mapping.inode,
            path: Some(runtime.path.clone()),
        };
        let helper = LiteinstHelperCode {
            range: GuestRange::new(load_bias + PAGE, PAGE).unwrap(),
            original_mapping: original_mapping.clone(),
            bytes: runtime.bytes[PAGE as usize..2 * PAGE as usize].to_vec(),
        };
        let initializer = crate::target_loader::TargetHostInitializer {
            tid: state.image.tid,
            start_ticks: state.image.start_ticks,
            executable_phdr: state.image.at_phdr,
            link_map: 0x60_0000,
            load_bias,
            address: load_bias + 0x100,
            mapping_identity: mapping.as_target_loader(),
        };
        let owned = AfterLoaderOwnedMapping {
            start: original_mapping.start,
            end: original_mapping.end,
            readable: true,
            writable: false,
            executable: true,
            shared: false,
            descriptor: Some(9),
            offset: 0,
            purpose: AfterLoaderMappingPurpose::Image { image },
        };
        state.owned_mappings.push(owned);
        assert!(
            state
                .bind_helper_isolation(&runtime, &initializer, &helper)
                .is_some(),
            "exact sealed-runtime helper provenance was refused"
        );

        let mut missing = state.clone();
        missing.owned_mappings.clear();
        assert!(
            missing
                .bind_helper_isolation(&runtime, &initializer, &helper)
                .is_none(),
            "missing owned image mapping was accepted"
        );

        let mut duplicate = state.clone();
        duplicate.owned_mappings.push(owned);
        assert!(
            duplicate
                .bind_helper_isolation(&runtime, &initializer, &helper)
                .is_none(),
            "duplicate owned image mapping was accepted"
        );

        let mut wrong_purpose = state.clone();
        wrong_purpose.owned_mappings[0].purpose = AfterLoaderMappingPurpose::Controller;
        assert!(
            wrong_purpose
                .bind_helper_isolation(&runtime, &initializer, &helper)
                .is_none(),
            "controller-owned mapping was accepted as sealed-runtime provenance"
        );

        let mut wrong_offset = state.clone();
        wrong_offset.owned_mappings[0].offset = PAGE;
        assert!(
            wrong_offset
                .bind_helper_isolation(&runtime, &initializer, &helper)
                .is_none(),
            "owned mapping with a mismatched file offset was accepted"
        );

        let mut wrong_identity = helper.clone();
        wrong_identity.original_mapping.inode ^= 1;
        assert!(
            state
                .bind_helper_isolation(&runtime, &initializer, &wrong_identity)
                .is_none(),
            "helper mapping with a mismatched identity was accepted"
        );

        let mut wrong_expected_bytes = helper;
        wrong_expected_bytes.bytes[0] ^= 1;
        assert!(
            state
                .bind_helper_isolation(&runtime, &initializer, &wrong_expected_bytes)
                .is_none(),
            "helper bytes differing from the sealed runtime were accepted"
        );
    }

    #[test]
    fn canonical_c_int_arguments_refuse_mixed_high_words() {
        for (raw, expected) in [
            (0x0000_0000_ffff_ff9c, libc::AT_FDCWD),
            (0xffff_ffff_ffff_ff9c, libc::AT_FDCWD),
            (0x0000_0000_ffff_ffff, -1),
            (0xffff_ffff_ffff_ffff, -1),
            (libc::O_CLOEXEC as u64, libc::O_CLOEXEC),
        ] {
            assert_eq!(canonical_c_int_argument(raw), Some(expected));
        }

        for raw in [
            0x0000_0001_ffff_ff9c,
            0xffff_fffe_ffff_ff9c,
            0xdead_beef_ffff_ff9c,
            0x0000_0001_ffff_ffff,
            0xffff_fffe_ffff_ffff,
            (1_u64 << 32) | libc::O_CLOEXEC as u64,
        ] {
            assert_eq!(
                canonical_c_int_argument(raw),
                None,
                "accepted noncanonical C int {raw:#018x}"
            );
        }
    }

    #[test]
    fn trace_only_classification_rejects_mixed_high_and_x32_before_forwarding() {
        for low in [
            libc::SYS_rt_sigreturn,
            libc::SYS_execve,
            libc::SYS_rt_sigaction,
            libc::SYS_mmap,
            libc::SYS_munmap,
            libc::SYS_mprotect,
        ] {
            assert_eq!(
                classify_trace_only_syscall_number((1_u64 << 32) | low as u64),
                Err(Errno::ENOSYS)
            );
        }
        for low in [512_u64, 513_u64, libc::SYS_getpid as u64] {
            assert_eq!(
                classify_trace_only_syscall_number(low | X32_SYSCALL_BIT),
                Err(Errno::ENOSYS)
            );
        }
        assert_eq!(
            classify_trace_only_syscall_number(libc::SYS_rt_sigreturn as u64),
            Ok((libc::SYS_rt_sigreturn, Some(Sysno::rt_sigreturn),))
        );
    }

    #[test]
    fn semantic_admission_ignores_only_registers_beyond_the_syscall_arity() {
        let extra = [0x1111, 0x2222, 0x3333, 0x4444, 0x5555];
        assert!(controller_semantic_arguments_match(
            libc::SYS_arch_prctl,
            [
                ARCH_SHSTK_STATUS,
                0x4000,
                extra[1],
                extra[2],
                extra[3],
                extra[4]
            ],
        ));
        assert!(controller_semantic_arguments_match(
            libc::SYS_sigaltstack,
            [0, 0x5000, extra[1], extra[2], extra[3], extra[4]],
        ));
        assert!(controller_semantic_arguments_match(
            libc::SYS_rt_sigaction,
            [libc::SIGUSR1 as u64, 0, 0x6000, 8, extra[3], extra[4]],
        ));
        assert!(controller_semantic_arguments_match(
            libc::SYS_rt_sigprocmask,
            [libc::SIG_SETMASK as u64, 0, 0x7000, 8, extra[3], extra[4]],
        ));
        assert!(controller_semantic_arguments_match(
            libc::SYS_brk,
            [0, extra[0], extra[1], extra[2], extra[3], extra[4]],
        ));
        assert!(controller_semantic_arguments_match(
            libc::SYS_fcntl,
            [9, libc::F_GET_SEALS as u64, 0, extra[2], extra[3], extra[4]],
        ));
        assert!(controller_semantic_arguments_match(
            libc::SYS_close,
            [9, extra[0], extra[1], extra[2], extra[3], extra[4]],
        ));
        assert!(initializer_fcntl_add_seals_arguments_match([
            9,
            libc::F_ADD_SEALS as u64,
            TRAMPOLINE_SEALS as u64,
            extra[2],
            extra[3],
            extra[4],
        ]));

        assert!(!controller_semantic_arguments_match(
            libc::SYS_arch_prctl,
            [ARCH_SHSTK_STATUS + 1, 0x4000, 0, 0, 0, 0],
        ));
        assert!(!controller_semantic_arguments_match(
            libc::SYS_rt_sigaction,
            [libc::SIGUSR1 as u64, 0, 0x6000, 16, 0, 0],
        ));
        assert!(!controller_semantic_arguments_match(
            libc::SYS_fcntl,
            [9, libc::F_GET_SEALS as u64, 1, 0, 0, 0],
        ));
        assert!(!initializer_fcntl_add_seals_arguments_match([
            9,
            libc::F_ADD_SEALS as u64,
            (TRAMPOLINE_SEALS as u64) ^ 1,
            0,
            0,
            0,
        ]));

        let exact_futex = [
            0x8000,
            (libc::FUTEX_WAKE | libc::FUTEX_PRIVATE_FLAG) as u64,
            1,
            0,
            0,
            0,
        ];
        assert!(initializer_futex_arguments_match(exact_futex));
        for index in 3..6 {
            let mut smuggled = exact_futex;
            smuggled[index] = index as u64;
            assert!(
                !initializer_futex_arguments_match(smuggled),
                "accepted within-arity futex register {index}"
            );
        }
    }

    #[test]
    fn raw_permit_comparator_pins_ignored_registers_across_stops() {
        let number = libc::SYS_arch_prctl;
        let args = [ARCH_SHSTK_STATUS, 0x4000, 11, 22, 33, 44];
        assert!(controller_semantic_arguments_match(number, args));
        let mut registers = libc::user_regs_struct {
            orig_rax: number as u64,
            rdi: args[0],
            rsi: args[1],
            rdx: args[2],
            r10: args[3],
            r8: args[4],
            r9: args[5],
            ..unsafe { core::mem::zeroed() }
        };
        assert!(after_loader_syscall_registers_match(
            number, args, &registers
        ));
        for index in 2..6 {
            let original = registers;
            match index {
                2 => registers.rdx ^= 1,
                3 => registers.r10 ^= 1,
                4 => registers.r8 ^= 1,
                5 => registers.r9 ^= 1,
                _ => unreachable!(),
            }
            assert!(
                !after_loader_syscall_registers_match(number, args, &registers),
                "permit accepted changed ignored register {index}"
            );
            registers = original;
        }
        registers.orig_rax ^= 1;
        assert!(!after_loader_syscall_registers_match(
            number, args, &registers
        ));
    }

    #[test]
    fn readonly_openat_shape_keeps_flags_mode_and_high_words_exact() {
        let expected_flags = (libc::O_RDONLY | libc::O_CLOEXEC | libc::O_LARGEFILE) as u64;
        let mut args = [
            0x0000_0000_ffff_ff9c,
            0x1234,
            expected_flags,
            0,
            0xfeed_face,
            0xdead_beef,
        ];
        assert!(exact_readonly_openat_arguments(&args));
        args[0] = 0xffff_ffff_ffff_ff9c;
        assert!(exact_readonly_openat_arguments(&args));

        for dirfd in [
            0x0000_0001_ffff_ff9c,
            0xffff_fffe_ffff_ff9c,
            0xdead_beef_ffff_ff9c,
            0x0000_0000_ffff_ff9b,
            0xffff_ffff_ffff_ff9d,
            3,
        ] {
            let mut changed = args;
            changed[0] = dirfd;
            assert!(!exact_readonly_openat_arguments(&changed));
        }
        for flags in [
            expected_flags & !(libc::O_CLOEXEC as u64),
            expected_flags | libc::O_WRONLY as u64,
            expected_flags | libc::O_CREAT as u64,
            expected_flags | libc::O_PATH as u64,
            expected_flags | (1_u64 << 32),
        ] {
            let mut changed = args;
            changed[2] = flags;
            assert!(!exact_readonly_openat_arguments(&changed));
        }
        for mode in [1, 1_u64 << 32, u64::MAX] {
            let mut changed = args;
            changed[3] = mode;
            assert!(!exact_readonly_openat_arguments(&changed));
        }
    }

    #[test]
    fn proc_fd_audit_argument_shapes_and_output_spans_are_exact() {
        let directory_flags = (libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC) as u64;
        let open = [
            libc::AT_FDCWD as i64 as u64,
            0x70_1000,
            directory_flags,
            0,
            0,
            0,
        ];
        assert!(exact_proc_fd_directory_openat_arguments(&open));
        let mut zero_extended = open;
        zero_extended[0] = u64::from(libc::AT_FDCWD as u32);
        assert!(exact_proc_fd_directory_openat_arguments(&zero_extended));
        for index in [0, 2, 3, 4, 5] {
            let mut changed = open;
            changed[index] ^= 1;
            assert!(
                !exact_proc_fd_directory_openat_arguments(&changed),
                "proc-fd open mutation {index} was admitted",
            );
        }
        let mut null_path = open;
        null_path[1] = 0;
        assert!(!exact_proc_fd_directory_openat_arguments(&null_path));
        for flags in [
            directory_flags & !(libc::O_DIRECTORY as u64),
            directory_flags & !(libc::O_CLOEXEC as u64),
            directory_flags | libc::O_WRONLY as u64,
            directory_flags | libc::O_CREAT as u64,
            directory_flags | libc::O_PATH as u64,
            directory_flags | (1_u64 << 32),
        ] {
            let mut changed = open;
            changed[2] = flags;
            assert!(!exact_proc_fd_directory_openat_arguments(&changed));
        }

        let getdents = [41, 0x70_2000, PROC_FD_GETDENTS_BYTES, 0, 0, 0];
        assert!(exact_proc_fd_getdents_arguments(&getdents));
        assert_eq!(
            syscall_output_spans(libc::SYS_getdents64, getdents),
            Ok(vec![(0x70_2000, 0x70_2000 + PROC_FD_GETDENTS_BYTES)])
        );
        for index in [2, 3, 4, 5] {
            let mut changed = getdents;
            changed[index] ^= 1;
            assert!(
                !exact_proc_fd_getdents_arguments(&changed),
                "proc-fd getdents mutation {index} was admitted",
            );
        }
        let mut null_buffer = getdents;
        null_buffer[1] = 0;
        assert!(!exact_proc_fd_getdents_arguments(&null_buffer));

        let readlink = [
            libc::AT_FDCWD as i64 as u64,
            0x70_3000,
            0x70_4000,
            PROC_FD_READLINK_BYTES,
            0,
            0,
        ];
        assert!(exact_proc_fd_readlink_arguments(&readlink));
        assert_eq!(
            syscall_output_spans(libc::SYS_readlinkat, readlink),
            Ok(vec![(0x70_4000, 0x70_4000 + PROC_FD_READLINK_BYTES)])
        );
        for index in [0, 3, 4, 5] {
            let mut changed = readlink;
            changed[index] ^= 1;
            assert!(
                !exact_proc_fd_readlink_arguments(&changed),
                "proc-fd readlink mutation {index} was admitted",
            );
        }
        for index in [1, 2] {
            let mut changed = readlink;
            changed[index] = 0;
            assert!(!exact_proc_fd_readlink_arguments(&changed));
        }
        assert_eq!(proc_self_fd_number(b"/proc/self/fd/0"), Some(0));
        assert_eq!(proc_self_fd_number(b"/proc/self/fd/73"), Some(73));
        for path in [
            b"/proc/self/fd/".as_slice(),
            b"/proc/self/fd/00",
            b"/proc/self/fd/+1",
            b"/proc/17/fd/1",
            b"/proc/self/fd/2147483648",
        ] {
            assert_eq!(proc_self_fd_number(path), None, "path={path:?}");
        }
    }

    #[test]
    fn proc_fd_audit_site_call_chain_and_stack_ranges_are_exact() {
        let profile = ProcFdAuditProfile {
            function_start: 0x1000,
            function_end: 0x2000,
            raw_syscall_start: 0x3000,
            raw_syscall_end: 0x300e,
            audit_call_returns: [0x1800, 0x1810, 0x1820, 0x1830, 0x1840, 0x1850],
            raw_call_return: 0x3009,
            trusted_gate: 0x4000,
            trusted_gate_end: 0x401a,
            trusted_syscall: 0x4017,
        };
        let load_bias = 0x7f00_0000_0000;
        let args = [
            libc::AT_FDCWD as i64 as u64,
            0x7400,
            0x7600,
            PROC_FD_READLINK_BYTES,
            0,
            0,
        ];
        let permit = AfterLoaderSyscallPermit {
            image: None,
            tid: Pid::from_raw(17),
            generation: 23,
            physical_generation: None,
            origin_status: None,
            admission_status: None,
            purpose: AfterLoaderSyscallPurpose::PrivateSetup,
            number: libc::SYS_readlinkat,
            args,
            executed_args: args,
            instruction_pointer: load_bias + profile.trusted_syscall,
            resume_pointer: load_bias + profile.trusted_syscall + 2,
            instruction: [0x0f, 0x05, 0, 0],
            instruction_length: 2,
            output_spans: vec![(0x7600, 0x7680)],
            effect: AfterLoaderSyscallEffect::None,
        };
        let mut registers = libc::user_regs_struct {
            orig_rax: permit.number as u64,
            rdi: args[0],
            rsi: args[1],
            rdx: args[2],
            r10: args[3],
            r8: args[4],
            r9: args[5],
            rsp: 0x8008,
            ..unsafe { core::mem::zeroed() }
        };
        let controller_stack = GuestRange::new(0x7000, 0x2000).unwrap();
        let return_address = load_bias + 0x1800;
        let call_target = load_bias + profile.raw_syscall_start;
        let call_slot = call_target;
        let indirect_call = |return_address: u64, target: u64| {
            let displacement = i32::try_from(target as i128 - return_address as i128).unwrap();
            let mut call = [0xe8, 0, 0, 0, 0];
            call[1..].copy_from_slice(&displacement.to_le_bytes());
            call
        };
        let call = indirect_call(return_address, call_slot);
        let raw_return_address = load_bias + profile.raw_call_return;
        let raw_call_target = load_bias + profile.trusted_gate;
        let raw_call_slot = raw_call_target;
        let raw_call = indirect_call(raw_return_address, raw_call_slot);
        let spans = [(0x7400, 0x7411), (0x7600, 0x7680)];
        let proc_fd_audit_site_is_exact =
            |profile: ProcFdAuditProfile,
             load_bias: u64,
             permit: &AfterLoaderSyscallPermit,
             registers: &libc::user_regs_struct,
             controller_stack: GuestRange,
             return_address: u64,
             call_instruction: [u8; 5],
             _call_slot: u64,
             call_target: u64,
             raw_return_address: u64,
             raw_call_instruction: [u8; 5],
             _raw_call_slot: u64,
             raw_call_target: u64,
             data_spans: &[(u64, u64)]| {
                call_target == load_bias + profile.raw_syscall_start
                    && raw_call_target == load_bias + profile.trusted_gate
                    && super::proc_fd_audit_site_is_exact(
                        profile,
                        load_bias,
                        permit,
                        registers,
                        controller_stack,
                        controller_stack.end - 8,
                        0x5000,
                        0x5000,
                        return_address,
                        call_instruction,
                        raw_return_address,
                        raw_call_instruction,
                        permit.args[5],
                        permit.args[5],
                        data_spans,
                    )
            };
        assert!(proc_fd_audit_site_is_exact(
            profile,
            load_bias,
            &permit,
            &registers,
            controller_stack,
            return_address,
            call,
            call_slot,
            call_target,
            raw_return_address,
            raw_call,
            raw_call_slot,
            raw_call_target,
            &spans,
        ));

        let mut changed = permit.clone();
        changed.instruction_pointer += 1;
        assert!(!proc_fd_audit_site_is_exact(
            profile,
            load_bias,
            &changed,
            &registers,
            controller_stack,
            return_address,
            call,
            call_slot,
            call_target,
            raw_return_address,
            raw_call,
            raw_call_slot,
            raw_call_target,
            &spans,
        ));
        let mut changed = permit.clone();
        changed.resume_pointer += 1;
        assert!(!proc_fd_audit_site_is_exact(
            profile,
            load_bias,
            &changed,
            &registers,
            controller_stack,
            return_address,
            call,
            call_slot,
            call_target,
            raw_return_address,
            raw_call,
            raw_call_slot,
            raw_call_target,
            &spans,
        ));
        let mut changed = permit.clone();
        changed.instruction[0] ^= 1;
        assert!(!proc_fd_audit_site_is_exact(
            profile,
            load_bias,
            &changed,
            &registers,
            controller_stack,
            return_address,
            call,
            call_slot,
            call_target,
            raw_return_address,
            raw_call,
            raw_call_slot,
            raw_call_target,
            &spans,
        ));
        registers.rdx ^= 1;
        assert!(!proc_fd_audit_site_is_exact(
            profile,
            load_bias,
            &permit,
            &registers,
            controller_stack,
            return_address,
            call,
            call_slot,
            call_target,
            raw_return_address,
            raw_call,
            raw_call_slot,
            raw_call_target,
            &spans,
        ));
        registers.rdx ^= 1;
        for bad_return in (0..6)
            .map(|offset| load_bias + profile.function_start + offset)
            .chain(core::iter::once(load_bias + profile.function_end))
        {
            let boundary_call = indirect_call(bad_return, call_slot);
            assert!(!proc_fd_audit_site_is_exact(
                profile,
                load_bias,
                &permit,
                &registers,
                controller_stack,
                bad_return,
                boundary_call,
                call_slot,
                call_target,
                raw_return_address,
                raw_call,
                raw_call_slot,
                raw_call_target,
                &spans,
            ));
        }
        for index in 0..raw_call.len() {
            let mut changed = raw_call;
            changed[index] ^= 1;
            assert!(!proc_fd_audit_site_is_exact(
                profile,
                load_bias,
                &permit,
                &registers,
                controller_stack,
                return_address,
                call,
                call_slot,
                call_target,
                raw_return_address,
                changed,
                raw_call_slot,
                raw_call_target,
                &spans,
            ));
        }
        for index in 0..call.len() {
            let mut changed = call;
            changed[index] ^= 1;
            assert!(!proc_fd_audit_site_is_exact(
                profile,
                load_bias,
                &permit,
                &registers,
                controller_stack,
                return_address,
                changed,
                call_slot,
                call_target,
                raw_return_address,
                raw_call,
                raw_call_slot,
                raw_call_target,
                &spans,
            ));
        }
        for bad_raw_return in (0..6)
            .map(|offset| load_bias + profile.raw_syscall_start + offset)
            .chain(core::iter::once(load_bias + profile.raw_syscall_end))
        {
            let boundary_call = indirect_call(bad_raw_return, raw_call_slot);
            assert!(!proc_fd_audit_site_is_exact(
                profile,
                load_bias,
                &permit,
                &registers,
                controller_stack,
                return_address,
                call,
                call_slot,
                call_target,
                bad_raw_return,
                boundary_call,
                raw_call_slot,
                raw_call_target,
                &spans,
            ));
        }
        for (bad_call_target, bad_raw_target) in [
            (call_target + 1, raw_call_target),
            (call_target, raw_call_target + 1),
        ] {
            assert!(!proc_fd_audit_site_is_exact(
                profile,
                load_bias,
                &permit,
                &registers,
                controller_stack,
                return_address,
                call,
                call_slot,
                bad_call_target,
                raw_return_address,
                raw_call,
                raw_call_slot,
                bad_raw_target,
                &spans,
            ));
        }
        for bad_spans in [
            vec![(0x6fff, 0x7100)],
            vec![(0x7ff8, 0x8010)],
            vec![(0x7400, 0x7500), (0x7480, 0x7600)],
        ] {
            assert!(!proc_fd_audit_site_is_exact(
                profile,
                load_bias,
                &permit,
                &registers,
                controller_stack,
                return_address,
                call,
                call_slot,
                call_target,
                raw_return_address,
                raw_call,
                raw_call_slot,
                raw_call_target,
                &bad_spans,
            ));
        }
        registers.rsp = 0x8000;
        assert!(!proc_fd_audit_site_is_exact(
            profile,
            load_bias,
            &permit,
            &registers,
            controller_stack,
            return_address,
            call,
            call_slot,
            call_target,
            raw_return_address,
            raw_call,
            raw_call_slot,
            raw_call_target,
            &spans,
        ));
        registers.rsp = controller_stack.end - 8;
        assert!(!proc_fd_audit_site_is_exact(
            profile,
            load_bias,
            &permit,
            &registers,
            controller_stack,
            return_address,
            call,
            call_slot,
            call_target,
            raw_return_address,
            raw_call,
            raw_call_slot,
            raw_call_target,
            &spans,
        ));
    }

    fn append_test_dirent(bytes: &mut Vec<u8>, name: &[u8], kind: u8) {
        let start = bytes.len();
        let record_length = (20 + name.len() + 7) & !7;
        bytes.resize(start + record_length, 0);
        bytes[start..start + 8].copy_from_slice(&(start as u64 + 1).to_ne_bytes());
        bytes[start + 8..start + 16].copy_from_slice(&(record_length as i64).to_ne_bytes());
        bytes[start + 16..start + 18].copy_from_slice(&(record_length as u16).to_ne_bytes());
        bytes[start + 18] = kind;
        bytes[start + 19..start + 19 + name.len()].copy_from_slice(name);
    }

    #[test]
    fn proc_fd_dirents_and_lifecycle_refuse_malformed_or_incomplete_scans() {
        let mut bytes = Vec::new();
        append_test_dirent(&mut bytes, b".", libc::DT_DIR);
        append_test_dirent(&mut bytes, b"..", libc::DT_DIR);
        append_test_dirent(&mut bytes, b"0", libc::DT_LNK);
        append_test_dirent(&mut bytes, b"17", libc::DT_LNK);
        assert_eq!(
            proc_fd_dirent_descriptors(&bytes),
            Ok(BTreeSet::from([0, 17]))
        );

        let mut truncated = bytes.clone();
        truncated.pop();
        assert!(proc_fd_dirent_descriptors(&truncated).is_err());
        let mut bad_length = bytes.clone();
        bad_length[16..18].copy_from_slice(&0_u16.to_ne_bytes());
        assert!(proc_fd_dirent_descriptors(&bad_length).is_err());
        let mut unterminated = vec![0_u8; 24];
        unterminated[16..18].copy_from_slice(&24_u16.to_ne_bytes());
        unterminated[18] = libc::DT_LNK;
        unterminated[19..].fill(b'7');
        assert!(proc_fd_dirent_descriptors(&unterminated).is_err());
        let mut duplicate = Vec::new();
        append_test_dirent(&mut duplicate, b"7", libc::DT_LNK);
        append_test_dirent(&mut duplicate, b"7", libc::DT_LNK);
        assert!(proc_fd_dirent_descriptors(&duplicate).is_err());
        let mut noncanonical = Vec::new();
        append_test_dirent(&mut noncanonical, b"07", libc::DT_LNK);
        assert!(proc_fd_dirent_descriptors(&noncanonical).is_err());
        let mut wrong_kind = Vec::new();
        append_test_dirent(&mut wrong_kind, b"7", libc::DT_REG);
        assert!(proc_fd_dirent_descriptors(&wrong_kind).is_err());

        let expected = BTreeMap::from([(0, b"/dev/null".to_vec()), (17, b"pipe:[41]".to_vec())]);
        let mut directory = AfterLoaderOwnedDescriptor::ProcFdDirectory {
            expected,
            seen: BTreeSet::new(),
            linked: BTreeSet::new(),
            eof: false,
        };
        assert!(!directory.proc_fd_scan_is_complete());
        assert!(directory.record_proc_fd_dirents(BTreeSet::from([0]), false));
        assert!(!directory.record_proc_fd_dirents(BTreeSet::from([17]), false));
        assert!(directory.record_proc_fd_readlink(0));
        assert!(!directory.record_proc_fd_readlink(0));
        assert!(directory.record_proc_fd_dirents(BTreeSet::from([17]), false));
        assert!(!directory.record_proc_fd_dirents(BTreeSet::new(), true));
        assert!(directory.record_proc_fd_readlink(17));
        assert!(directory.record_proc_fd_dirents(BTreeSet::new(), true));
        assert!(directory.proc_fd_scan_is_complete());
        assert!(!directory.record_proc_fd_dirents(BTreeSet::new(), true));
        assert!(!directory.record_proc_fd_readlink(17));

        let mut unknown = AfterLoaderOwnedDescriptor::ProcFdDirectory {
            expected: BTreeMap::from([(0, Vec::new())]),
            seen: BTreeSet::new(),
            linked: BTreeSet::new(),
            eof: false,
        };
        assert!(!unknown.record_proc_fd_dirents(BTreeSet::from([1]), false));
        assert!(!unknown.record_proc_fd_dirents(BTreeSet::new(), true));
    }

    fn test_guest_map(start: u64, end: u64, path: Option<&str>) -> GuestMap {
        GuestMap {
            start,
            end,
            offset: 0,
            device_major: 0,
            device_minor: 0,
            readable: true,
            writable: true,
            executable: false,
            shared: false,
            inode: 0,
            path: path.map(PathBuf::from),
        }
    }

    #[test]
    fn private_brk_growth_is_bounded_one_shot_and_vma_exact() {
        assert_eq!(
            private_brk_growth_range(0x405000, 0x426000),
            Ok(Some((0x405000, 0x426000)))
        );
        assert_eq!(private_brk_growth_range(0x405001, 0x405002), Ok(None));
        assert_eq!(
            private_brk_growth_range(0x405000, 0x405000),
            Err(Errno::EINVAL)
        );
        assert_eq!(
            private_brk_growth_range(0x405000, 0x404fff),
            Err(Errno::EINVAL)
        );
        assert!(private_brk_growth_range(0x405000, 0x405000 + MAX_PRIVATE_BRK_GROWTH).is_ok());
        assert_eq!(
            private_brk_growth_range(0x405000, 0x405000 + MAX_PRIVATE_BRK_GROWTH + 1),
            Err(Errno::EINVAL)
        );

        let key = [0xa5; 8];
        let profile = PtmallocBootstrapState {
            load_bias: 0x7f00_0000_0000,
            entry: PtmallocBootstrapObservation {
                initialized: 0,
                tcache_key: [0; 8],
            },
            pre_dlopen_verified: true,
            entropy_consumed: true,
            injected_key: Some(key),
        };
        let args = [0x426000, 135168, 0x7fff_1234, 4095, 3, 0];
        let mut permit = AfterLoaderSyscallPermit {
            image: None,
            tid: Pid::from_raw(17),
            generation: 23,
            physical_generation: None,
            origin_status: None,
            admission_status: None,
            purpose: AfterLoaderSyscallPurpose::PrivateSetup,
            number: libc::SYS_brk,
            args,
            executed_args: args,
            instruction_pointer: profile.load_bias + PTMALLOC_BRK_SYSCALL_RVA,
            resume_pointer: profile.load_bias + PTMALLOC_BRK_SYSCALL_RVA + 2,
            instruction: [0x0f, 0x05, 0, 0],
            instruction_length: 2,
            output_spans: Vec::new(),
            effect: AfterLoaderSyscallEffect::None,
        };
        let observed = PtmallocBootstrapObservation {
            initialized: 1,
            tcache_key: key,
        };
        assert!(exact_ptmalloc_brk_growth_request(
            profile,
            profile.load_bias,
            observed,
            &permit,
        ));
        permit.instruction_pointer += 1;
        assert!(!exact_ptmalloc_brk_growth_request(
            profile,
            profile.load_bias,
            observed,
            &permit,
        ));
        permit.instruction_pointer -= 1;
        permit.output_spans.push((1, 2));
        assert!(!exact_ptmalloc_brk_growth_request(
            profile,
            profile.load_bias,
            observed,
            &permit,
        ));
        permit.output_spans.clear();
        let mut unconsumed = profile;
        unconsumed.entropy_consumed = false;
        assert!(!exact_ptmalloc_brk_growth_request(
            unconsumed,
            profile.load_bias,
            observed,
            &permit,
        ));
        let mut changed_observation = observed;
        changed_observation.tcache_key[0] ^= 1;
        assert!(!exact_ptmalloc_brk_growth_request(
            profile,
            profile.load_bias,
            changed_observation,
            &permit,
        ));

        let mut current = Some(0x405000);
        let mut consumed = false;
        assert!(!complete_private_brk_growth_state(
            &mut current,
            &mut consumed,
            0x405000,
            0x426000,
            0x405000,
        ));
        assert_eq!(current, Some(0x405000));
        assert!(!consumed);
        assert!(complete_private_brk_growth_state(
            &mut current,
            &mut consumed,
            0x405000,
            0x426000,
            0x426000,
        ));
        assert_eq!(current, Some(0x426000));
        assert!(consumed);
        assert!(!complete_private_brk_growth_state(
            &mut current,
            &mut consumed,
            0x426000,
            0x427000,
            0x427000,
        ));

        let other = test_guest_map(0x10_0000, 0x11_0000, None);
        let created = test_guest_map(0x405000, 0x426000, Some("[heap]"));
        assert!(private_brk_maps_advance_exactly(
            std::slice::from_ref(&other),
            &[created.clone(), other.clone()],
            0x405000,
            0x426000,
        ));
        let prior = test_guest_map(0x400000, 0x405000, Some("[heap]"));
        let extended = GuestMap {
            end: 0x426000,
            ..prior.clone()
        };
        assert!(private_brk_maps_advance_exactly(
            &[prior.clone(), other.clone()],
            &[extended.clone(), other.clone()],
            0x405000,
            0x426000,
        ));
        let mut unrelated_changed = other.clone();
        unrelated_changed.end += PAGE;
        assert!(!private_brk_maps_advance_exactly(
            &[prior.clone(), other.clone()],
            &[extended.clone(), unrelated_changed],
            0x405000,
            0x426000,
        ));
        let mut protected = extended;
        protected.readable = false;
        protected.writable = false;
        assert!(!private_brk_maps_advance_exactly(
            &[prior, other],
            &[protected],
            0x405000,
            0x426000,
        ));
    }

    #[test]
    fn anonymous_mmap_descriptor_accepts_only_canonical_minus_one() {
        assert!(canonical_anonymous_mmap_descriptor(0x0000_0000_ffff_ffff));
        assert!(canonical_anonymous_mmap_descriptor(u64::MAX));
        for raw in [
            0x0000_0001_ffff_ffff,
            0xffff_fffe_ffff_ffff,
            0xdead_beef_ffff_ffff,
            0,
            3,
        ] {
            assert!(!canonical_anonymous_mmap_descriptor(raw));
        }
    }

    #[test]
    fn page_effect_lengths_follow_linux_rounding_without_changing_raw_permits() {
        let start = 0x20_0000;
        for (raw_length, effective_length) in [
            (1, PAGE),
            (PAGE - 1, PAGE),
            (PAGE, PAGE),
            (PAGE + 1, 2 * PAGE),
            (0x7d6_f702, 0x7d70_000),
        ] {
            assert_eq!(checked_page_effect_length(raw_length), Ok(effective_length));
            assert_eq!(
                checked_page_effect_range(start, raw_length),
                Ok((start, start + effective_length))
            );
        }

        assert_eq!(checked_page_effect_length(0), Err(Errno::EINVAL));
        assert_eq!(
            checked_page_effect_range(start + 1, PAGE),
            Err(Errno::EINVAL)
        );
        assert_eq!(checked_page_effect_length(u64::MAX), Err(Errno::EOVERFLOW));
        assert_eq!(
            checked_page_effect_range(u64::MAX - (PAGE - 1), PAGE),
            Err(Errno::EOVERFLOW)
        );

        let args = [
            0,
            0x7d6_f702,
            libc::PROT_READ as u64,
            (libc::MAP_PRIVATE | libc::MAP_DENYWRITE) as u64,
            4,
            0,
        ];
        let permit = AfterLoaderSyscallPermit {
            image: None,
            tid: Pid::from_raw(17),
            generation: 23,
            physical_generation: None,
            origin_status: None,
            admission_status: None,
            purpose: AfterLoaderSyscallPurpose::PrivateSetup,
            number: libc::SYS_mmap,
            args,
            executed_args: args,
            instruction_pointer: 0x401000,
            resume_pointer: 0x401002,
            instruction: [0x0f, 0x05, 0, 0],
            instruction_length: 2,
            output_spans: Vec::new(),
            effect: AfterLoaderSyscallEffect::Map {
                requested: args[0],
                raw_length: args[1],
                protection: libc::PROT_READ,
                flags: libc::MAP_PRIVATE | libc::MAP_DENYWRITE,
                descriptor: Some(args[4]),
                offset: args[5],
                purpose: AfterLoaderMappingPurpose::Controller,
            },
        };
        assert_eq!(permit.args, args, "page rounding changed raw permit args");
        assert!(matches!(
            permit.effect,
            AfterLoaderSyscallEffect::Map {
                raw_length: 0x7d6_f702,
                ..
            }
        ));
    }

    #[test]
    fn private_mmap_effect_cap_and_offset_alignment_are_exact() {
        assert_eq!(
            checked_private_mmap_effect_length(MAX_PRIVATE_MMAP_EFFECT),
            Ok(MAX_PRIVATE_MMAP_EFFECT)
        );
        assert_eq!(
            checked_private_mmap_effect_length(MAX_PRIVATE_MMAP_EFFECT - PAGE + 1),
            Ok(MAX_PRIVATE_MMAP_EFFECT)
        );
        assert_eq!(
            checked_private_mmap_effect_length(MAX_PRIVATE_MMAP_EFFECT + 1),
            Err(Errno::EINVAL)
        );
        assert!(private_mmap_offset_is_admissible(0));
        assert!(private_mmap_offset_is_admissible(PAGE));
        assert!(private_mmap_offset_is_admissible(
            (i64::MAX as u64) & !(PAGE - 1)
        ));
        assert!(!private_mmap_offset_is_admissible(1));
        assert!(!private_mmap_offset_is_admissible(PAGE + 1));
        assert!(!private_mmap_offset_is_admissible(i64::MAX as u64));
        assert!(!private_mmap_offset_is_admissible(1_u64 << 63));

        assert!(exact_trampoline_mmap_length(TRAMPOLINE_ARENA_SIZE));
        assert!(!exact_trampoline_mmap_length(TRAMPOLINE_ARENA_SIZE - 1));
        assert_eq!(
            checked_page_effect_length(TRAMPOLINE_ARENA_SIZE - 1),
            Ok(TRAMPOLINE_ARENA_SIZE),
            "near-miss control must reach the exact raw-size gate"
        );
        assert!(exact_shared_reservation_mmap_length(PAGE));
        assert!(!exact_shared_reservation_mmap_length(PAGE - 1));
        assert_eq!(
            checked_page_effect_length(PAGE - 1),
            Ok(PAGE),
            "near-miss control must reach the exact raw-size gate"
        );
    }

    #[test]
    fn rounded_page_tail_drives_overlap_ownership_protection_and_removal() {
        let start = 0x40_0000;
        let raw_length = PAGE + 1;
        let raw_range = checked_range(start, raw_length).unwrap();
        let effective_range = checked_page_effect_range(start, raw_length).unwrap();
        assert_eq!(effective_range, (start, start + 2 * PAGE));

        let tail = (raw_range.1, effective_range.1);
        let mut overlap = mapping_state(1);
        overlap.original_mappings.push(tail);
        overlap.protected_ranges.push(tail);
        assert!(!overlap.refuses_original_overlap(raw_range));
        assert!(!overlap.refuses_protected_overlap(raw_range));
        assert!(overlap.refuses_original_overlap(effective_range));
        assert!(overlap.refuses_protected_overlap(effective_range));

        let mut protected = mapping_state(2);
        protected
            .owned_mappings
            .push(controller_mapping(start, start + 3 * PAGE));
        assert!(protected.owns_range(effective_range, false));
        protected.protect_owned_range(effective_range, libc::PROT_READ);
        protected
            .owned_mappings
            .sort_by_key(|mapping| mapping.start);
        assert_eq!(protected.owned_mappings.len(), 2);
        assert_eq!(
            (
                protected.owned_mappings[0].start,
                protected.owned_mappings[0].end
            ),
            effective_range
        );
        assert!(protected.owned_mappings[0].readable);
        assert!(!protected.owned_mappings[0].writable);
        assert_eq!(
            (
                protected.owned_mappings[1].start,
                protected.owned_mappings[1].end
            ),
            (start + 2 * PAGE, start + 3 * PAGE)
        );
        assert!(protected.owned_mappings[1].writable);

        let mut removed = mapping_state(3);
        removed
            .owned_mappings
            .push(controller_mapping(start, start + 3 * PAGE));
        removed.remove_owned_range(effective_range);
        assert_eq!(removed.owned_mappings.len(), 1);
        assert_eq!(
            (
                removed.owned_mappings[0].start,
                removed.owned_mappings[0].end
            ),
            (start + 2 * PAGE, start + 3 * PAGE)
        );
        assert_eq!(removed.owned_mappings[0].offset, 2 * PAGE);

        let one_page = {
            let mut state = mapping_state(4);
            state
                .owned_mappings
                .push(controller_mapping(start, start + PAGE));
            state
        };
        assert!(!one_page.owns_range(effective_range, false));
        assert_eq!(
            checked_page_effect_range(start + 1, 1),
            Err(Errno::EINVAL),
            "unaligned mprotect/munmap start was rounded instead of refused"
        );
    }

    #[test]
    fn fixed_file_mmap_can_replace_only_its_exact_image_reservation() {
        let range = (0x60_0000, 0x60_0000 + 2 * PAGE);
        let image = AfterLoaderImageId {
            generation: 5,
            file: crate::after_loader::FileIdentity {
                device: 7,
                inode: 11,
            },
        };
        let other_image = AfterLoaderImageId {
            file: crate::after_loader::FileIdentity {
                device: 13,
                inode: 17,
            },
            ..image
        };
        let trampoline = AfterLoaderTrampolineId {
            generation: image.generation,
            serial: 1,
            file: crate::after_loader::FileIdentity {
                device: 19,
                inode: 23,
            },
        };
        let mut state = mapping_state(image.generation);
        let mut mapping = controller_mapping(range.0, range.1);
        mapping.purpose = AfterLoaderMappingPurpose::Image { image };
        state.owned_mappings.push(mapping);
        assert!(state.owns_same_image_range(range, image));
        assert!(!state.owns_same_image_range(range, other_image));

        for purpose in [
            AfterLoaderMappingPurpose::Controller,
            AfterLoaderMappingPurpose::Trampoline { trampoline },
            AfterLoaderMappingPurpose::SharedReservation { trampoline },
        ] {
            state.owned_mappings[0].purpose = purpose;
            assert!(
                !state.owns_same_image_range(range, image),
                "accepted foreign fixed-mmap owner {purpose:?}"
            );
        }

        state.owned_mappings = vec![
            AfterLoaderOwnedMapping {
                end: range.0 + PAGE,
                purpose: AfterLoaderMappingPurpose::Image { image },
                ..mapping
            },
            AfterLoaderOwnedMapping {
                start: range.0 + PAGE,
                offset: PAGE,
                purpose: AfterLoaderMappingPurpose::ImageZeroFill { image },
                ..mapping
            },
        ];
        assert!(
            state.owns_same_image_range(range, image),
            "refused gap-free adjacent same-image file/BSS reservations"
        );

        let mut gap = state.clone();
        gap.owned_mappings[1].start += 1;
        assert!(!gap.owns_same_image_range(range, image));

        let mut overlap = state.clone();
        overlap.owned_mappings[1].start -= 1;
        assert!(
            !overlap.owns_same_image_range(range, image),
            "accepted overlapping same-image reservations"
        );

        let mut foreign = state;
        foreign.owned_mappings[1].purpose =
            AfterLoaderMappingPurpose::ImageZeroFill { image: other_image };
        assert!(!foreign.owns_same_image_range(range, image));
    }

    #[test]
    fn rounded_munmap_must_remove_a_shared_reservation_exactly() {
        let start = 0x70_0000;
        let range = (start, start + PAGE);
        let trampoline = AfterLoaderTrampolineId {
            generation: 7,
            serial: 1,
            file: crate::after_loader::FileIdentity {
                device: 29,
                inode: 31,
            },
        };
        let mut state = mapping_state(trampoline.generation);
        state.shared_reservations.insert(
            trampoline,
            (
                range,
                SharedReservationIdentity {
                    mapping: MappingIdentity {
                        device_major: 0,
                        device_minor: 1,
                        inode: 37,
                    },
                    path: Some(PathBuf::from("/dev/zero (deleted)")),
                },
            ),
        );
        let mut mapping = controller_mapping(range.0, range.1);
        mapping.shared = true;
        mapping.purpose = AfterLoaderMappingPurpose::SharedReservation { trampoline };
        state.owned_mappings.push(mapping);

        let raw_range = checked_range(start, 1).unwrap();
        let effective_range = checked_page_effect_range(start, 1).unwrap();
        assert!(!state.shared_reservation_unmap_is_exact(raw_range));
        assert!(state.shared_reservation_unmap_is_exact(effective_range));
        assert!(
            !state.shared_reservation_unmap_is_exact(
                checked_page_effect_range(start, PAGE + 1).unwrap()
            ),
            "accepted an effective range extending beyond the reservation"
        );

        state.remove_owned_range(effective_range);
        assert!(state.owned_mappings.is_empty());
        assert!(!state.shared_reservations.contains_key(&trampoline));
    }

    #[test]
    fn mprotect_and_munmap_completion_require_exact_zero() {
        let protect = AfterLoaderSyscallEffect::Protect {
            start: 0x80_0000,
            raw_length: 1,
            protection: libc::PROT_READ,
            loader_cache_alias: None,
        };
        let remove = AfterLoaderSyscallEffect::Remove {
            start: 0x80_0000,
            raw_length: 1,
        };
        for effect in [&protect, &remove] {
            assert!(private_memory_effect_result_is_exact(effect, 0));
            assert!(!private_memory_effect_result_is_exact(effect, 1));
            assert!(!private_memory_effect_result_is_exact(effect, -1));
        }

        let map = AfterLoaderSyscallEffect::Map {
            requested: 0,
            raw_length: 1,
            protection: libc::PROT_READ,
            flags: libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
            descriptor: None,
            offset: 0,
            purpose: AfterLoaderMappingPurpose::Controller,
        };
        assert!(private_memory_effect_result_is_exact(&map, 0x80_0000));
    }

    #[test]
    fn close_completion_requires_exact_zero() {
        assert!(private_close_result_is_exact(0));
        for changed in [-1, 1, i64::MAX] {
            assert!(!private_close_result_is_exact(changed));
        }
    }

    #[test]
    fn read_completion_accepts_only_an_exact_nonempty_prefix_or_eof() {
        let expected = vec![0x5a; 4096];
        assert_eq!(exact_read_prefix_length(4078, &expected), Some(4078));
        assert_eq!(exact_read_prefix_length(4096, &expected), Some(4096));
        assert_eq!(exact_read_prefix_length(0, &expected), None);
        assert_eq!(exact_read_prefix_length(4097, &expected), None);
        assert_eq!(exact_read_prefix_length(-1, &expected), None);

        assert_eq!(exact_read_prefix_length(0, &[]), Some(0));
        assert_eq!(exact_read_prefix_length(1, &[]), None);

        assert_eq!(exact_read_window_end(0, 4096, 0), None);
        assert_eq!(exact_read_window_end(4096, 4096, 0), Some(4096));
        assert_eq!(exact_read_window_end(4096, 4096, 17), Some(4096));
        assert_eq!(exact_read_window_end(4097, 4096, 1), None);
        assert_eq!(exact_read_window_end(7, 4096, 11), Some(18));
        assert_eq!(exact_read_window_end(usize::MAX, usize::MAX, 1), None);
    }

    #[test]
    fn owned_descriptor_statx_shape_is_exact_and_output_is_full_width() {
        assert_eq!(STATX_OUTPUT_BYTES, 256);
        assert_eq!(PRIVATE_STATX_MASK, 0x0fff);
        assert_eq!(PRIVATE_STATX_MASK, libc::STATX_ALL);
        let args = [
            7,
            0x1234,
            libc::AT_EMPTY_PATH as u64,
            PRIVATE_STATX_MASK as u64,
            0x5678,
            0xfeed_face_dead_beef,
        ];
        assert!(exact_owned_descriptor_statx_arguments(&args));

        for (index, value) in [
            (2, 0),
            (2, libc::AT_SYMLINK_NOFOLLOW as u64),
            (2, libc::AT_EMPTY_PATH as u64 | (1_u64 << 32)),
            (3, libc::STATX_BASIC_STATS as u64),
            (3, PRIVATE_STATX_MASK as u64 | (1_u64 << 32)),
        ] {
            let mut changed = args;
            changed[index] = value;
            assert!(!exact_owned_descriptor_statx_arguments(&changed));
        }

        let file = std::fs::File::open("/proc/self/maps").unwrap();
        let direct = fd_statx_bytes(file.as_raw_fd()).unwrap();
        let bytes = descriptor_statx_bytes(
            Pid::from_raw(std::process::id() as i32),
            file.as_raw_fd() as u64,
        )
        .unwrap();
        assert_eq!(bytes, direct);
        let mask = u32::from_ne_bytes(bytes[..4].try_into().unwrap());
        assert_eq!(mask & libc::STATX_BASIC_STATS, libc::STATX_BASIC_STATS);
    }

    #[test]
    fn file_and_mapping_identity_domains_bind_without_device_conflation() {
        let mut state = mapping_state(1);
        // This models the observed Btrfs split: pathname stat reports 0:21,
        // while /proc/<pid>/maps reports 0:20 for the exact mapped image.
        let image = AfterLoaderImageId {
            generation: 1,
            file: crate::after_loader::FileIdentity {
                device: 0x21,
                inode: 21_907_919,
            },
        };
        let mapping = MappingIdentity {
            device_major: 0,
            device_minor: 0x20,
            inode: 21_907_919,
        };
        let geometry = ResolvedImageGeometry {
            image,
            mapping,
            load_bias: 0x7f00_0000,
            span: (0x7f00_0000, 0x7f01_0000),
        };
        assert!(!state.has_causal_image_mapping(geometry));
        assert!(state.bind_image_mapping(image, mapping));
        assert!(state.has_causal_image_mapping(geometry));
        assert!(state.bind_image_geometry(geometry));
        assert_eq!(state.image_mappings.get(&image), Some(&mapping));
        assert_eq!(state.image_geometries.get(&image), Some(&geometry));
        assert!(
            state.bind_image_geometry(geometry),
            "exact rebinding changed"
        );

        for changed in [
            MappingIdentity {
                device_major: 1,
                ..mapping
            },
            MappingIdentity {
                device_minor: 0x21,
                ..mapping
            },
            MappingIdentity {
                inode: mapping.inode + 1,
                ..mapping
            },
        ] {
            assert!(
                !state.bind_image_mapping(image, changed),
                "accepted changed map identity component: {changed:?}"
            );
        }
        let colliding_image = AfterLoaderImageId {
            file: crate::after_loader::FileIdentity {
                device: 0x22,
                inode: image.file.inode,
            },
            ..image
        };
        assert!(!state.bind_image_mapping(colliding_image, mapping));
        assert!(!state.bind_image_mapping(
            AfterLoaderImageId {
                generation: 2,
                ..image
            },
            mapping,
        ));

        assert_eq!(one_exact_geometry_candidate(&[]), None);
        assert_eq!(
            one_exact_geometry_candidate(&[(mapping, 0x1000)]),
            Some((mapping, 0x1000))
        );
        assert_eq!(
            one_exact_geometry_candidate(&[(mapping, 0x1000), (mapping, 0x2000)]),
            None,
            "duplicate exact geometries were not refused"
        );
        assert!(loader_resolution_matches_geometry(
            mapping.as_target_loader(),
            geometry.load_bias,
            geometry,
        ));
        for (identity, bias) in [
            (
                (
                    mapping.device_major + 1,
                    mapping.device_minor,
                    mapping.inode,
                ),
                geometry.load_bias,
            ),
            (
                (
                    mapping.device_major,
                    mapping.device_minor + 1,
                    mapping.inode,
                ),
                geometry.load_bias,
            ),
            (
                (
                    mapping.device_major,
                    mapping.device_minor,
                    mapping.inode + 1,
                ),
                geometry.load_bias,
            ),
            (mapping.as_target_loader(), geometry.load_bias + PAGE),
        ] {
            assert!(!loader_resolution_matches_geometry(
                identity, bias, geometry
            ));
        }
    }

    #[test]
    fn trampoline_mapping_identity_is_captured_once_per_generation() {
        let mut state = mapping_state(4);
        let trampoline = state
            .next_trampoline_id(crate::after_loader::FileIdentity {
                device: 0x21,
                inode: 91,
            })
            .unwrap();
        let mapping = MappingIdentity {
            device_major: 0,
            device_minor: 1,
            inode: 91,
        };
        assert!(state.bind_trampoline_mapping(trampoline, mapping));
        assert!(state.bind_trampoline_mapping(trampoline, mapping));
        assert_eq!(state.trampoline_mapping(trampoline), Some(mapping));
        for changed in [
            MappingIdentity {
                device_major: 1,
                ..mapping
            },
            MappingIdentity {
                device_minor: 2,
                ..mapping
            },
            MappingIdentity {
                inode: 92,
                ..mapping
            },
        ] {
            assert!(!state.bind_trampoline_mapping(trampoline, changed));
        }
        let next = state
            .next_trampoline_id(crate::after_loader::FileIdentity {
                device: 0x21,
                inode: 93,
            })
            .unwrap();
        assert_ne!(trampoline.serial, next.serial);
        assert!(!state.bind_trampoline_mapping(next, mapping));
        assert!(!state.bind_trampoline_mapping(
            AfterLoaderTrampolineId {
                generation: 5,
                ..next
            },
            MappingIdentity {
                inode: 94,
                ..mapping
            },
        ));
    }

    #[test]
    fn abandoned_trampoline_close_requires_every_partial_alias_removed() {
        let mut state = mapping_state(6);
        let trampoline = state
            .next_trampoline_id(crate::after_loader::FileIdentity {
                device: 37,
                inode: 41,
            })
            .unwrap();
        state.owned_descriptors.insert(
            9,
            AfterLoaderOwnedDescriptor::Trampoline {
                id: Some(trampoline),
                size: Some(TRAMPOLINE_ARENA_SIZE),
            },
        );
        assert_eq!(
            state.trampoline_close_shape(9, trampoline, TRAMPOLINE_ARENA_SIZE),
            Some(TrampolineCloseShape::Abandoned),
            "an arena whose first mmap failed owns no mapping"
        );

        let identity = MappingIdentity {
            device_major: 0,
            device_minor: 1,
            inode: 41,
        };
        assert!(state.bind_trampoline_mapping(trampoline, identity));
        let writable = (0x10_0000, 0x10_0000 + TRAMPOLINE_ARENA_SIZE);
        let executable = (0x20_0000, 0x20_0000 + TRAMPOLINE_ARENA_SIZE);
        for (range, writable, executable) in [(writable, true, false), (executable, false, true)] {
            state.owned_mappings.push(AfterLoaderOwnedMapping {
                start: range.0,
                end: range.1,
                readable: true,
                writable,
                executable,
                shared: true,
                descriptor: Some(9),
                offset: 0,
                purpose: AfterLoaderMappingPurpose::Trampoline { trampoline },
            });
        }
        assert_eq!(
            state.trampoline_close_shape(9, trampoline, TRAMPOLINE_ARENA_SIZE),
            None,
            "aliases without their reservation are a partial arena"
        );

        state.remove_owned_range(writable);
        assert_eq!(
            state.trampoline_close_shape(9, trampoline, TRAMPOLINE_ARENA_SIZE),
            None,
            "one surviving alias prevents abandoned cleanup"
        );
        state.remove_owned_range(executable);
        assert_eq!(state.trampoline_mapping(trampoline), Some(identity));
        assert_eq!(
            state.trampoline_close_shape(9, trampoline, TRAMPOLINE_ARENA_SIZE),
            Some(TrampolineCloseShape::Abandoned)
        );
    }

    #[test]
    fn shared_reservation_binds_one_fully_aliased_trampoline() {
        let mut state = mapping_state(7);
        let trampoline = state
            .next_trampoline_id(crate::after_loader::FileIdentity {
                device: 41,
                inode: 43,
            })
            .unwrap();
        let identity = MappingIdentity {
            device_major: 0,
            device_minor: 9,
            inode: 43,
        };
        assert!(state.bind_trampoline_mapping(trampoline, identity));
        state.owned_descriptors.insert(
            11,
            AfterLoaderOwnedDescriptor::Trampoline {
                id: Some(trampoline),
                size: Some(TRAMPOLINE_ARENA_SIZE),
            },
        );
        assert_eq!(state.trampoline_awaiting_shared_reservation(), None);

        for (start, writable, executable) in [(0x10_0000, true, false), (0x20_0000, false, true)] {
            state.owned_mappings.push(AfterLoaderOwnedMapping {
                start,
                end: start + TRAMPOLINE_ARENA_SIZE,
                readable: true,
                writable,
                executable,
                shared: true,
                descriptor: Some(11),
                offset: 0,
                purpose: AfterLoaderMappingPurpose::Trampoline { trampoline },
            });
            if writable {
                assert_eq!(state.trampoline_awaiting_shared_reservation(), None);
            }
        }
        assert_eq!(
            state.trampoline_awaiting_shared_reservation(),
            Some(trampoline)
        );

        let mut ambiguous = state.clone();
        let other = ambiguous
            .next_trampoline_id(crate::after_loader::FileIdentity {
                device: 47,
                inode: 53,
            })
            .unwrap();
        assert!(ambiguous.bind_trampoline_mapping(
            other,
            MappingIdentity {
                inode: 53,
                ..identity
            }
        ));
        ambiguous.owned_descriptors.insert(
            12,
            AfterLoaderOwnedDescriptor::Trampoline {
                id: Some(other),
                size: Some(TRAMPOLINE_ARENA_SIZE),
            },
        );
        for (start, writable, executable) in [(0x40_0000, true, false), (0x50_0000, false, true)] {
            ambiguous.owned_mappings.push(AfterLoaderOwnedMapping {
                start,
                end: start + TRAMPOLINE_ARENA_SIZE,
                readable: true,
                writable,
                executable,
                shared: true,
                descriptor: Some(12),
                offset: 0,
                purpose: AfterLoaderMappingPurpose::Trampoline { trampoline: other },
            });
        }
        assert_eq!(ambiguous.trampoline_awaiting_shared_reservation(), None);

        let reservation = (0x30_0000, 0x30_0000 + PAGE);
        let reservation_binding = (
            reservation,
            SharedReservationIdentity {
                mapping: MappingIdentity {
                    device_major: 0,
                    device_minor: 1,
                    inode: 59,
                },
                path: Some(PathBuf::from("/dev/zero (deleted)")),
            },
        );
        assert_eq!(
            state
                .shared_reservations
                .insert(trampoline, reservation_binding.clone()),
            None
        );
        state.owned_mappings.push(AfterLoaderOwnedMapping {
            start: reservation.0,
            end: reservation.1,
            readable: true,
            writable: true,
            executable: false,
            shared: true,
            descriptor: None,
            offset: 0,
            purpose: AfterLoaderMappingPurpose::SharedReservation { trampoline },
        });
        assert_eq!(state.trampoline_awaiting_shared_reservation(), None);
        assert_eq!(
            state.shared_reservations.get(&trampoline),
            Some(&reservation_binding)
        );
        assert_eq!(
            state.trampoline_close_shape(11, trampoline, TRAMPOLINE_ARENA_SIZE),
            Some(TrampolineCloseShape::Complete)
        );

        let mut partial_alias = state.clone();
        partial_alias.owned_mappings.retain(|mapping| {
            mapping.purpose != AfterLoaderMappingPurpose::Trampoline { trampoline }
                || !mapping.writable
        });
        assert_eq!(
            partial_alias.trampoline_close_shape(11, trampoline, TRAMPOLINE_ARENA_SIZE),
            None
        );

        let mut partial_reservation = state.clone();
        partial_reservation.owned_mappings.retain(|mapping| {
            mapping.purpose != AfterLoaderMappingPurpose::Trampoline { trampoline }
        });
        assert_eq!(
            partial_reservation.trampoline_close_shape(11, trampoline, TRAMPOLINE_ARENA_SIZE),
            None
        );

        let mut abandoned = partial_reservation;
        abandoned.owned_mappings.retain(|mapping| {
            mapping.purpose != AfterLoaderMappingPurpose::SharedReservation { trampoline }
        });
        assert_eq!(
            abandoned.trampoline_close_shape(11, trampoline, TRAMPOLINE_ARENA_SIZE),
            None,
            "a live reservation binding cannot be abandoned"
        );
        assert_eq!(
            abandoned.shared_reservations.remove(&trampoline),
            Some(reservation_binding)
        );
        assert_eq!(
            abandoned.trampoline_close_shape(11, trampoline, TRAMPOLINE_ARENA_SIZE),
            Some(TrampolineCloseShape::Abandoned)
        );
        assert_eq!(
            abandoned.trampoline_mappings.remove(&trampoline),
            Some(identity)
        );
        let next = abandoned
            .next_trampoline_id(crate::after_loader::FileIdentity {
                device: 61,
                inode: 67,
            })
            .unwrap();
        assert!(abandoned.bind_trampoline_mapping(
            next,
            MappingIdentity {
                device_major: 0,
                device_minor: 11,
                inode: 67,
            }
        ));
    }

    #[test]
    fn mapping_identity_match_checks_major_minor_and_inode() {
        let map = GuestMap {
            start: 0x1000,
            end: 0x2000,
            offset: 0,
            device_major: 0,
            device_minor: 0x20,
            readable: true,
            writable: false,
            executable: true,
            shared: false,
            inode: 77,
            path: Some(PathBuf::from("/exact-image")),
        };
        let identity = map.mapping_identity();
        assert!(mapping_identity_matches(identity, &map));
        for changed in [
            MappingIdentity {
                device_major: 1,
                ..identity
            },
            MappingIdentity {
                device_minor: 0x21,
                ..identity
            },
            MappingIdentity {
                inode: 78,
                ..identity
            },
        ] {
            assert!(!mapping_identity_matches(changed, &map));
        }
    }

    #[test]
    fn geometry_byte_comparison_has_no_entry_guard_or_neighbor_exemption() {
        let original = [0xf3, 0x0f, 0x1e, 0xfa, 0x31, 0xed, 0x49, 0x89];
        let mut guarded = original;
        guarded[0] = 0xcc;
        assert!(exact_geometry_bytes_match(&original, &original));
        assert!(!exact_geometry_bytes_match(&guarded, &original));
        for index in 0..original.len() {
            let mut changed = original;
            changed[index] ^= 1;
            assert!(
                !exact_geometry_bytes_match(&changed, &original),
                "changed byte {index} was accepted"
            );
        }
        let mut surrounding = [0_u8; 18];
        surrounding[5..13].copy_from_slice(&original);
        assert!(exact_geometry_bytes_match(&surrounding, &surrounding));
        for index in [4, 13] {
            let mut changed = surrounding;
            changed[index] = 1;
            assert!(
                !exact_geometry_bytes_match(&changed, &surrounding),
                "neighbor byte {index} was ignored"
            );
        }
    }

    #[test]
    fn shared_reservation_requires_exact_linux_dev_zero_identity() {
        let make = |device_major, device_minor, inode, path: Option<&str>| GuestMap {
            start: 0x1000,
            end: 0x2000,
            offset: 0,
            device_major,
            device_minor,
            readable: true,
            writable: true,
            executable: false,
            shared: true,
            inode,
            path: path.map(PathBuf::from),
        };
        let captured = make(0, 1, 78, Some("/dev/zero (deleted)"));
        let identity = exact_shared_reservation_identity(&captured).unwrap();
        assert_eq!(identity.mapping, captured.mapping_identity());
        assert_eq!(identity.path.as_ref(), captured.path.as_ref());

        for changed in [
            make(1, 1, 78, Some("/dev/zero (deleted)")),
            make(0, 2, 78, Some("/dev/zero (deleted)")),
            make(0, 1, 0, Some("/dev/zero (deleted)")),
            make(0, 1, 78, None),
            make(0, 1, 78, Some("[anon_shmem:kernel-name]")),
        ] {
            assert!(
                exact_shared_reservation_identity(&changed).is_none(),
                "accepted noncanonical shared reservation identity {changed:?}"
            );
        }

        let changed_inode =
            exact_shared_reservation_identity(&make(0, 1, 79, Some("/dev/zero (deleted)")))
                .unwrap();
        assert_ne!(identity, changed_inode);
    }

    #[test]
    fn dlopen_graph_excludes_the_unsealed_staging_runtime() {
        let source = ["/usr/bin/true", "/bin/true"]
            .into_iter()
            .map(Path::new)
            .find(|path| path.is_file())
            .unwrap();
        let mut provider = LiteinstCallerImage::read(source).unwrap();
        provider.path = PathBuf::from("/bound/provider.so");
        let mut dependency = provider.clone();
        dependency.path = PathBuf::from("/bound/dependency.so");
        let mut unsealed_runtime = provider.clone();
        unsealed_runtime.path = PathBuf::from("/staging/unsealed-runtime.so");
        let dependencies = [dependency];

        assert_eq!(
            dlopen_graph_image_for_path(
                &provider,
                &dependencies,
                provider.path.as_os_str().as_encoded_bytes(),
            )
            .map(|image| image.path.as_path()),
            Some(provider.path.as_path())
        );
        assert_eq!(
            dlopen_graph_image_for_path(
                &provider,
                &dependencies,
                dependencies[0].path.as_os_str().as_encoded_bytes(),
            )
            .map(|image| image.path.as_path()),
            Some(dependencies[0].path.as_path())
        );
        assert!(
            dlopen_graph_image_for_path(
                &provider,
                &dependencies,
                unsealed_runtime.path.as_os_str().as_encoded_bytes(),
            )
            .is_none(),
            "unsealed staging runtime entered the private dlopen graph"
        );
    }

    #[test]
    fn exact_open_file_mapping_identity_is_observed_stably_from_proc_maps() {
        let path = std::env::current_exe().unwrap();
        let file = std::fs::File::open(path).unwrap();
        let before = stable_backing_stamp(&file.metadata().unwrap());
        let first = mapping_identity_for_open_file(&file).unwrap();
        let second = mapping_identity_for_open_file(&file).unwrap();
        assert_eq!(first, second);
        assert_ne!(first.inode, 0);
        assert_eq!(before, stable_backing_stamp(&file.metadata().unwrap()));
    }

    #[test]
    fn loader_cache_alias_openat2_keeps_absolute_and_relative_links_in_root() {
        static SEQUENCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let directory = std::env::temp_dir().join(format!(
            "liteinst-alias-root-{}-{}",
            std::process::id(),
            SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::SeqCst),
        ));
        std::fs::create_dir(&directory).unwrap();
        let target = directory.join("target");
        std::fs::write(&target, b"exact-target").unwrap();
        std::os::unix::fs::symlink("/target", directory.join("absolute")).unwrap();
        std::os::unix::fs::symlink("target", directory.join("relative")).unwrap();
        std::fs::create_dir_all(directory.join("usr/lib")).unwrap();
        std::fs::write(directory.join("usr/lib/target"), b"nested-target").unwrap();
        std::os::unix::fs::symlink("/usr/lib", directory.join("lib64")).unwrap();
        std::os::unix::fs::symlink("target", directory.join("usr/lib/alias")).unwrap();
        let root = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_PATH | libc::O_CLOEXEC)
            .open(&directory)
            .unwrap();
        let expected =
            crate::after_loader::FileIdentity::from_metadata(&std::fs::metadata(&target).unwrap());
        for path in [Path::new("/absolute"), Path::new("/relative")] {
            let opened = open_path_with_root(&root, path, libc::O_PATH | libc::O_CLOEXEC).unwrap();
            assert_eq!(
                crate::after_loader::FileIdentity::from_metadata(&opened.metadata().unwrap()),
                expected,
                "openat2 path escaped or resolved a different target for {}",
                path.display(),
            );
        }
        let nested = open_path_with_root(
            &root,
            Path::new("/lib64/alias"),
            libc::O_PATH | libc::O_CLOEXEC,
        )
        .unwrap();
        assert_eq!(
            crate::after_loader::FileIdentity::from_metadata(&nested.metadata().unwrap()),
            crate::after_loader::FileIdentity::from_metadata(
                &std::fs::metadata(directory.join("usr/lib/target")).unwrap(),
            ),
            "absolute intermediate symlink escaped the supplied target root",
        );
        let nested_link = open_path_with_root(
            &root,
            Path::new("/lib64/alias"),
            libc::O_PATH | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
        .unwrap();
        assert!(nested_link.metadata().unwrap().file_type().is_symlink());
        assert_eq!(
            read_link_from_path_descriptor(&nested_link).unwrap(),
            Path::new("target"),
        );
        std::fs::remove_file(directory.join("usr/lib/alias")).unwrap();
        std::os::unix::fs::symlink("/target", directory.join("usr/lib/alias")).unwrap();
        assert_eq!(
            read_link_from_path_descriptor(&nested_link).unwrap(),
            Path::new("target"),
            "pinned no-follow descriptor followed a replacement symlink",
        );
        let replacement_link = open_path_with_root(
            &root,
            Path::new("/lib64/alias"),
            libc::O_PATH | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
        .unwrap();
        assert_eq!(
            read_link_from_path_descriptor(&replacement_link).unwrap(),
            Path::new("/target"),
        );
        assert!(
            open_path_with_root(&root, Path::new("relative"), libc::O_PATH | libc::O_CLOEXEC,)
                .is_err(),
            "accepted a non-absolute target-root path",
        );
        let retained_root = directory.with_extension("pinned-root");
        std::fs::rename(&directory, &retained_root).unwrap();
        std::fs::create_dir(&directory).unwrap();
        std::fs::write(directory.join("target"), b"replacement-target").unwrap();
        let mut pinned_target = open_path_with_root(
            &root,
            Path::new("/target"),
            libc::O_RDONLY | libc::O_CLOEXEC,
        )
        .unwrap();
        let mut pinned_bytes = Vec::new();
        pinned_target.read_to_end(&mut pinned_bytes).unwrap();
        assert_eq!(pinned_bytes, b"exact-target");
        let replacement_root = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_PATH | libc::O_CLOEXEC)
            .open(&directory)
            .unwrap();
        let mut replacement_target = open_path_with_root(
            &replacement_root,
            Path::new("/target"),
            libc::O_RDONLY | libc::O_CLOEXEC,
        )
        .unwrap();
        let mut replacement_bytes = Vec::new();
        replacement_target
            .read_to_end(&mut replacement_bytes)
            .unwrap();
        assert_eq!(replacement_bytes, b"replacement-target");
        std::fs::remove_dir_all(directory).unwrap();
        std::fs::remove_dir_all(retained_root).unwrap();
    }

    fn map_test_image(
        file: &std::fs::File,
        image: &LiteinstCallerImage,
    ) -> (*mut libc::c_void, ResolvedImageGeometry) {
        let address = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                PAGE as usize,
                libc::PROT_READ,
                libc::MAP_PRIVATE,
                file.as_raw_fd(),
                0,
            )
        };
        assert_ne!(address, libc::MAP_FAILED);
        let start = address as u64;
        let end = start + PAGE;
        let maps = guest_maps(Pid::from_raw(std::process::id() as i32)).unwrap();
        let mapping = maps
            .iter()
            .find(|mapping| {
                mapping.start <= start
                    && end <= mapping.end
                    && mapping.offset.checked_add(start - mapping.start) == Some(0)
                    && mapping.inode != 0
            })
            .unwrap();
        (
            address,
            ResolvedImageGeometry {
                image: AfterLoaderImageId {
                    generation: 1,
                    file: image.file_identity,
                },
                mapping: mapping.mapping_identity(),
                load_bias: start,
                span: (start, end),
            },
        )
    }

    #[test]
    fn exact_image_backing_guard_accepts_only_the_bound_file_and_full_bytes() {
        static SEQUENCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let source = ["/usr/bin/true", "/bin/true"]
            .into_iter()
            .map(Path::new)
            .find(|path| path.is_file())
            .unwrap();
        let directory = std::env::temp_dir().join(format!(
            "liteinst-mapping-identity-{}-{}",
            std::process::id(),
            SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::SeqCst),
        ));
        std::fs::create_dir(&directory).unwrap();
        let first_path = directory.join("first-image");
        let second_path = directory.join("second-image");
        std::fs::copy(source, &first_path).unwrap();
        std::fs::copy(source, &second_path).unwrap();

        let first_image = LiteinstCallerImage::read(&first_path).unwrap();
        let first_file = std::fs::File::open(&first_path).unwrap();
        let (first_mapping, first_geometry) = map_test_image(&first_file, &first_image);
        let pid = Pid::from_raw(std::process::id() as i32);
        let maps = guest_maps(pid).unwrap();
        assert_eq!(
            target_bound_image_mapping_identity(pid, &first_image).unwrap(),
            Some(first_geometry.mapping),
            "target-root open did not bridge to the exact maps-domain identity"
        );
        assert!(
            exact_image_backing_paths_match(pid, &first_image, first_geometry, &maps,).unwrap()
        );
        assert!(exact_image_mapping_paths_are_literal(
            &first_image,
            first_geometry,
            &maps,
        ));
        let mut alternate_spelling = first_image.clone();
        alternate_spelling.path = second_path.clone();
        assert!(
            !exact_image_mapping_paths_are_literal(&alternate_spelling, first_geometry, &maps,),
            "accepted an alternate pathname spelling for the exact same mapping",
        );

        let mut wrong_mapping = first_geometry;
        wrong_mapping.mapping.device_minor ^= 1;
        assert!(
            !exact_image_backing_paths_match(pid, &first_image, wrong_mapping, &maps,).unwrap()
        );

        let original_last = *first_image.bytes.last().unwrap();
        let writer = std::fs::OpenOptions::new()
            .write(true)
            .open(&first_path)
            .unwrap();
        writer
            .write_all_at(&[original_last ^ 1], first_image.bytes.len() as u64 - 1)
            .unwrap();
        assert!(
            !exact_image_backing_paths_match(pid, &first_image, first_geometry, &maps,).unwrap()
        );
        assert_eq!(unsafe { libc::munmap(first_mapping, PAGE as usize) }, 0);

        let second_image = LiteinstCallerImage::read(&second_path).unwrap();
        let second_file = std::fs::File::open(&second_path).unwrap();
        let (second_mapping, second_geometry) = map_test_image(&second_file, &second_image);
        let second_maps = guest_maps(pid).unwrap();
        assert!(
            exact_image_backing_paths_match(pid, &second_image, second_geometry, &second_maps,)
                .unwrap()
        );
        assert!(
            !exact_image_backing_paths_match(pid, &first_image, second_geometry, &second_maps,)
                .unwrap(),
            "different inode with identical bytes was accepted"
        );

        std::fs::remove_file(&second_path).unwrap();
        let deleted_maps = guest_maps(pid).unwrap();
        assert!(
            !matches!(
                exact_image_backing_paths_match(pid, &second_image, second_geometry, &deleted_maps,),
                Ok(true)
            ),
            "deleted target backing path was accepted"
        );
        assert_eq!(unsafe { libc::munmap(second_mapping, PAGE as usize) }, 0);

        std::fs::remove_file(&first_path).unwrap();
        std::fs::remove_dir(&directory).unwrap();
    }

    #[test]
    fn private_wait_diagnostics_are_structured_and_do_not_expand_event_owners() {
        assert_eq!(after_loader_event_summary(&Event::Seccomp), "Seccomp");
        assert_eq!(after_loader_event_summary(&Event::Syscall), "Syscall");
        assert_eq!(
            after_loader_event_summary(&Event::Signal(Signal::SIGTRAP)),
            "Signal(SIGTRAP)"
        );
        assert_eq!(
            after_loader_wait_summary(&Wait::Exited(Pid::from_raw(17), ExitStatus::Exited(3),)),
            "Exited(tid=17 status=Exited(3))"
        );
        let observer = safeptrace::PhysicalEventObserver::new(
            safeptrace::PhysicalEventObserverConfig::default(),
        )
        .expect("create default-capacity physical observer");
        let child = Running::new(Pid::from_raw(19));
        child
            .attach_physical_event_observer(&observer)
            .expect("attach observer to synthetic child generation");
        let child_summary = after_loader_event_summary(&Event::NewChild(ChildOp::Fork, child));
        assert!(child_summary.starts_with("NewChild(operation=Fork tid=19 generation="));
        assert!(child_summary.len() < 128, "{child_summary}");
        assert!(!child_summary.contains("PhysicalEventObserver"));
        let stopped = Stopped::new_unchecked(Pid::from_raw(29));
        stopped
            .attach_physical_event_observer(&observer)
            .expect("attach observer to synthetic stopped generation");
        let stopped_summary = after_loader_wait_summary(&Wait::Stopped(stopped, Event::Seccomp));
        assert!(stopped_summary.starts_with("Stopped(tid=29 generation="));
        assert!(stopped_summary.contains(" physical_status=None event=Seccomp)"));
        assert!(stopped_summary.len() < 192, "{stopped_summary}");
        for recursive in [
            "EventHandle",
            "TraceeToken",
            "PhysicalEventObserver",
            "RecordBuffer",
        ] {
            assert!(!stopped_summary.contains(recursive), "{stopped_summary}");
        }
        assert_eq!(
            after_loader_event_summary(&Event::Exec(Pid::from_raw(23))),
            "Exec(previous_tid=23)"
        );
    }

    #[test]
    fn zero_fill_verification_reads_prot_none_without_changing_permissions() {
        let length = 2 * PAGE as usize;
        let mapping = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                length,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
                -1,
                0,
            )
        };
        assert_ne!(mapping, libc::MAP_FAILED);
        unsafe {
            *((mapping as *mut u8).add(length - 1)) = 1;
        }
        let start = mapping as u64;

        let child = match unsafe { nix::unistd::fork() }.expect("fork PROT_NONE target") {
            nix::unistd::ForkResult::Child => {
                assert_eq!(
                    unsafe { libc::mprotect(mapping, length, libc::PROT_NONE) },
                    0
                );
                safeptrace::traceme_and_stop().expect("stop PROT_NONE target under ptrace");
                unsafe { libc::_exit(0) };
            }
            nix::unistd::ForkResult::Parent { child } => child,
        };
        // Make the controller's private mapping the exact opposite of the
        // stopped child's after the fork. Reading /proc/self/mem by mistake
        // must therefore fail both content assertions below.
        unsafe {
            *(mapping as *mut u8) = 1;
            *((mapping as *mut u8).add(length - 1)) = 0;
        }
        assert_eq!(
            unsafe { libc::mprotect(mapping, length, libc::PROT_NONE) },
            0
        );
        let (stopped, _) = Running::new(child.into())
            .wait()
            .expect("wait for PROT_NONE target")
            .assume_stopped();
        let assert_child_prot_none = || {
            let maps = guest_maps(child.into()).expect("read stopped child mappings");
            let map = maps
                .iter()
                .find(|map| map.start <= start && start + length as u64 <= map.end)
                .expect("find complete stopped child test mapping");
            assert!(
                !map.readable && !map.writable && !map.executable && !map.shared,
                "stopped child test mapping permissions changed: {map:?}"
            );
        };
        assert_child_prot_none();

        let controller_memory = std::fs::File::open("/proc/self/mem")
            .expect("open controller memory for negative control");
        let mut controller_first = [0];
        let mut controller_tail = [0];
        controller_memory
            .read_exact_at(&mut controller_first, start)
            .expect("read controller first test page");
        controller_memory
            .read_exact_at(&mut controller_tail, start + length as u64 - 1)
            .expect("read controller rounded-tail byte");
        assert_eq!((controller_first, controller_tail), ([1], [0]));

        assert!(
            target_range_is_zero(&stopped, start, PAGE)
                .expect("read zero PROT_NONE page through stopped task")
        );
        assert_child_prot_none();
        assert!(
            target_range_is_zero(&stopped, start, PAGE + 1)
                .expect("raw unrounded prefix misses the nonzero rounded tail")
        );
        let (_, rounded_end) = checked_page_effect_range(start, PAGE + 1).unwrap();
        assert!(
            !target_range_is_zero(&stopped, start, rounded_end - start)
                .expect("read the full rounded PROT_NONE effect through stopped task")
        );
        assert_child_prot_none();
        let exited = stopped
            .resume(None)
            .expect("resume PROT_NONE target")
            .wait()
            .expect("wait for PROT_NONE target exit");
        assert_eq!(exited.assume_exited().1, ExitStatus::Exited(0));
        assert_eq!(unsafe { libc::munmap(mapping, length) }, 0);
    }

    #[test]
    fn helper_isolation_mixed_read_spans_exact_prot_none_page() {
        let page_size = PAGE as usize;
        let length = 3 * page_size;
        let mapping = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                length,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
                -1,
                0,
            )
        };
        assert_ne!(mapping, libc::MAP_FAILED);
        let mapping = mapping.cast::<u8>();
        let expected = (0..length)
            .map(|index| {
                (index as u8)
                    .wrapping_mul(37)
                    .wrapping_add((index / page_size) as u8 * 71)
            })
            .collect::<Vec<_>>();
        unsafe {
            core::ptr::copy_nonoverlapping(expected.as_ptr(), mapping, length);
        }
        let start = mapping as u64;
        let helper_start = start + PAGE;
        let helper_end = helper_start + PAGE;

        let child = match unsafe { nix::unistd::fork() }.expect("fork mixed helper target") {
            nix::unistd::ForkResult::Child => {
                if unsafe {
                    libc::mprotect(
                        helper_start as *mut libc::c_void,
                        page_size,
                        libc::PROT_NONE,
                    )
                } != 0
                {
                    unsafe { libc::_exit(2) };
                }
                safeptrace::traceme_and_stop().expect("stop mixed helper target under ptrace");
                unsafe { libc::_exit(0) };
            }
            nix::unistd::ForkResult::Parent { child } => child,
        };
        let (stopped, _) = Running::new(child.into())
            .wait()
            .expect("wait for mixed helper target")
            .assume_stopped();

        // Make the controller mapping bytewise different after fork. An
        // accidental local read can no longer satisfy the complete comparison.
        unsafe {
            core::ptr::write_bytes(mapping, 0xa5, length);
        }
        let child_maps = guest_maps(child.into()).expect("read mixed helper target mappings");
        let helper_mapping = child_maps
            .iter()
            .find(|mapping| mapping.start == helper_start && mapping.end == helper_end)
            .cloned()
            .expect("find exact isolated helper page");
        let isolation = AfterLoaderHelperIsolation {
            image: AfterLoaderImageId {
                generation: 1,
                file: crate::after_loader::FileIdentity {
                    device: 1,
                    inode: 1,
                },
            },
            range: GuestRange {
                start: helper_start,
                end: helper_end,
            },
            original_mapping: GuestMap {
                start,
                end: start + length as u64,
                readable: true,
                writable: false,
                executable: true,
                shared: false,
                ..helper_mapping.clone()
            },
            mapping: helper_mapping.mapping_identity(),
            load_bias: start,
            file_offset: helper_mapping.offset,
            expected_bytes: expected[page_size..2 * page_size].to_vec(),
        };

        let live_page_is_exact = isolation.validates_live_page(&stopped);
        let mut observed = vec![0_u8; length];
        let mixed_result =
            read_exact_with_helper_isolation(&stopped, start, &mut observed, Some(&isolation));
        let mut direct = vec![0_u8; length];
        let direct_result = read_exact_with_helper_isolation(&stopped, start, &mut direct, None);

        let mut wrong_first = isolation.clone();
        wrong_first.expected_bytes[0] ^= 1;
        let mut wrong_first_output = vec![0_u8; length];
        let wrong_first_result = read_exact_with_helper_isolation(
            &stopped,
            start,
            &mut wrong_first_output,
            Some(&wrong_first),
        );

        let mut wrong_last = isolation.clone();
        *wrong_last.expected_bytes.last_mut().unwrap() ^= 1;
        let mut wrong_last_output = vec![0_u8; length];
        let wrong_last_result = read_exact_with_helper_isolation(
            &stopped,
            start,
            &mut wrong_last_output,
            Some(&wrong_last),
        );
        let helper_mapping_after = guest_maps(child.into())
            .and_then(|maps| {
                maps.into_iter()
                    .find(|mapping| mapping.start == helper_start && mapping.end == helper_end)
            })
            .expect("isolated helper page remained mapped");

        let exited = stopped
            .resume(None)
            .expect("resume mixed helper target")
            .wait()
            .expect("wait for mixed helper target exit");
        assert_eq!(unsafe { libc::munmap(mapping.cast(), length) }, 0);

        assert_eq!(exited.assume_exited().1, ExitStatus::Exited(0));
        assert!(
            live_page_is_exact,
            "isolated helper page failed exact validation"
        );
        assert_eq!(mixed_result, Ok(()));
        assert_eq!(
            observed, expected,
            "mixed read skipped, duplicated, or changed bytes at a page boundary"
        );
        assert!(
            direct_result.is_err(),
            "ordinary user-access read unexpectedly crossed PROT_NONE"
        );
        assert_eq!(
            wrong_first_result,
            Err(safeptrace::Error::Errno(Errno::EPROTO)),
            "first expected helper-byte mismatch was accepted"
        );
        assert_eq!(
            wrong_last_result,
            Err(safeptrace::Error::Errno(Errno::EPROTO)),
            "last expected helper-byte mismatch was accepted"
        );
        assert!(
            !helper_mapping_after.readable
                && !helper_mapping_after.writable
                && !helper_mapping_after.executable
                && !helper_mapping_after.shared,
            "mixed reads changed helper-page protection: {helper_mapping_after:?}"
        );
    }

    #[test]
    fn unsupported_cet_status_completion_reaches_the_final_caller_gate() {
        let before = [0x5a; 8];
        let effect = AfterLoaderSyscallEffect::CetStatus {
            destination: 0x4000,
            before,
        };
        let completion = complete_cet_status_query(&effect, -(libc::EINVAL as i64), before)
            .expect("CET effect has a completion")
            .expect("unchanged EINVAL is the supported CET absence result");
        assert_eq!(
            completion,
            AfterLoaderSyscallCompletion::UnsupportedCetStatus
        );
        assert!(caller_private_syscall_result_is_accepted(completion));
        for index in 0..before.len() {
            let mut changed = before;
            changed[index] ^= 1;
            assert_eq!(
                complete_cet_status_query(&effect, -(libc::EINVAL as i64), changed),
                Some(Err(CetStatusCompletionError::OutputChanged)),
                "changed byte {index} was accepted"
            );
        }
        assert_eq!(
            complete_cet_status_query(&effect, -(libc::EPERM as i64), before),
            Some(Err(CetStatusCompletionError::SyscallFailed(
                -(libc::EPERM as i64)
            )))
        );
        assert_eq!(
            complete_cet_status_query(
                &AfterLoaderSyscallEffect::None,
                -(libc::EINVAL as i64),
                before
            ),
            None
        );
        assert_eq!(
            complete_cet_status_query(&effect, 1, [0; 8]),
            Some(Err(CetStatusCompletionError::NonzeroSuccess(1)))
        );
        let successful = complete_cet_status_query(&effect, 0, [0; 8])
            .expect("CET effect has a completion")
            .expect("zero is the exact successful CET result");
        assert_eq!(successful, AfterLoaderSyscallCompletion::KernelResult(0));
        assert!(caller_private_syscall_result_is_accepted(successful));
        assert!(!caller_private_syscall_result_is_accepted(
            AfterLoaderSyscallCompletion::KernelResult(-(libc::EINVAL as i64)),
        ));
        assert!(!caller_private_syscall_result_is_accepted(
            AfterLoaderSyscallCompletion::KernelResult(-4095),
        ));
        assert!(caller_private_syscall_result_is_accepted(
            AfterLoaderSyscallCompletion::KernelResult(-4096),
        ));
    }

    #[test]
    fn private_entropy_signal_timer_exec_and_clone_requests_are_refused() {
        for syscall in [
            libc::SYS_getrandom,
            libc::SYS_rt_sigaction,
            libc::SYS_rt_sigprocmask,
            libc::SYS_clone,
            libc::SYS_clone3,
            libc::SYS_execve,
            libc::SYS_execveat,
            libc::SYS_timer_create,
            libc::SYS_clock_gettime,
            libc::SYS_madvise,
            libc::SYS_ptrace,
        ] {
            assert!(!private_syscall_allowed(syscall), "syscall {syscall}");
        }
        assert!(private_syscall_allowed(libc::SYS_mmap));
        assert!(private_syscall_allowed(libc::SYS_close));
        assert!(private_syscall_allowed(libc::SYS_getdents64));
        assert!(private_syscall_allowed(libc::SYS_readlinkat));
    }

    #[test]
    fn getrandom_output_span_uses_buffer_and_length_without_widening_admission() {
        let args = [0x40_0000, 8, libc::GRND_NONBLOCK as u64, 0x44, 0x55, 0x66];
        assert_eq!(
            syscall_output_spans(libc::SYS_getrandom, args),
            Ok(vec![(0x40_0000, 0x40_0008)])
        );
        assert_eq!(
            syscall_output_spans(libc::SYS_read, args),
            Ok(vec![(8, 8 + libc::GRND_NONBLOCK as u64)])
        );
        let mut null = args;
        null[0] = 0;
        assert_eq!(
            syscall_output_spans(libc::SYS_getrandom, null),
            Err(Errno::EFAULT)
        );
        null[1] = 0;
        assert_eq!(
            syscall_output_spans(libc::SYS_getrandom, null),
            Ok(Vec::new())
        );
        let mut overflow = args;
        overflow[0] = u64::MAX - 3;
        assert_eq!(
            syscall_output_spans(libc::SYS_getrandom, overflow),
            Err(Errno::EOVERFLOW)
        );
        assert!(!private_syscall_allowed(libc::SYS_getrandom));
    }

    fn exact_ptmalloc_test_permit(load_bias: u64) -> AfterLoaderSyscallPermit {
        let destination = load_bias + PTMALLOC_BOOTSTRAP_TCACHE_KEY_RVA;
        let syscall = load_bias + PTMALLOC_BOOTSTRAP_SYSCALL_RVA;
        let args = [
            destination,
            8,
            libc::GRND_NONBLOCK as u64,
            0x4444,
            0x5555,
            0x6666,
        ];
        AfterLoaderSyscallPermit {
            image: None,
            tid: Pid::from_raw(17),
            generation: 23,
            physical_generation: None,
            origin_status: None,
            admission_status: None,
            purpose: AfterLoaderSyscallPurpose::PrivateSetup,
            number: libc::SYS_getrandom,
            args,
            executed_args: args,
            instruction_pointer: syscall,
            resume_pointer: syscall + 2,
            instruction: [0x0f, 0x05, 0, 0],
            instruction_length: 2,
            output_spans: vec![(destination, destination + 8)],
            effect: AfterLoaderSyscallEffect::None,
        }
    }

    #[test]
    fn loader_cache_open_and_mmap_require_one_exact_raw_shape() {
        let logical_path = 0x7f00_0002_d266;
        let flags = (libc::O_RDONLY | libc::O_CLOEXEC) as u64;
        let open = [
            u64::from(libc::AT_FDCWD as u32),
            logical_path,
            flags,
            0,
            flags,
            logical_path,
        ];
        assert!(exact_loader_cache_openat_arguments(&open, logical_path));
        for index in 0..open.len() {
            let mut changed = open;
            changed[index] ^= 1;
            assert!(
                !exact_loader_cache_openat_arguments(&changed, logical_path),
                "loader-cache open mutation {index} was admitted",
            );
        }
        assert!(!exact_loader_cache_openat_arguments(
            &open,
            logical_path + 1
        ));

        let descriptor = 41;
        let length = 0x7d6_f702;
        let mmap = [
            0,
            length,
            libc::PROT_READ as u64,
            libc::MAP_PRIVATE as u64,
            descriptor,
            0,
        ];
        assert!(loader_cache_mmap_arguments_match(mmap, descriptor, length));
        for index in 0..mmap.len() {
            let mut changed = mmap;
            changed[index] ^= 1;
            assert!(
                !loader_cache_mmap_arguments_match(changed, descriptor, length),
                "loader-cache mmap mutation {index} was admitted",
            );
        }
    }

    #[test]
    fn loader_cache_lifecycle_rejects_duplicates_and_out_of_order_steps() {
        let descriptor = 41;
        let mapping = LoaderCacheMapping {
            start: 0x7000_0000,
            raw_length: 0x12345,
            end: 0x7001_3000,
            identity: MappingIdentity {
                device_major: 0,
                device_minor: 1,
                inode: 43,
            },
        };
        let mut lifecycle = LoaderCacheLifecycle::AwaitingOpen;
        assert!(!lifecycle.complete_stat(descriptor));
        assert!(
            !lifecycle.complete_map(descriptor, mapping, mapping.raw_length, mapping.identity,)
        );
        assert!(!lifecycle.complete_close(descriptor, mapping));
        assert!(!lifecycle.complete_retire(descriptor, mapping));
        assert!(lifecycle.complete_open(descriptor));
        assert!(!lifecycle.complete_open(descriptor));
        assert!(!lifecycle.complete_stat(descriptor + 1));
        assert!(lifecycle.complete_stat(descriptor));
        assert!(!lifecycle.complete_map(
            descriptor + 1,
            mapping,
            mapping.raw_length,
            mapping.identity,
        ));
        assert!(!lifecycle.complete_map(
            descriptor,
            LoaderCacheMapping {
                raw_length: mapping.raw_length + 1,
                ..mapping
            },
            mapping.raw_length,
            mapping.identity,
        ));
        assert!(!lifecycle.complete_map(
            descriptor,
            LoaderCacheMapping {
                identity: MappingIdentity {
                    inode: mapping.identity.inode + 1,
                    ..mapping.identity
                },
                ..mapping
            },
            mapping.raw_length,
            mapping.identity,
        ));
        assert!(lifecycle.complete_map(descriptor, mapping, mapping.raw_length, mapping.identity,));
        assert!(!lifecycle.complete_close(descriptor + 1, mapping));
        assert!(lifecycle.complete_close(descriptor, mapping));
        assert!(!lifecycle.complete_retire(
            descriptor,
            LoaderCacheMapping {
                start: mapping.start + PAGE,
                ..mapping
            },
        ));
        assert!(lifecycle.complete_retire(descriptor, mapping));
        assert!(!lifecycle.complete_retire(descriptor, mapping));
        assert!(!lifecycle.complete_scratch_release(descriptor, mapping));
        assert!(lifecycle.arm_scratch_release(descriptor, mapping));
        assert!(!lifecycle.arm_scratch_release(descriptor, mapping));
        assert!(lifecycle.complete_scratch_release(descriptor, mapping));
        assert!(!lifecycle.complete_scratch_release(descriptor, mapping));
    }

    fn profiled_alias_mmap_plan_for_test() -> LoaderCacheAliasMmapPlan {
        LoaderCacheAliasMmapPlan {
            file_maps: vec![
                LoaderCacheAliasFileMapStep {
                    relative_start: 0,
                    raw_length: 0x1c1c8,
                    protection: libc::PROT_READ,
                    offset: 0,
                    fixed: false,
                },
                LoaderCacheAliasFileMapStep {
                    relative_start: 0x3000,
                    raw_length: 0x14000,
                    protection: libc::PROT_READ | libc::PROT_EXEC,
                    offset: 0x3000,
                    fixed: true,
                },
                LoaderCacheAliasFileMapStep {
                    relative_start: 0x17000,
                    raw_length: 0x4000,
                    protection: libc::PROT_READ,
                    offset: 0x17000,
                    fixed: true,
                },
                LoaderCacheAliasFileMapStep {
                    relative_start: 0x1b000,
                    raw_length: 0x1000,
                    protection: libc::PROT_READ | libc::PROT_WRITE,
                    offset: 0x1a000,
                    fixed: true,
                },
            ],
            zero_fill: LoaderCacheAliasZeroFillStep {
                relative_start: 0x1c000,
                raw_length: 0x1c8,
                protection: libc::PROT_READ | libc::PROT_WRITE,
            },
        }
    }

    fn synthetic_profiled_alias_elf() -> Vec<u8> {
        fn put16(bytes: &mut [u8], at: usize, value: u16) {
            bytes[at..at + 2].copy_from_slice(&value.to_le_bytes());
        }
        fn put32(bytes: &mut [u8], at: usize, value: u32) {
            bytes[at..at + 4].copy_from_slice(&value.to_le_bytes());
        }
        fn put64(bytes: &mut [u8], at: usize, value: u64) {
            bytes[at..at + 8].copy_from_slice(&value.to_le_bytes());
        }

        let mut bytes = vec![0_u8; 0x1b000];
        bytes[..7].copy_from_slice(b"\x7fELF\x02\x01\x01");
        put16(&mut bytes, 16, header::ET_DYN);
        put16(&mut bytes, 18, header::EM_X86_64);
        put32(&mut bytes, 20, 1);
        put64(&mut bytes, 32, 64);
        put16(&mut bytes, 52, 64);
        put16(&mut bytes, 54, 56);
        put16(&mut bytes, 56, 4);
        for (index, (offset, address, file_size, memory_size, flags)) in [
            (0, 0, 0x2a90, 0x2a90, ph::PF_R),
            (0x3000, 0x3000, 0x133a5, 0x133a5, ph::PF_R | ph::PF_X),
            (0x17000, 0x17000, 0x3064, 0x3064, ph::PF_R),
            (0x1abf0, 0x1bbf0, 0x410, 0x5d8, ph::PF_R | ph::PF_W),
        ]
        .into_iter()
        .enumerate()
        {
            let at = 64 + index * 56;
            put32(&mut bytes, at, ph::PT_LOAD);
            put32(&mut bytes, at + 4, flags);
            put64(&mut bytes, at + 8, offset);
            put64(&mut bytes, at + 16, address);
            put64(&mut bytes, at + 24, address);
            put64(&mut bytes, at + 32, file_size);
            put64(&mut bytes, at + 40, memory_size);
            put64(&mut bytes, at + 48, PAGE);
        }
        bytes
    }

    #[test]
    fn loader_cache_alias_mmap_plan_is_exactly_derived_from_pt_loads() {
        let expected = profiled_alias_mmap_plan_for_test();
        let bytes = synthetic_profiled_alias_elf();
        assert_eq!(loader_cache_alias_mmap_plan(&bytes).unwrap(), expected);

        let mut no_anonymous_bss = bytes.clone();
        let last_program_header = 64 + 3 * 56;
        no_anonymous_bss[last_program_header + 40..last_program_header + 48]
            .copy_from_slice(&0x410_u64.to_le_bytes());
        assert!(loader_cache_alias_mmap_plan(&no_anonymous_bss).is_err());

        let mut reordered = bytes.clone();
        let first = <[u8; 56]>::try_from(&reordered[64..120]).unwrap();
        let second = <[u8; 56]>::try_from(&reordered[120..176]).unwrap();
        reordered[64..120].copy_from_slice(&second);
        reordered[120..176].copy_from_slice(&first);
        assert!(loader_cache_alias_mmap_plan(&reordered).is_err());

        let mut nonfinal_bss = bytes.clone();
        nonfinal_bss[64 + 40..64 + 48].copy_from_slice(&0x2a91_u64.to_le_bytes());
        assert!(loader_cache_alias_mmap_plan(&nonfinal_bss).is_err());

        let mut unaligned_final_data = bytes.clone();
        unaligned_final_data[last_program_header + 32..last_program_header + 40]
            .copy_from_slice(&0x400_u64.to_le_bytes());
        assert!(loader_cache_alias_mmap_plan(&unaligned_final_data).is_err());

        let mut hole = bytes;
        let second_program_header = 64 + 56;
        hole[second_program_header + 16..second_program_header + 24]
            .copy_from_slice(&0x4000_u64.to_le_bytes());
        assert!(loader_cache_alias_mmap_plan(&hole).is_err());
    }

    #[test]
    fn loader_cache_alias_mmap_plan_refuses_every_argument_mutation() {
        let descriptor = 47;
        let base = 0x7100_0000;
        let plan = profiled_alias_mmap_plan_for_test();
        let requests = [
            [
                0,
                0x1c1c8,
                libc::PROT_READ as u64,
                (libc::MAP_PRIVATE | libc::MAP_DENYWRITE) as u64,
                descriptor,
                0,
            ],
            [
                base + 0x3000,
                0x14000,
                (libc::PROT_READ | libc::PROT_EXEC) as u64,
                (libc::MAP_PRIVATE | libc::MAP_DENYWRITE | libc::MAP_FIXED) as u64,
                descriptor,
                0x3000,
            ],
            [
                base + 0x17000,
                0x4000,
                libc::PROT_READ as u64,
                (libc::MAP_PRIVATE | libc::MAP_DENYWRITE | libc::MAP_FIXED) as u64,
                descriptor,
                0x17000,
            ],
            [
                base + 0x1b000,
                0x1000,
                (libc::PROT_READ | libc::PROT_WRITE) as u64,
                (libc::MAP_PRIVATE | libc::MAP_DENYWRITE | libc::MAP_FIXED) as u64,
                descriptor,
                0x1a000,
            ],
        ];
        for (index, request) in requests.into_iter().enumerate() {
            let request_base = (index != 0).then_some(base);
            assert!(plan.file_request_is_exact(index, request_base, descriptor, request));
            for argument in 0..request.len() {
                let mut changed = request;
                changed[argument] ^= 1;
                assert!(
                    !plan.file_request_is_exact(index, request_base, descriptor, changed),
                    "file mmap step {index} accepted changed argument {argument}",
                );
            }
        }

        let anonymous = [
            base + 0x1c000,
            0x1c8,
            (libc::PROT_READ | libc::PROT_WRITE) as u64,
            (libc::MAP_PRIVATE | libc::MAP_ANONYMOUS | libc::MAP_FIXED) as u64,
            u64::from(u32::MAX),
            0,
        ];
        assert!(plan.zero_fill_request_is_exact(Some(base), anonymous));
        for argument in 0..anonymous.len() {
            let mut changed = anonymous;
            changed[argument] ^= 1;
            assert!(
                !plan.zero_fill_request_is_exact(Some(base), changed),
                "anonymous mmap accepted changed argument {argument}",
            );
        }
    }

    #[test]
    fn loader_cache_alias_lifecycle_requires_exact_read_stat_map_close_sequence() {
        let descriptor = 47;
        let plan = profiled_alias_mmap_plan_for_test();
        let image = AfterLoaderImageId {
            generation: 3,
            file: crate::after_loader::FileIdentity {
                device: 0x21,
                inode: 110_440,
            },
        };
        let identity = MappingIdentity {
            device_major: 0,
            device_minor: 0x21,
            inode: 110_440,
        };
        let first = LoaderCacheAliasMapping {
            owned: AfterLoaderOwnedMapping {
                start: 0x7100_0000,
                end: 0x7101_d000,
                readable: true,
                writable: false,
                executable: false,
                shared: false,
                descriptor: Some(descriptor),
                offset: 0,
                purpose: AfterLoaderMappingPurpose::Image { image },
            },
            identity,
            raw_length: 0x1c1c8,
            fixed: false,
        };
        let second = LoaderCacheAliasMapping {
            owned: AfterLoaderOwnedMapping {
                start: 0x7100_3000,
                end: 0x7101_7000,
                readable: true,
                writable: false,
                executable: true,
                shared: false,
                descriptor: Some(descriptor),
                offset: 0x3000,
                purpose: AfterLoaderMappingPurpose::Image { image },
            },
            identity,
            raw_length: 0x14000,
            fixed: true,
        };
        let third = LoaderCacheAliasMapping {
            owned: AfterLoaderOwnedMapping {
                start: 0x7101_7000,
                end: 0x7101_b000,
                readable: true,
                writable: false,
                executable: false,
                shared: false,
                descriptor: Some(descriptor),
                offset: 0x17000,
                purpose: AfterLoaderMappingPurpose::Image { image },
            },
            identity,
            raw_length: 0x4000,
            fixed: true,
        };
        let fourth = LoaderCacheAliasMapping {
            owned: AfterLoaderOwnedMapping {
                start: 0x7101_b000,
                end: 0x7101_c000,
                readable: true,
                writable: true,
                executable: false,
                shared: false,
                descriptor: Some(descriptor),
                offset: 0x1a000,
                purpose: AfterLoaderMappingPurpose::Image { image },
            },
            identity,
            raw_length: 0x1000,
            fixed: true,
        };
        let geometry = ResolvedImageGeometry {
            image,
            mapping: identity,
            load_bias: 0x7100_0000,
            span: (0x7100_0000, 0x7101_d000),
        };
        let zero_fill = AfterLoaderOwnedMapping {
            start: 0x7101_c000,
            end: 0x7101_d000,
            readable: true,
            writable: true,
            executable: false,
            shared: false,
            descriptor: None,
            offset: 0,
            purpose: AfterLoaderMappingPurpose::ImageZeroFill { image },
        };

        let mut lifecycle = LoaderCacheAliasLifecycle::AwaitingOpen { image };
        let wrong_image = AfterLoaderImageId {
            generation: image.generation + 1,
            ..image
        };
        assert!(!lifecycle.complete_read(descriptor, 0, LOADER_CACHE_ALIAS_HEADER_BYTES));
        assert!(!lifecycle.complete_stat(descriptor));
        assert!(!lifecycle.complete_map(&plan, descriptor, image, first));
        assert!(!lifecycle.complete_close(&plan, descriptor, image, geometry));
        assert!(!lifecycle.complete_open(descriptor, wrong_image));
        assert!(lifecycle.complete_open(descriptor, image));
        assert!(!lifecycle.complete_open(descriptor, image));
        let read = [
            descriptor,
            0x7200_0000,
            LOADER_CACHE_ALIAS_HEADER_BYTES,
            0,
            0,
            0,
        ];
        assert!(loader_cache_alias_next_step_is_exact(
            &plan,
            &lifecycle,
            libc::SYS_read,
            read,
        ));
        assert!(!loader_cache_alias_next_step_is_exact(
            &plan,
            &lifecycle,
            libc::SYS_getpid,
            [0; 6],
        ));
        let mut wrong_read = read;
        wrong_read[0] += 1;
        assert!(!loader_cache_alias_next_step_is_exact(
            &plan,
            &lifecycle,
            libc::SYS_read,
            wrong_read,
        ));
        assert!(!lifecycle.complete_read(descriptor + 1, 0, LOADER_CACHE_ALIAS_HEADER_BYTES));
        assert!(!lifecycle.complete_read(descriptor, 1, LOADER_CACHE_ALIAS_HEADER_BYTES));
        assert!(lifecycle.complete_read(descriptor, 0, LOADER_CACHE_ALIAS_HEADER_BYTES));
        assert!(loader_cache_alias_next_step_is_exact(
            &plan,
            &lifecycle,
            libc::SYS_fstat,
            [descriptor, 0x7200_1000, 0, 0, 0, 0],
        ));
        assert!(!loader_cache_alias_next_step_is_exact(
            &plan,
            &lifecycle,
            libc::SYS_read,
            read,
        ));
        assert!(!lifecycle.complete_read(
            descriptor,
            LOADER_CACHE_ALIAS_HEADER_BYTES,
            LOADER_CACHE_ALIAS_HEADER_BYTES + 1,
        ));
        assert!(!lifecycle.complete_stat(descriptor + 1));
        assert!(lifecycle.complete_stat(descriptor));
        assert!(loader_cache_alias_next_step_is_exact(
            &plan,
            &lifecycle,
            libc::SYS_mmap,
            [
                0,
                0x1c1c8,
                libc::PROT_READ as u64,
                (libc::MAP_PRIVATE | libc::MAP_DENYWRITE) as u64,
                descriptor,
                0,
            ],
        ));
        assert!(!loader_cache_alias_next_step_is_exact(
            &plan,
            &lifecycle,
            libc::SYS_close,
            [descriptor, 0, 0, 0, 0, 0],
        ));
        assert!(!lifecycle.complete_map(&plan, descriptor + 1, image, first));
        assert!(!lifecycle.complete_map(&plan, descriptor, wrong_image, first));
        assert!(lifecycle.complete_map(&plan, descriptor, image, first));
        let wrong_fixed_args = [
            0x7100_3000,
            0x14000,
            libc::PROT_READ as u64,
            (libc::MAP_PRIVATE | libc::MAP_DENYWRITE | libc::MAP_FIXED) as u64,
            descriptor,
            0x3000,
        ];
        assert!(!loader_cache_alias_next_step_is_exact(
            &plan,
            &lifecycle,
            libc::SYS_mmap,
            wrong_fixed_args,
        ));
        assert!(!lifecycle.complete_map(
            &plan,
            descriptor,
            image,
            LoaderCacheAliasMapping {
                owned: AfterLoaderOwnedMapping {
                    readable: true,
                    executable: false,
                    ..second.owned
                },
                ..second
            },
        ));
        assert!(!lifecycle.complete_map(&plan, descriptor, image, first));
        assert!(!lifecycle.complete_map(
            &plan,
            descriptor,
            image,
            LoaderCacheAliasMapping {
                identity: MappingIdentity {
                    inode: identity.inode + 1,
                    ..identity
                },
                ..second
            },
        ));
        assert!(!lifecycle.complete_map(
            &plan,
            descriptor,
            image,
            LoaderCacheAliasMapping {
                owned: AfterLoaderOwnedMapping {
                    offset: 0x4000,
                    ..second.owned
                },
                ..second
            },
        ));
        assert!(lifecycle.complete_map(&plan, descriptor, image, second));
        assert!(!lifecycle.complete_map(&plan, descriptor, image, second));
        assert!(loader_cache_alias_next_step_is_exact(
            &plan,
            &lifecycle,
            libc::SYS_mmap,
            [
                0x7101_7000,
                0x4000,
                libc::PROT_READ as u64,
                (libc::MAP_PRIVATE | libc::MAP_DENYWRITE | libc::MAP_FIXED) as u64,
                descriptor,
                0x17000,
            ],
        ));
        assert!(lifecycle.complete_map(&plan, descriptor, image, third));
        assert!(loader_cache_alias_next_step_is_exact(
            &plan,
            &lifecycle,
            libc::SYS_mmap,
            [
                0x7101_b000,
                0x1000,
                (libc::PROT_READ | libc::PROT_WRITE) as u64,
                (libc::MAP_PRIVATE | libc::MAP_DENYWRITE | libc::MAP_FIXED) as u64,
                descriptor,
                0x1a000,
            ],
        ));
        assert!(lifecycle.complete_map(&plan, descriptor, image, fourth));
        assert!(loader_cache_alias_next_step_is_exact(
            &plan,
            &lifecycle,
            libc::SYS_mmap,
            [
                0x7101_c000,
                0x1c8,
                (libc::PROT_READ | libc::PROT_WRITE) as u64,
                (libc::MAP_PRIVATE | libc::MAP_ANONYMOUS | libc::MAP_FIXED) as u64,
                u64::from(u32::MAX),
                0,
            ],
        ));
        assert!(!loader_cache_alias_next_step_is_exact(
            &plan,
            &lifecycle,
            libc::SYS_mmap,
            [
                0x7101_c000,
                0x1c8,
                (libc::PROT_READ | libc::PROT_WRITE) as u64,
                (libc::MAP_PRIVATE | libc::MAP_ANONYMOUS | libc::MAP_FIXED) as u64,
                u64::MAX,
                0,
            ],
        ));
        assert!(!loader_cache_alias_next_step_is_exact(
            &plan,
            &lifecycle,
            libc::SYS_close,
            [descriptor, 0, 0, 0, 0, 0],
        ));
        assert!(!loader_cache_alias_next_step_is_exact(
            &plan,
            &lifecycle,
            libc::SYS_getpid,
            [0; 6],
        ));
        assert!(!lifecycle.complete_close(&plan, descriptor, image, geometry));
        assert!(!lifecycle.complete_zero_fill(&plan, wrong_image, 0x1c8, zero_fill,));
        assert!(!lifecycle.complete_zero_fill(
            &plan,
            image,
            0x1c8,
            AfterLoaderOwnedMapping {
                descriptor: Some(descriptor),
                ..zero_fill
            },
        ));
        assert!(!lifecycle.complete_zero_fill(
            &plan,
            image,
            0x1c8,
            AfterLoaderOwnedMapping {
                purpose: AfterLoaderMappingPurpose::Controller,
                ..zero_fill
            },
        ));
        assert!(!lifecycle.complete_zero_fill(
            &plan,
            image,
            0x1c8,
            AfterLoaderOwnedMapping {
                start: zero_fill.start + PAGE,
                end: zero_fill.end + PAGE,
                ..zero_fill
            },
        ));
        assert!(lifecycle.complete_zero_fill(&plan, image, 0x1c8, zero_fill));
        assert!(!lifecycle.complete_zero_fill(&plan, image, 0x1c8, zero_fill));
        assert!(loader_cache_alias_next_step_is_exact(
            &plan,
            &lifecycle,
            libc::SYS_close,
            [descriptor, 0, 0, 0, 0, 0],
        ));
        assert!(!lifecycle.complete_close(
            &plan,
            descriptor,
            image,
            ResolvedImageGeometry {
                mapping: MappingIdentity {
                    inode: identity.inode + 1,
                    ..identity
                },
                ..geometry
            },
        ));
        assert!(!lifecycle.complete_close(&plan, descriptor + 1, image, geometry));
        assert!(!lifecycle.complete_close(&plan, descriptor, wrong_image, geometry));
        assert!(lifecycle.complete_close(&plan, descriptor, image, geometry));
        assert!(!lifecycle.complete_close(&plan, descriptor, image, geometry));
        assert!(!lifecycle.is_consumed());
        assert!(loader_cache_alias_next_step_is_exact(
            &plan,
            &lifecycle,
            libc::SYS_mprotect,
            [0x7101_b000, PAGE, libc::PROT_READ as u64, 0, 0, 0],
        ));
        assert!(!loader_cache_alias_next_step_is_exact(
            &plan,
            &lifecycle,
            libc::SYS_munmap,
            [0x7101_b000, PAGE, 0, 0, 0, 0],
        ));
        assert!(!lifecycle.complete_relro(
            image,
            ResolvedImageGeometry {
                load_bias: geometry.load_bias + PAGE,
                ..geometry
            },
        ));
        assert!(lifecycle.complete_relro(image, geometry));
        assert!(lifecycle.is_consumed());
        assert!(loader_cache_alias_next_step_is_exact(
            &plan,
            &lifecycle,
            libc::SYS_getpid,
            [0; 6],
        ));
        assert!(!lifecycle.complete_relro(image, geometry));
    }

    #[test]
    fn loader_cache_procfs_absence_requires_exact_not_found() {
        assert!(!classify_procfs_absence(Ok(())).unwrap());
        assert!(
            classify_procfs_absence::<()>(Err(io::Error::from(io::ErrorKind::NotFound,))).unwrap()
        );
        for kind in [
            io::ErrorKind::PermissionDenied,
            io::ErrorKind::Other,
            io::ErrorKind::UnexpectedEof,
        ] {
            assert_eq!(
                classify_procfs_absence::<()>(Err(io::Error::from(kind)))
                    .unwrap_err()
                    .kind(),
                kind,
            );
        }
    }

    #[test]
    fn loader_cache_scratch_requires_the_production_guard_rw_guard_transition() {
        let base = 0x6000_0000;
        let reservation = (base, base + 3 * PAGE);
        let page = base + PAGE;
        let mut state = mapping_state(1);
        let mut initial = controller_mapping(base, reservation.1);
        initial.readable = false;
        initial.writable = false;
        state.owned_mappings.push(initial);
        state.protect_owned_range((page, page + PAGE), libc::PROT_READ | libc::PROT_WRITE);
        let owner = *state
            .owned_mappings
            .iter()
            .find(|mapping| mapping.start == page && mapping.end == page + PAGE)
            .unwrap();
        assert_eq!(owner.offset, PAGE);
        let scratch = (
            page + LOADER_CACHE_SCRATCH_OFFSET,
            page + LOADER_CACHE_SCRATCH_END_OFFSET,
        );
        assert!(loader_cache_scratch_metadata_is_exact(
            reservation,
            scratch,
            owner,
        ));
        assert!(loader_cache_scratch_logical_reservation_is_exact(
            reservation,
            scratch,
            owner,
            &state.owned_mappings,
        ));

        for changed in [
            {
                let mut changed = state.owned_mappings.clone();
                changed.remove(0);
                changed
            },
            {
                let mut changed = state.owned_mappings.clone();
                changed.pop();
                changed
            },
            {
                let mut changed = state.owned_mappings.clone();
                changed[0].readable = true;
                changed
            },
            {
                let mut changed = state.owned_mappings.clone();
                changed[2].writable = true;
                changed
            },
            {
                let mut changed = state.owned_mappings.clone();
                changed[1].offset = 0;
                changed
            },
            {
                let mut changed = state.owned_mappings.clone();
                changed.push(owner);
                changed
            },
        ] {
            assert!(!loader_cache_scratch_logical_reservation_is_exact(
                reservation,
                scratch,
                owner,
                &changed,
            ));
        }
        assert!(!loader_cache_scratch_metadata_is_exact(
            (base, base + 2 * PAGE),
            scratch,
            owner,
        ));
        assert!(!loader_cache_scratch_owner_shape_is_exact(
            scratch,
            AfterLoaderOwnedMapping {
                writable: false,
                ..owner
            },
        ));

        let middle = GuestMap {
            start: page,
            end: page + PAGE,
            offset: 0,
            device_major: 0,
            device_minor: 0,
            readable: true,
            writable: true,
            executable: false,
            shared: false,
            inode: 0,
            path: None,
        };
        let attributes = Some(GuestHookMappingAttributes {
            fork_safe: true,
            protection_key: 0,
        });
        assert!(loader_cache_scratch_physical_map_is_exact(
            owner, &middle, attributes,
        ));
        for changed in [
            GuestMap {
                end: middle.end + PAGE,
                ..middle.clone()
            },
            GuestMap {
                end: middle.end - 1,
                ..middle.clone()
            },
            GuestMap {
                writable: false,
                ..middle.clone()
            },
            GuestMap {
                inode: 17,
                path: Some(PathBuf::from("/remapped")),
                ..middle.clone()
            },
        ] {
            assert!(!loader_cache_scratch_physical_map_is_exact(
                owner, &changed, attributes,
            ));
        }
        assert!(!loader_cache_scratch_physical_map_is_exact(
            owner,
            &middle,
            Some(GuestHookMappingAttributes {
                fork_safe: false,
                protection_key: 0,
            }),
        ));
        assert!(!loader_cache_scratch_physical_map_is_exact(
            owner,
            &middle,
            Some(GuestHookMappingAttributes {
                fork_safe: true,
                protection_key: 1,
            }),
        ));

        let lower = GuestMap {
            start: base,
            end: page,
            readable: false,
            writable: false,
            ..middle.clone()
        };
        let upper = GuestMap {
            start: page + PAGE,
            end: reservation.1,
            ..lower.clone()
        };
        assert!(loader_cache_scratch_physical_guard_is_exact(
            (base, page),
            true,
            &lower,
            attributes,
        ));
        assert!(loader_cache_scratch_physical_guard_is_exact(
            (page + PAGE, reservation.1),
            false,
            &upper,
            attributes,
        ));
        assert!(!loader_cache_scratch_physical_guard_is_exact(
            (base, page),
            true,
            &GuestMap {
                readable: true,
                ..lower.clone()
            },
            attributes,
        ));
        assert!(!loader_cache_scratch_physical_guard_is_exact(
            (page + PAGE, reservation.1),
            false,
            &GuestMap {
                start: page + PAGE + 1,
                ..upper
            },
            attributes,
        ));

        let lifecycle_mapping = LoaderCacheMapping {
            start: 0x7000_0000,
            raw_length: PAGE,
            end: 0x7000_1000,
            identity: MappingIdentity {
                device_major: 0,
                device_minor: 1,
                inode: 43,
            },
        };
        state.loader_cache = Some(LoaderCacheState {
            scratch_reservation: reservation,
            scratch,
            scratch_owner: owner,
            scratch_preimage: vec![0; LOADER_CACHE_SCRATCH_BYTES],
            lifecycle: LoaderCacheLifecycle::Retired {
                descriptor: 41,
                mapping: lifecycle_mapping,
            },
            aliases: BTreeMap::new(),
        });
        assert!(state.loader_cache_scratch_overlaps((base, base + PAGE)));
        assert!(state.loader_cache_scratch_overlaps((base + 2 * PAGE, reservation.1)));
        let exact_release = [base, 3 * PAGE, 0, 0, 0, 0];
        assert!(!state.loader_cache_scratch_release_is_armed_for(exact_release, reservation,));
        state.loader_cache.as_mut().unwrap().lifecycle =
            LoaderCacheLifecycle::ScratchReleaseArmed {
                descriptor: 41,
                mapping: lifecycle_mapping,
            };
        assert!(state.loader_cache_scratch_release_is_armed_for(exact_release, reservation,));
        for changed in [
            [base + PAGE, 3 * PAGE, 0, 0, 0, 0],
            [base, 3 * PAGE - 1, 0, 0, 0, 0],
            [base, 3 * PAGE + 1, 0, 0, 0, 0],
            [base, 2 * PAGE + 1, 0, 0, 0, 0],
            [base, 3 * PAGE, 1, 0, 0, 0],
        ] {
            assert!(!state.loader_cache_scratch_release_is_armed_for(changed, reservation,));
        }
        assert!(
            !state.loader_cache_scratch_release_is_armed_for(exact_release, (page, page + PAGE),)
        );
        state.remove_owned_range(reservation);
        assert!(
            state
                .owned_mappings
                .iter()
                .all(|mapping| !ranges_overlap((mapping.start, mapping.end), reservation))
        );
        assert!(
            state
                .loader_cache
                .as_mut()
                .unwrap()
                .lifecycle
                .complete_scratch_release(41, lifecycle_mapping)
        );
        assert!(!state.loader_cache_scratch_overlaps(reservation));
    }

    #[test]
    fn loader_cache_redirect_binds_every_register_word_and_xstate() {
        let admission = core::array::from_fn(|index| 0x1000 + index as u64);
        let executed_path = 0x6000_1a08;
        let logical_path = admission[13];
        let result = 41_i64;

        let mut executed = admission;
        executed[13] = executed_path;
        assert!(loader_cache_redirect_register_words_match(
            admission,
            executed_path,
            executed,
        ));
        for index in 0..executed.len() {
            let mut changed = executed;
            changed[index] ^= 1;
            assert!(
                !loader_cache_redirect_register_words_match(admission, executed_path, changed,),
                "redirect register word {index} was not bound",
            );
        }

        let mut completed = admission;
        completed[10] = result as u64;
        completed[13] = executed_path;
        assert!(loader_cache_completion_register_words_match(
            admission,
            executed_path,
            result,
            completed,
        ));
        for index in 0..completed.len() {
            let mut changed = completed;
            changed[index] ^= 1;
            assert!(
                !loader_cache_completion_register_words_match(
                    admission,
                    executed_path,
                    result,
                    changed,
                ),
                "completion register word {index} was not bound",
            );
        }
        completed[13] = logical_path;
        assert!(loader_cache_completion_register_words_match(
            admission,
            logical_path,
            result,
            completed,
        ));

        let xstate = zeroed_test_xstate();
        let mut changed_fpregs = unsafe { core::mem::zeroed::<safeptrace::FpRegs>() };
        changed_fpregs.cwd = 1;
        let changed_xstate = safeptrace::X86ExtendedState::Fxsave64(Box::new(changed_fpregs));
        assert_eq!(xstate, xstate.clone());
        assert_ne!(xstate, changed_xstate);
    }

    #[test]
    fn loader_cache_stat_output_is_byte_exact_including_padding() {
        let fields = AfterLoaderStatFields {
            device: 1,
            inode: 2,
            mode: 0o100444,
            links: 3,
            uid: 4,
            gid: 5,
            rdev: 6,
            size: 7,
            block_size: 8,
            blocks: 9,
            access_seconds: 10,
            access_nanoseconds: 11,
            modify_seconds: 12,
            modify_nanoseconds: 13,
            change_seconds: 14,
            change_nanoseconds: 15,
        };
        let exact = fields.exact_x86_64_output();
        assert!(stat_output_matches(fields, &exact));
        for index in 0..exact.len() {
            let mut changed = exact;
            changed[index] ^= 1;
            assert!(
                !stat_output_matches(fields, &changed),
                "loader-cache stat mutation {index} was admitted",
            );
        }
        let mut later_access = fields;
        later_access.access_seconds += 86_400;
        later_access.access_nanoseconds ^= 1;
        assert_ne!(
            fields.exact_x86_64_output(),
            later_access.exact_x86_64_output()
        );
        assert_eq!(
            fields
                .with_deterministic_access_time()
                .exact_x86_64_output(),
            later_access
                .with_deterministic_access_time()
                .exact_x86_64_output(),
            "guest-visible alias fstat retained ambient access time",
        );
    }

    #[test]
    fn loader_cache_permit_keeps_logical_and_executed_arguments_distinct() {
        let logical_path = 0x7f00_0002_d266;
        let scratch = (0x6000_0a08, 0x6000_0c00);
        let flags = (libc::O_RDONLY | libc::O_CLOEXEC) as u64;
        let logical = [
            u64::from(libc::AT_FDCWD as u32),
            logical_path,
            flags,
            0,
            flags,
            logical_path,
        ];
        let number = libc::SYS_openat;
        let instruction_pointer = 0x7f00_0002_57b6;
        let resume_pointer = instruction_pointer + 2;
        let mut admission_registers = [0; 27];
        admission_registers[7] = logical[3];
        admission_registers[8] = logical[5];
        admission_registers[9] = logical[4];
        admission_registers[12] = logical[2];
        admission_registers[13] = logical[1];
        admission_registers[14] = logical[0];
        admission_registers[15] = number as u64;
        admission_registers[16] = resume_pointer;
        let redirect = LoaderCacheRedirect {
            scratch,
            preimage: vec![0xa5; LOADER_CACHE_SCRATCH_BYTES],
            path: b"/proc/17/fd/41\0".to_vec(),
            admission_registers,
            admission_xstate: zeroed_test_xstate(),
        };
        let executed = loader_cache_redirect_arguments(logical, &redirect).unwrap();
        assert_eq!(executed[1], scratch.0);
        assert_eq!(executed[5], logical_path);
        let mut permit = AfterLoaderSyscallPermit {
            image: None,
            tid: Pid::from_raw(17),
            generation: 23,
            physical_generation: None,
            origin_status: None,
            admission_status: None,
            purpose: AfterLoaderSyscallPurpose::PrivateSetup,
            number,
            args: logical,
            executed_args: executed,
            instruction_pointer,
            resume_pointer,
            instruction: [0x0f, 0x05, 0, 0],
            instruction_length: 2,
            output_spans: Vec::new(),
            effect: AfterLoaderSyscallEffect::OpenLoaderCache {
                descriptor: AfterLoaderOwnedDescriptor::LoaderCache {
                    file: crate::after_loader::FileIdentity {
                        device: 1,
                        inode: 41,
                    },
                    mapping: MappingIdentity {
                        device_major: 0,
                        device_minor: 1,
                        inode: 41,
                    },
                    length: 0x12345,
                    position: 0,
                },
                redirect,
            },
        };
        assert!(after_loader_permit_argument_binding_is_exact(&permit));
        permit.executed_args[5] = scratch.0;
        assert!(!after_loader_permit_argument_binding_is_exact(&permit));
        permit.executed_args = executed;
        permit.args[1] += 1;
        assert!(!after_loader_permit_argument_binding_is_exact(&permit));

        let mut malformed = match permit.effect.clone() {
            AfterLoaderSyscallEffect::OpenLoaderCache { redirect, .. } => redirect,
            _ => unreachable!(),
        };
        malformed.path.pop();
        assert!(loader_cache_redirect_arguments(logical, &malformed).is_none());
        malformed.path = vec![b'x'; LOADER_CACHE_SCRATCH_BYTES + 1];
        malformed.path.push(0);
        assert!(loader_cache_redirect_arguments(logical, &malformed).is_none());
        malformed.path = b"/proc/17/fd/41\0".to_vec();
        malformed.preimage.pop();
        assert!(loader_cache_redirect_arguments(logical, &malformed).is_none());

        let mut ordinary = permit;
        ordinary.args = logical;
        ordinary.executed_args = logical;
        ordinary.effect = AfterLoaderSyscallEffect::None;
        assert!(after_loader_permit_argument_binding_is_exact(&ordinary));
        ordinary.executed_args[1] = scratch.0;
        assert!(!after_loader_permit_argument_binding_is_exact(&ordinary));
    }

    #[test]
    fn ptmalloc_bootstrap_request_is_one_exact_raw_shape() {
        let load_bias = 0x7f00_0000_0000;
        let permit = exact_ptmalloc_test_permit(load_bias);
        let admission_rax = -(libc::ENOSYS as i64) as u64;
        assert!(exact_ptmalloc_bootstrap_request(
            load_bias,
            admission_rax,
            &permit
        ));

        let mut mutations = Vec::new();
        let mut changed = permit.clone();
        changed.number = libc::SYS_read;
        mutations.push(changed);
        for (index, value) in [
            (0, permit.args[0] - 1),
            (0, permit.args[0] + 1),
            (1, 0),
            (1, 7),
            (1, 9),
            (1, u64::MAX),
            (2, 0),
            (2, libc::GRND_RANDOM as u64),
            (2, 1_u64 << 63),
        ] {
            let mut changed = permit.clone();
            changed.args[index] = value;
            mutations.push(changed);
        }
        let mut changed = permit.clone();
        changed.instruction_pointer += 1;
        mutations.push(changed);
        let mut changed = permit.clone();
        changed.resume_pointer += 1;
        mutations.push(changed);
        let mut changed = permit.clone();
        changed.instruction[0] ^= 1;
        mutations.push(changed);
        let mut changed = permit.clone();
        changed.instruction_length = 4;
        mutations.push(changed);
        let mut changed = permit.clone();
        changed.output_spans[0].0 += 1;
        mutations.push(changed);
        let mut changed = permit.clone();
        changed.output_spans.push((0x10, 0x18));
        mutations.push(changed);

        for (index, changed) in mutations.iter().enumerate() {
            assert!(
                !exact_ptmalloc_bootstrap_request(load_bias, admission_rax, changed),
                "mutation {index} was accepted: {changed:?}"
            );
        }
        for changed_rax in [0, 8, libc::SYS_getrandom as u64, u64::MAX] {
            assert!(
                !exact_ptmalloc_bootstrap_request(load_bias, changed_rax, &permit),
                "alternate admission RAX {changed_rax:#x} was accepted"
            );
        }
        assert!(!exact_ptmalloc_bootstrap_request(
            load_bias + 1,
            admission_rax,
            &permit
        ));
        assert!(!exact_ptmalloc_bootstrap_request(
            u64::MAX,
            admission_rax,
            &permit
        ));
    }

    #[test]
    fn ptmalloc_bootstrap_admission_state_is_one_shot_and_zero_preimage() {
        let load_bias = 0x7f00_0000_0000;
        let entry = PtmallocBootstrapObservation {
            initialized: 0,
            tcache_key: [0; 8],
        };
        let profile = PtmallocBootstrapState {
            load_bias,
            entry,
            pre_dlopen_verified: true,
            entropy_consumed: false,
            injected_key: None,
        };
        let observed = PtmallocBootstrapObservation {
            initialized: 1,
            tcache_key: [0; 8],
        };
        assert!(exact_ptmalloc_bootstrap_admission_state(
            profile, load_bias, observed
        ));

        let mut changed = profile;
        changed.pre_dlopen_verified = false;
        assert!(!exact_ptmalloc_bootstrap_admission_state(
            changed, load_bias, observed
        ));
        let mut changed = profile;
        changed.entry.initialized = 1;
        assert!(!exact_ptmalloc_bootstrap_admission_state(
            changed, load_bias, observed
        ));
        let mut changed = profile;
        changed.entry.tcache_key[0] = 1;
        assert!(!exact_ptmalloc_bootstrap_admission_state(
            changed, load_bias, observed
        ));
        let mut changed = profile;
        changed.entropy_consumed = true;
        assert!(!exact_ptmalloc_bootstrap_admission_state(
            changed, load_bias, observed
        ));
        let mut changed = profile;
        changed.injected_key = Some([1; 8]);
        assert!(!exact_ptmalloc_bootstrap_admission_state(
            changed, load_bias, observed
        ));
        let mut changed_observed = observed;
        changed_observed.initialized = 0;
        assert!(!exact_ptmalloc_bootstrap_admission_state(
            profile,
            load_bias,
            changed_observed
        ));
        let mut changed_observed = observed;
        changed_observed.tcache_key[7] = 1;
        assert!(!exact_ptmalloc_bootstrap_admission_state(
            profile,
            load_bias,
            changed_observed
        ));
        assert!(!exact_ptmalloc_bootstrap_admission_state(
            profile,
            load_bias + 1,
            observed
        ));
    }

    #[test]
    fn ptmalloc_bootstrap_profile_constants_are_byte_exact() {
        assert_eq!(PTMALLOC_BOOTSTRAP_FUNCTION.len(), 485);
        let digest: [u8; 32] = Sha256::digest(PTMALLOC_BOOTSTRAP_FUNCTION).into();
        assert_eq!(
            digest,
            [
                0x31, 0xbc, 0xaf, 0x2b, 0xee, 0x7c, 0xc7, 0x6a, 0xe3, 0x96, 0x03, 0xc1, 0x22, 0xf1,
                0x31, 0x23, 0xff, 0x72, 0x08, 0xa4, 0x74, 0xdf, 0xd6, 0xaf, 0x1c, 0x98, 0x2d, 0xe8,
                0xb6, 0xac, 0xdd, 0x62,
            ]
        );
        let syscall_offset =
            usize::try_from(PTMALLOC_BOOTSTRAP_SYSCALL_RVA - PTMALLOC_BOOTSTRAP_FUNCTION_RVA)
                .unwrap();
        assert_eq!(
            &PTMALLOC_BOOTSTRAP_FUNCTION[syscall_offset..syscall_offset + 2],
            &[0x0f, 0x05]
        );
        assert_eq!(ptmalloc_bootstrap_profile_matches(&[]), Ok(false));
        assert_eq!(
            ptmalloc_bootstrap_profile_matches(PTMALLOC_BOOTSTRAP_FUNCTION),
            Ok(false)
        );
    }

    #[test]
    fn ptmalloc_bootstrap_completion_changes_only_rax() {
        let admission: libc::user_regs_struct = unsafe { std::mem::zeroed() };
        let mut completed = admission;
        completed.rax = 8;
        assert!(ptmalloc_bootstrap_completion_registers_match(
            &admission, &completed
        ));
        macro_rules! reject_field_mutations {
            ($($field:ident),+ $(,)?) => {
                $(
                    let mut changed = completed;
                    changed.$field ^= 1;
                    assert!(
                        !ptmalloc_bootstrap_completion_registers_match(&admission, &changed),
                        "register {} mutation was accepted",
                        stringify!($field),
                    );
                )+
            };
        }
        reject_field_mutations!(
            r15, r14, r13, r12, rbp, rbx, r11, r10, r9, r8, rax, rcx, rdx, rsi, rdi, orig_rax, rip,
            cs, eflags, rsp, ss, fs_base, gs_base, ds, es, fs, gs,
        );
    }

    #[test]
    fn alternate_stack_compares_all_abi_fields_and_preserves_raw_padding_evidence() {
        let before = [0_u8; 24];
        for index in (0..12).chain(16..24) {
            let mut after = before;
            after[index] = 1;
            assert_ne!(
                altstack_fields(&before),
                altstack_fields(&after),
                "field byte {index}"
            );
        }
        let mut padding = before;
        padding[12..16].fill(0xff);
        assert_eq!(altstack_fields(&before), altstack_fields(&padding));
        assert_ne!(before, padding);
    }

    #[test]
    fn complete_environment_rejects_duplicate_keys_and_truncation() {
        let actual = environment_map(b"A=one\0B=two=three\0").unwrap();
        assert_eq!(
            actual.get(&OsString::from("B")),
            Some(&OsString::from("two=three"))
        );
        assert!(environment_map(b"A=one\0A=two\0").is_err());
        assert!(environment_map(b"A=one").is_err());
        assert!(environment_map(b"=empty-key\0").is_err());
    }
}
