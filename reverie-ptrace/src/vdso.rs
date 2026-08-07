/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! Provides APIs to disable VDSOs at runtime.
use std::collections::HashMap;
use std::sync::LazyLock;

use goblin::elf::Elf;
use nix::sys::mman::ProtFlags;
use nix::unistd;
use reverie::Errno;
use reverie::Error;
use reverie::Guest;
use reverie::Subscription;
use reverie::Tool;
use reverie::syscalls::Addr;
use reverie::syscalls::AddrMut;
use reverie::syscalls::MemoryAccess;
use reverie::syscalls::Mprotect;
use reverie::syscalls::Sysno;
use tracing::debug;

#[repr(align(64))]
struct BufferAligned<const N: usize>([u8; N]);

// Byte code for the new pseudo vdso functions which do the actual syscalls.
// Note: the byte code must be 8 bytes aligned
#[cfg(target_arch = "x86_64")]
mod vdso_syms {
    #![allow(non_upper_case_globals)]

    use crate::vdso::BufferAligned;

    const time_code: BufferAligned<8> = BufferAligned::<8>([
        0xb8, 0xc9, 0x00, 0x00, 0x00, // mov %SYS_time, %eax
        0x0f, 0x05, // syscall
        0xc3, // retq
    ]);

    pub const time: &[u8; 8] = &time_code.0;

    const clock_gettime_code: BufferAligned<8> = BufferAligned::<8>([
        0xb8, 0xe4, 0x00, 0x00, 0x00, // mov SYS_clock_gettime, %eax
        0x0f, 0x05, // syscall
        0xc3, // retq
    ]);

    pub const clock_gettime: &[u8; 8] = &clock_gettime_code.0;

    const getcpu_code: BufferAligned<8> = BufferAligned::<8>([
        0xb8, 0x35, 0x01, 0x00, 0x00, // mov SYS_getcpu, %eax
        0x0f, 0x05, // syscall
        0xc3, // retq
    ]);

    pub const getcpu: &[u8; 8] = &getcpu_code.0;

    const gettimeofday_code: BufferAligned<8> = BufferAligned::<8>([
        0xb8, 0x60, 0x00, 0x00, 0x00, // mov SYS_gettimeofday, %eax
        0x0f, 0x05, // syscall
        0xc3, // retq
    ]);

    pub const gettimeofday: &[u8; 8] = &gettimeofday_code.0;

    const clock_getres_code: BufferAligned<8> = BufferAligned::<8>([
        0xb8, 0xe5, 0x00, 0x00, 0x00, // mov SYS_clock_getres, %eax
        0x0f, 0x05, // syscall
        0xc3, // retq
    ]);

    pub const clock_getres: &[u8; 8] = &clock_getres_code.0;

    const getrandom_code: BufferAligned<8> = BufferAligned::<8>([
        0xb8, 0x3e, 0x01, 0x00, 0x00, // mov SYS_getrandom, %eax
        0x0f, 0x05, // syscall
        0xc3, // retq
    ]);

    pub const getrandom: &[u8; 8] = &getrandom_code.0;
}

#[cfg(target_arch = "aarch64")]
mod vdso_syms {
    #![allow(non_upper_case_globals)]

    // See this example for how to generate the byte code: https://godbolt.org/z/hbzK7Ydc3
    //
    // Example below:
    // ```
    // __attribute__((noinline)) static int sys_gettimeofday(void) {
    //     register long x0 __asm__("x0");
    //     asm volatile("bti c; mov x8, 169; svc 0" : "=r"(x0) : : "memory", "cc");
    //     return (int)x0;
    // }
    // ```
    //
    // Notes:
    //  * The byte order below may be different from what the disassembler will
    //    show. aarch64 is little-endian by default whereas the 4-byte
    //    instructions are usually displayed in big-endian.
    //  * The aarch64 calling convention matches syscall arguments, so no need
    //    to adjust registers x0-x5 or the stack pointer before calling the
    //    syscall.
    //  * The `bti c` instruction is the "Branch Target Identification"
    //    instruction. This is here because this is the first instruction of the
    //    vdso function and will be the branch target. This also effectively
    //    serves as a NOP instruction to pad out the size of the thunk.
    //    See also
    //    https://developer.arm.com/documentation/ddi0596/2021-06/Base-Instructions/BTI--Branch-Target-Identification-

    use crate::vdso::BufferAligned;

    const clock_getres_code: BufferAligned<16> = BufferAligned::<16>([
        0x5f, 0x24, 0x03, 0xd5, // bti c
        0x48, 0x0e, 0x80, 0xd2, // mov x8, 114 (#__NR_clock_getres)
        0x01, 0x00, 0x00, 0xd4, // svc 0
        0xc0, 0x03, 0x5f, 0xd6, // ret
    ]);

    pub const clock_getres: &[u8; 16] = &clock_getres_code.0;

    const clock_gettime_code: BufferAligned<16> = BufferAligned::<16>([
        0x5f, 0x24, 0x03, 0xd5, // bti c
        0x28, 0x0e, 0x80, 0xd2, // mov x8, 113 (#__NR_clock_gettime)
        0x01, 0x00, 0x00, 0xd4, // svc 0
        0xc0, 0x03, 0x5f, 0xd6, // ret
    ]);

    pub const clock_gettime: &[u8; 16] = &clock_gettime_code.0;

    const gettimeofday_code: BufferAligned<16> = BufferAligned::<16>([
        0x5f, 0x24, 0x03, 0xd5, // bti c
        0x28, 0x15, 0x80, 0xd2, // mov x8, 169 (#__NR_gettimeofday)
        0x01, 0x00, 0x00, 0xd4, // svc 0
        0xc0, 0x03, 0x5f, 0xd6, // ret
    ]);

    pub const gettimeofday: &[u8; 16] = &gettimeofday_code.0;

    // On aarch64, the vdso version of rt_sigreturn is only 8 bytes, so our
    // patch can't exceed that size. However, since this syscall doesn't return,
    // we can just call it without the `ret` instruction.
    //
    // NOTE: This is currently *exactly* how the kernel implements the
    // rt_sigreturn vdso, so we could probably get away with not even patching
    // it. See also `linux/arch/arm64/kernel/vdso/sigreturn.S`.
    const rt_sigreturn_code: BufferAligned<8> = BufferAligned::<8>([
        0x68, 0x11, 0x80, 0xd2, // mov x8, 139 (#__NR_rt_sigreturn)
        0x01, 0x00, 0x00, 0xd4, // svc 0
    ]);

    pub const rt_sigreturn: &[u8; 8] = &rt_sigreturn_code.0;

    const getrandom_code: BufferAligned<16> = BufferAligned::<16>([
        0x5f, 0x24, 0x03, 0xd5, // bti c
        0xc8, 0x22, 0x80, 0xd2, // mov x8, 278 (#__NR_getrandom)
        0x01, 0x00, 0x00, 0xd4, // svc 0
        0xc0, 0x03, 0x5f, 0xd6, // ret
    ]);

    pub const getrandom: &[u8; 16] = &getrandom_code.0;
}

#[cfg(target_arch = "x86_64")]
const VDSO_SYMBOLS: &[(&str, Sysno, &[u8])] = &[
    ("__vdso_time", Sysno::time, vdso_syms::time),
    (
        "__vdso_clock_gettime",
        Sysno::clock_gettime,
        vdso_syms::clock_gettime,
    ),
    ("__vdso_getcpu", Sysno::getcpu, vdso_syms::getcpu),
    (
        "__vdso_gettimeofday",
        Sysno::gettimeofday,
        vdso_syms::gettimeofday,
    ),
    (
        "__vdso_clock_getres",
        Sysno::clock_getres,
        vdso_syms::clock_getres,
    ),
    ("__vdso_getrandom", Sysno::getrandom, vdso_syms::getrandom),
];

#[cfg(target_arch = "aarch64")]
const VDSO_SYMBOLS: &[(&str, Sysno, &[u8])] = &[
    (
        "__kernel_clock_getres",
        Sysno::clock_getres,
        vdso_syms::clock_getres,
    ),
    (
        "__kernel_clock_gettime",
        Sysno::clock_gettime,
        vdso_syms::clock_gettime,
    ),
    (
        "__kernel_gettimeofday",
        Sysno::gettimeofday,
        vdso_syms::gettimeofday,
    ),
    (
        "__kernel_rt_sigreturn",
        Sysno::rt_sigreturn,
        vdso_syms::rt_sigreturn,
    ),
    ("__kernel_getrandom", Sysno::getrandom, vdso_syms::getrandom),
];

/// Rounds up `value` so that it is a multiple of `alignment`.
fn align_up(value: usize, alignment: usize) -> usize {
    (value + alignment - 1) & alignment.wrapping_neg()
}

/// Per-symbol VDSO patch info: `symbol name -> (base offset, size, replacement bytes)`.
#[derive(Debug, Clone, Copy)]
struct VdsoPatch {
    name: &'static str,
    offset: u64,
    size: usize,
    bytes: &'static [u8],
}

type VdsoPatchInfo = Vec<VdsoPatch>;
type VdsoSymbolInfo = HashMap<&'static str, (u64, usize)>;

/// Evidence that every required vDSO symbol was patched and read back exactly.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VdsoPatchReceipt {
    required_symbols: usize,
    patched_symbols: Vec<&'static str>,
    verified_bytes: usize,
}

/// A failure to establish or verify the complete vDSO patch.
#[derive(Debug, Clone, thiserror::Error)]
pub enum VdsoPatchError {
    #[error("could not inspect {target}: {detail}")]
    Inspect { target: String, detail: String },

    #[error("no vDSO mapping found for {target}")]
    MissingVdsoMapping { target: String },

    #[error("required vDSO symbol {name} is missing")]
    MissingRequiredSymbol { name: &'static str },

    #[error("required vDSO symbol {name} is not a function")]
    InvalidSymbolType { name: &'static str },

    #[error(
        "vDSO symbol {name} has {available} aligned bytes, fewer than the {required} replacement bytes"
    )]
    ReplacementTooLarge {
        name: &'static str,
        available: usize,
        required: usize,
    },

    #[error("vDSO patch address for {name} is null (computed address {address:#x})")]
    InvalidPatchAddress { name: &'static str, address: u64 },

    #[error("could not make tracee {pid}'s vDSO writable: {source}")]
    Unprotect { pid: i32, source: Errno },

    #[error("could not write tracee {pid}'s vDSO symbol {name}: {source}")]
    Write {
        pid: i32,
        name: &'static str,
        source: Errno,
    },

    #[error("could not restore tracee {pid}'s vDSO protections: {source}")]
    Reprotect { pid: i32, source: Errno },

    #[error("could not read back tracee {pid}'s vDSO symbol {name}: {source}")]
    ReadBack {
        pid: i32,
        name: &'static str,
        source: Errno,
    },

    #[error(
        "vDSO patch verification failed for {name}: expected {expected:?}, observed {observed:?}"
    )]
    VerificationMismatch {
        name: &'static str,
        expected: Vec<u8>,
        observed: Vec<u8>,
    },
}

fn build_vdso_patch_info(
    info: &VdsoSymbolInfo,
    subscriptions: &Subscription,
) -> Result<VdsoPatchInfo, VdsoPatchError> {
    let mut patches = Vec::with_capacity(VDSO_SYMBOLS.len());

    for (name, syscall, bytes) in VDSO_SYMBOLS {
        if !subscriptions
            .iter_syscalls()
            .any(|subscribed| subscribed == *syscall)
        {
            continue;
        }
        let &(offset, size) = info
            .get(name)
            .ok_or(VdsoPatchError::MissingRequiredSymbol { name })?;

        // There is padding at the end of every vDSO entry to bring it up to a
        // 16-byte size alignment. The dynamic symbol table reports the
        // unaligned size, so include the padding in the verified patch image.
        let aligned_size = align_up(size, 16);
        if bytes.len() > aligned_size {
            return Err(VdsoPatchError::ReplacementTooLarge {
                name,
                available: aligned_size,
                required: bytes.len(),
            });
        }
        patches.push(VdsoPatch {
            name,
            offset,
            size: aligned_size,
            bytes,
        });
    }

    Ok(patches)
}

fn replacement_image(size: usize, bytes: &[u8]) -> Vec<u8> {
    let mut image = vec![0x90; size];
    image[..bytes.len()].copy_from_slice(bytes);
    image
}

fn verify_patch_bytes(
    name: &'static str,
    expected: &[u8],
    observed: &[u8],
) -> Result<(), VdsoPatchError> {
    if expected == observed {
        Ok(())
    } else {
        Err(VdsoPatchError::VerificationMismatch {
            name,
            expected: expected.to_vec(),
            observed: observed.to_vec(),
        })
    }
}

static VDSO_SYMBOL_INFO: LazyLock<Result<VdsoSymbolInfo, VdsoPatchError>> =
    LazyLock::new(vdso_get_symbols_info);

pub(crate) fn is_patch_required(subscriptions: &Subscription) -> bool {
    subscriptions.iter_syscalls().any(|subscribed| {
        VDSO_SYMBOLS
            .iter()
            .any(|(_, syscall, _)| *syscall == subscribed)
    })
}

/// One vDSO entry point rewritten for an in-guest syscall hook.
#[derive(Clone, Copy, Debug)]
pub struct VdsoSyscallSite {
    /// Address at which the in-guest backend should install its hook.
    pub address: u64,
    /// Linux syscall number implemented by this entry point.
    pub number: i64,
    /// Start of the special vDSO mapping containing this entry point.
    pub mapping_start: u64,
    /// Length of the special vDSO mapping containing this entry point.
    pub mapping_len: u64,
}

/// Rewrite the calling process's vDSO entry points into hookable syscalls.
///
/// Returns each rewritten symbol's entry address and syscall number so an
/// in-guest patching backend can install its ordinary syscall trampoline while
/// the process is still single-threaded. The two-byte syscall is deliberately
/// placed at the aligned symbol entry: LiteInst needs a full patch word after
/// the hook address, which is not guaranteed at the tail of an eight-byte
/// pseudo-vDSO function. This shares the authoritative symbol table with
/// ptrace's stopped-guest path instead of maintaining a backend-specific list.
#[cfg(target_arch = "x86_64")]
pub fn patch_current_vdso(subscriptions: &Subscription) -> Result<Vec<VdsoSyscallSite>, Error> {
    if !is_patch_required(subscriptions) {
        return Ok(Vec::new());
    }
    let process =
        procfs::process::Process::new(unistd::getpid().as_raw()).map_err(|_| Errno::ENOENT)?;
    let maps = process.maps().map_err(|_| Errno::EIO)?;
    let Some(vdso) = maps
        .iter()
        .find(|entry| entry.pathname == procfs::process::MMapPath::Vdso)
    else {
        return Err(Errno::ENOENT.into());
    };
    let start = vdso.address.0 as usize;
    let len = (vdso.address.1 - vdso.address.0) as usize;
    Errno::result(unsafe {
        libc::mprotect(
            start as *mut libc::c_void,
            len,
            libc::PROT_READ | libc::PROT_WRITE | libc::PROT_EXEC,
        )
    })?;

    // Symbol discovery is now fail-closed: `VDSO_SYMBOL_INFO` is a `Result`,
    // where the pre-#410 `vdso_get_symbols_info` returned an empty map on
    // failure and left this loop silently patching nothing. Propagate instead.
    let symbol_info = VDSO_SYMBOL_INFO.as_ref().map_err(|_| Errno::ENOENT)?;

    let mut syscall_sites = Vec::new();
    // Iterating the static `VDSO_SYMBOLS` table, not the subscription-filtered
    // `build_vdso_patch_info`, deliberately preserves this function's pre-#410
    // behaviour: the in-guest LiteInst path rewrites every entry point present
    // in the vDSO and filters by the `match` below, and the `continue` on an
    // absent symbol is that path's existing contract. Extending #410's
    // fail-closed required-symbol policy here would be a behaviour change its
    // author never reviewed, so it is left as a follow-up.
    for (name, _syscall, _bytes) in VDSO_SYMBOLS {
        let Some(&(offset, unaligned_size)) = symbol_info.get(name) else {
            continue;
        };
        // Same 16-byte entry alignment the pre-#410 `VDSO_PATCH_INFO` applied.
        let size = align_up(unaligned_size, 16);
        let symbol = start + offset as usize;
        let number = match *name {
            "__vdso_time" => libc::SYS_time,
            "__vdso_clock_gettime" => libc::SYS_clock_gettime,
            "__vdso_getcpu" => libc::SYS_getcpu,
            "__vdso_gettimeofday" => libc::SYS_gettimeofday,
            "__vdso_clock_getres" => libc::SYS_clock_getres,
            _ => continue,
        };
        assert!(size >= 3);
        unsafe {
            core::ptr::write(symbol as *mut u8, 0x0f);
            core::ptr::write((symbol + 1) as *mut u8, 0x05);
            core::ptr::write((symbol + 2) as *mut u8, 0xc3);
            core::ptr::write_bytes((symbol + 3) as *mut u8, 0x90, size - 3);
        }
        syscall_sites.push(VdsoSyscallSite {
            address: symbol as u64,
            number,
            mapping_start: start as u64,
            mapping_len: len as u64,
        });
    }

    Errno::result(unsafe {
        libc::mprotect(
            start as *mut libc::c_void,
            len,
            libc::PROT_READ | libc::PROT_EXEC,
        )
    })?;
    Ok(syscall_sites)
}

// get vdso symbols offset/size from current process
// assuming vdso binary is the same for all processes
// so that we don't have to decode vdso for each process
fn vdso_get_symbols_info() -> Result<VdsoSymbolInfo, VdsoPatchError> {
    let pid = unistd::getpid().as_raw();
    let target = format!("patch metadata source process {pid}");
    let process = procfs::process::Process::new(pid).map_err(|error| VdsoPatchError::Inspect {
        target: target.clone(),
        detail: error.to_string(),
    })?;
    let maps = process.maps().map_err(|error| VdsoPatchError::Inspect {
        target: target.clone(),
        detail: error.to_string(),
    })?;
    let vdso = maps
        .iter()
        .find(|entry| entry.pathname == procfs::process::MMapPath::Vdso)
        .ok_or_else(|| VdsoPatchError::MissingVdsoMapping {
            target: target.clone(),
        })?;
    let slice = unsafe {
        std::slice::from_raw_parts(
            vdso.address.0 as *mut u8,
            (vdso.address.1 - vdso.address.0) as usize,
        )
    };
    let elf = Elf::parse(slice).map_err(|error| VdsoPatchError::Inspect {
        target,
        detail: error.to_string(),
    })?;

    let mut res = HashMap::new();
    for sym in elf.dynsyms.iter() {
        let Some(sym_name) = elf.dynstrtab.get_at(sym.st_name) else {
            continue;
        };
        if let Some((name, _, _)) = VDSO_SYMBOLS.iter().find(|(name, _, _)| name == &sym_name) {
            // __kernel_rt_sigreturn on ARM64 unfortunately is not marked as a
            // function in the vDSO, but as STT_NONE.
            if !(sym.is_function() || name == &"__kernel_rt_sigreturn") {
                return Err(VdsoPatchError::InvalidSymbolType { name });
            }
            res.insert(*name, (sym.st_value, sym.st_size as usize));
        }
    }
    Ok(res)
}

fn vdso_mapping(pid: i32) -> Result<(u64, u64), VdsoPatchError> {
    let target = format!("tracee {pid}");
    let process = procfs::process::Process::new(pid).map_err(|error| VdsoPatchError::Inspect {
        target: target.clone(),
        detail: error.to_string(),
    })?;
    let maps = process.maps().map_err(|error| VdsoPatchError::Inspect {
        target: target.clone(),
        detail: error.to_string(),
    })?;
    maps.iter()
        .find(|entry| entry.pathname == procfs::process::MMapPath::Vdso)
        .map(|entry| entry.address)
        .ok_or(VdsoPatchError::MissingVdsoMapping { target })
}

/// patch VDSOs when enabled
///
/// `guest` must be in one of ptrace's stopped states.
pub async fn vdso_patch<G, T>(
    guest: &mut G,
    subscriptions: &Subscription,
) -> Result<VdsoPatchReceipt, VdsoPatchError>
where
    G: Guest<T>,
    T: Tool,
{
    let pid = guest.pid().as_raw();
    let symbol_info = VDSO_SYMBOL_INFO.as_ref().map_err(Clone::clone)?;
    let patch_info = build_vdso_patch_info(symbol_info, subscriptions)?;
    let (vdso_start, vdso_end) = vdso_mapping(pid)?;
    let vdso_len = (vdso_end - vdso_start) as usize;
    let vdso_addr =
        AddrMut::from_raw(vdso_start as usize).ok_or(VdsoPatchError::InvalidPatchAddress {
            name: "[vdso]",
            address: vdso_start,
        })?;
    let mut memory = guest.memory();

    // Allow write access to the vDSO memory page.
    guest
        .inject_with_retry(
            Mprotect::new()
                .with_addr(Some(vdso_addr))
                .with_len(vdso_len)
                .with_protection(
                    ProtFlags::PROT_READ | ProtFlags::PROT_WRITE | ProtFlags::PROT_EXEC,
                ),
        )
        .await
        .map_err(|source| VdsoPatchError::Unprotect { pid, source })?;

    // Do not let any write failure leave the vDSO writable. Capture the write
    // result, restore RX protection unconditionally, and only then propagate.
    let write_result = (|| {
        let mut applied = Vec::with_capacity(patch_info.len());
        for patch in &patch_info {
            let start = vdso_start + patch.offset;
            let rptr =
                AddrMut::from_raw(start as usize).ok_or(VdsoPatchError::InvalidPatchAddress {
                    name: patch.name,
                    address: start,
                })?;
            let expected = replacement_image(patch.size, patch.bytes);
            memory
                .write_exact(rptr, &expected)
                .map_err(|source| VdsoPatchError::Write {
                    pid,
                    name: patch.name,
                    source,
                })?;
            applied.push((patch.name, start, expected));
        }
        Ok::<_, VdsoPatchError>(applied)
    })();

    let vdso_addr =
        AddrMut::from_raw(vdso_start as usize).ok_or(VdsoPatchError::InvalidPatchAddress {
            name: "[vdso]",
            address: vdso_start,
        })?;
    guest
        .inject_with_retry(
            Mprotect::new()
                .with_addr(Some(vdso_addr))
                .with_len(vdso_len)
                .with_protection(ProtFlags::PROT_READ | ProtFlags::PROT_EXEC),
        )
        .await
        .map_err(|source| VdsoPatchError::Reprotect { pid, source })?;

    let applied = write_result?;
    let mut receipt = VdsoPatchReceipt {
        required_symbols: patch_info.len(),
        patched_symbols: Vec::with_capacity(patch_info.len()),
        verified_bytes: 0,
    };
    for (name, start, expected) in applied {
        let rptr = Addr::from_raw(start as usize).ok_or(VdsoPatchError::InvalidPatchAddress {
            name,
            address: start,
        })?;
        let mut observed = vec![0; expected.len()];
        memory
            .read_exact(rptr, &mut observed)
            .map_err(|source| VdsoPatchError::ReadBack { pid, name, source })?;
        verify_patch_bytes(name, &expected, &observed)?;
        receipt.patched_symbols.push(name);
        receipt.verified_bytes += observed.len();
        debug!("{} patched and verified {}@{:x}", guest.pid(), name, start);
    }

    debug!(pid, ?receipt, "vDSO patch integrity verified");
    Ok(receipt)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, Default, Clone)]
    struct GetrandomTool;

    #[reverie::tool]
    impl Tool for GetrandomTool {
        type GlobalState = ();
        type ThreadState = ();

        fn subscriptions(_config: &()) -> Subscription {
            [Sysno::getrandom].into_iter().collect()
        }

        async fn handle_syscall_event<G: Guest<Self>>(
            &self,
            guest: &mut G,
            syscall: reverie::syscalls::Syscall,
        ) -> Result<i64, reverie::Error> {
            guest.tail_inject(syscall).await
        }
    }

    #[test]
    fn test_align_up() {
        assert_eq!(align_up(0, 16), 0);
        assert_eq!(align_up(1, 16), 16);
        assert_eq!(align_up(15, 16), 16);
        assert_eq!(align_up(16, 16), 16);
        assert_eq!(align_up(17, 16), 32);
    }

    #[test]
    fn can_find_vdso() {
        assert!(
            procfs::process::Process::new(unistd::getpid().as_raw())
                .map_or_else(
                    |_| Vec::new(),
                    |p| match p.maps() {
                        Ok(maps) => maps.0,
                        Err(_) => Vec::new(),
                    },
                )
                .iter()
                .any(|e| e.pathname == procfs::process::MMapPath::Vdso)
        );
    }

    #[test]
    fn vdso_can_find_symbols_info() {
        let info = vdso_get_symbols_info().expect("vDSO discovery must succeed");
        assert_eq!(info.len(), VDSO_SYMBOLS.len());
    }

    #[test]
    fn vdso_patch_info_is_valid() {
        let symbol_info = VDSO_SYMBOL_INFO
            .as_ref()
            .expect("vDSO symbol discovery must succeed");
        let info = build_vdso_patch_info(symbol_info, &Subscription::all())
            .expect("vDSO patch plan must be complete");
        info.iter().for_each(|i| println!("info: {:x?}", i));
        assert_eq!(info.len(), VDSO_SYMBOLS.len());
    }

    fn complete_symbol_info_fixture() -> VdsoSymbolInfo {
        VDSO_SYMBOLS
            .iter()
            .enumerate()
            .map(|(index, (name, _, bytes))| (*name, (((index + 1) * 0x100) as u64, bytes.len())))
            .collect()
    }

    #[test]
    fn partial_patch_plan_is_refused() {
        let mut info = complete_symbol_info_fixture();
        let missing = VDSO_SYMBOLS[0].0;
        info.remove(missing);

        let error = build_vdso_patch_info(&info, &Subscription::all()).unwrap_err();
        assert!(matches!(
            error,
            VdsoPatchError::MissingRequiredSymbol { name } if name == missing
        ));
    }

    #[test]
    fn exact_patch_bytes_succeed_and_corruption_is_refused() {
        let patch = build_vdso_patch_info(&complete_symbol_info_fixture(), &Subscription::all())
            .expect("complete patch plan must succeed")[0];
        let expected = replacement_image(patch.size, patch.bytes);

        verify_patch_bytes(patch.name, &expected, &expected)
            .expect("legitimate patch bytes must verify");

        let mut corrupted = expected.clone();
        corrupted[0] ^= 0xff;
        assert!(matches!(
            verify_patch_bytes(patch.name, &expected, &corrupted),
            Err(VdsoPatchError::VerificationMismatch { name, .. }) if name == patch.name
        ));
    }

    #[test]
    fn legitimate_vdso_patch_succeeds_in_tracee() {
        crate::testing::check_fn::<GetrandomTool, _>(|| {
            let mut bytes = [0u8; 8];
            let result = unsafe {
                libc::getrandom(bytes.as_mut_ptr().cast(), bytes.len(), libc::GRND_NONBLOCK)
            };
            assert_eq!(result, bytes.len() as isize);
        });
    }

    #[test]
    fn patch_requirement_tracks_vdso_syscall_subscriptions() {
        assert!(!is_patch_required(&Subscription::none()));
        assert!(!is_patch_required(&[Sysno::read].into_iter().collect()));
        assert!(is_patch_required(
            &[Sysno::clock_gettime].into_iter().collect()
        ));
        assert!(is_patch_required(&[Sysno::time].into_iter().collect()));
        assert!(is_patch_required(&[Sysno::getrandom].into_iter().collect()));
    }
}
