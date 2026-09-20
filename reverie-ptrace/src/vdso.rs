/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! Provides APIs to disable VDSOs at runtime.
use std::collections::BTreeMap;
use std::sync::LazyLock;

use goblin::elf::Elf;
use nix::sys::mman::ProtFlags;
use nix::unistd;
use reverie::Errno;
use reverie::Error;
use reverie::Guest;
use reverie::Subscription;
use reverie::Tool;
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

    // The five-argument vDSO ABI has an allocation query that is not a
    // getrandom syscall: (NULL, 0, 0, params, SIZE_MAX). Returning syscall
    // success for that query would leave libc's allocation parameters unset.
    // Reject only the query, without touching params. All ordinary calls,
    // including callers whose libc already owns vDSO state, use the syscall.
    // Keep its instruction aligned with a full hook word after it so LiteInst
    // can replace just that instruction without erasing the query predicate.
    pub const GETRANDOM_SYSCALL_OFFSET: usize = 48;
    const getrandom_code: BufferAligned<56> = BufferAligned::<56>([
        0x49, 0x83, 0xf8, 0xff, // cmp r8, -1
        0x75, 0x1a, // jne ordinary (32)
        0x48, 0x85, 0xff, // test rdi, rdi
        0x75, 0x15, // jne ordinary
        0x48, 0x85, 0xf6, // test rsi, rsi
        0x75, 0x10, // jne ordinary
        0x85, 0xd2, // test edx, edx (unsigned int flags)
        0x75, 0x0c, // jne ordinary
        0x48, 0xc7, 0xc0, 0xda, 0xff, 0xff, 0xff, // mov rax, -ENOSYS
        0xc3, // ret
        0x90, 0x90, 0x90, 0x90, // padding to ordinary
        0xb8, 0x3e, 0x01, 0x00, 0x00, // mov eax, SYS_getrandom (318)
        0x90, 0x90, 0x90, 0x90, 0x90, 0x90, 0x90, 0x90, 0x90, 0x90, 0x90, 0x0f,
        0x05, // syscall (offset 48)
        0xc3, // ret
        0x90, 0x90, 0x90, 0x90, 0x90, // remainder of the hook word
    ]);

    pub const getrandom: &[u8; 56] = &getrandom_code.0;
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
}

#[cfg(target_arch = "x86_64")]
const VDSO_SYMBOLS: &[(&str, &[u8], Sysno)] = &[
    ("__vdso_time", vdso_syms::time, Sysno::time),
    (
        "__vdso_clock_gettime",
        vdso_syms::clock_gettime,
        Sysno::clock_gettime,
    ),
    ("__vdso_getcpu", vdso_syms::getcpu, Sysno::getcpu),
    ("__vdso_getrandom", vdso_syms::getrandom, Sysno::getrandom),
    (
        "__vdso_gettimeofday",
        vdso_syms::gettimeofday,
        Sysno::gettimeofday,
    ),
    (
        "__vdso_clock_getres",
        vdso_syms::clock_getres,
        Sysno::clock_getres,
    ),
];

#[cfg(target_arch = "aarch64")]
const VDSO_SYMBOLS: &[(&str, &[u8], Sysno)] = &[
    (
        "__kernel_clock_getres",
        vdso_syms::clock_getres,
        Sysno::clock_getres,
    ),
    (
        "__kernel_clock_gettime",
        vdso_syms::clock_gettime,
        Sysno::clock_gettime,
    ),
    (
        "__kernel_gettimeofday",
        vdso_syms::gettimeofday,
        Sysno::gettimeofday,
    ),
    (
        "__kernel_rt_sigreturn",
        vdso_syms::rt_sigreturn,
        Sysno::rt_sigreturn,
    ),
];

/// Rounds up `value` so that it is a multiple of `alignment`.
fn align_up(value: usize, alignment: usize) -> usize {
    (value + alignment - 1) & alignment.wrapping_neg()
}

/// One vDSO symbol's patch coordinates and corresponding syscall.
type VdsoPatch = (u64, usize, &'static [u8], Sysno);

/// Per-symbol VDSO patch info: `symbol name -> patch`.
type VdsoPatchInfo = BTreeMap<&'static str, VdsoPatch>;

static VDSO_PATCH_INFO: LazyLock<VdsoPatchInfo> = LazyLock::new(|| {
    let info = vdso_get_symbols_info();
    let mut res = BTreeMap::new();

    for (k, v, sysno) in VDSO_SYMBOLS {
        if let Some(&(base, size)) = info.get(*k) {
            // NOTE: There is padding at the end of every VDSO entry to
            // bring it up to a 16-byte size alignment. The dynamic symbol
            // table doesn't report the aligned size, so we must do the same
            // alignment here. For example, some VDSO entries might only be
            // 5 bytes, but they have padding to align them up to 16 bytes.
            let aligned_size = align_up(size, 16);
            assert!(
                v.len() <= aligned_size,
                "vdso symbol {}'s real size is {} bytes, but trying to replace it with {} bytes",
                k,
                size,
                v.len()
            );
            res.insert(*k, (base, aligned_size, *v, *sysno));
        }
    }

    res
});

/// Is this individual vDSO entry point's syscall subscribed?
///
/// Patching is per-symbol because subscription is per-syscall. Rewriting an entry
/// point nobody subscribed to converts a pure userspace vDSO call into a real
/// syscall for no gain: the tool never sees it, and the guest pays the kernel
/// crossing anyway. That cost is not hypothetical -- QEMU's main loop calls
/// `clock_gettime` continuously *while holding the Big QEMU Lock*, so every
/// needless crossing is taken with a lock held that vCPU threads are waiting on.
fn is_symbol_patch_required(subscriptions: &Subscription, sysno: Sysno) -> bool {
    subscriptions
        .iter_syscalls()
        .any(|syscall| syscall == sysno)
}

/// The exact symbol patches selected by a syscall subscription.
///
/// Both the in-process and stopped-guest patch paths consume this iterator, so
/// their subscription selection is implemented in one place.
fn subscribed_vdso_patches(
    subscriptions: &Subscription,
) -> impl Iterator<Item = (&'static str, &'static VdsoPatch)> + '_ {
    VDSO_PATCH_INFO
        .iter()
        .filter(|(_, (_, _, _, sysno))| is_symbol_patch_required(subscriptions, *sysno))
        .map(|(name, patch)| (*name, patch))
}

pub fn is_patch_required(subscriptions: &Subscription) -> bool {
    subscribed_vdso_patches(subscriptions).next().is_some()
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
/// Returns each rewritten symbol's syscall address and number so an
/// in-guest patching backend can install its ordinary syscall trampoline while
/// the process is still single-threaded. LiteInst needs a full patch word after
/// the aligned hook address. Most entries place it at the symbol entry;
/// getrandom retains its allocation-query predicate and syscall-number load
/// before that address. This shares the authoritative symbol table with
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

    let mut syscall_sites = Vec::new();
    for (name, (offset, size, bytes, sysno)) in subscribed_vdso_patches(subscriptions) {
        let symbol = start + *offset as usize;
        let number = match name {
            "__vdso_time" => libc::SYS_time,
            "__vdso_clock_gettime" => libc::SYS_clock_gettime,
            "__vdso_getcpu" => libc::SYS_getcpu,
            "__vdso_getrandom" => libc::SYS_getrandom,
            "__vdso_gettimeofday" => libc::SYS_gettimeofday,
            "__vdso_clock_getres" => libc::SYS_clock_getres,
            _ => continue,
        };
        let (code, syscall_offset) = in_process_vdso_code(bytes, *sysno);
        assert!(*size >= code.len());
        unsafe {
            core::ptr::copy_nonoverlapping(code.as_ptr(), symbol as *mut u8, code.len());
            core::ptr::write_bytes((symbol + code.len()) as *mut u8, 0x90, size - code.len());
        }
        syscall_sites.push(VdsoSyscallSite {
            address: (symbol + syscall_offset) as u64,
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

#[cfg(target_arch = "x86_64")]
fn in_process_vdso_code(bytes: &'static [u8], sysno: Sysno) -> (&'static [u8], usize) {
    if sysno == Sysno::getrandom {
        // This must work before the hook is installed as well: tool setup can
        // call an already-initialized libc between patching and activation.
        (bytes, vdso_syms::GETRANDOM_SYSCALL_OFFSET)
    } else {
        (&[0x0f, 0x05, 0xc3], 0)
    }
}

// get vdso symbols offset/size from current process
// assuming vdso binary is the same for all processes
// so that we don't have to decode vdso for each process
fn vdso_get_symbols_info() -> BTreeMap<&'static str, (u64, usize)> {
    let mut res = BTreeMap::new();
    procfs::process::Process::new(unistd::getpid().as_raw())
        .map_or_else(
            |_| Vec::new(),
            |p| match p.maps() {
                Ok(maps) => maps.0,
                Err(_) => Vec::new(),
            },
        )
        .iter()
        .find(|e| e.pathname == procfs::process::MMapPath::Vdso)
        .and_then(|vdso| {
            let slice = unsafe {
                std::slice::from_raw_parts(
                    vdso.address.0 as *mut u8,
                    (vdso.address.1 - vdso.address.0) as usize,
                )
            };
            Elf::parse(slice)
                .map(|elf| {
                    let strtab = elf.dynstrtab;
                    elf.dynsyms.iter().for_each(|sym| {
                        let sym_name = &strtab[sym.st_name];
                        if let Some((name, _, _)) =
                            VDSO_SYMBOLS.iter().find(|&(name, _, _)| name == &sym_name)
                        {
                            // __kernel_rt_sigreturn on ARM64 unfortunately is
                            // not marked as a function in VDSO, but as
                            // STT_NONE.
                            debug_assert!(sym.is_function() || name == &"__kernel_rt_sigreturn");
                            res.insert(*name, (sym.st_value, sym.st_size as usize));
                        }
                    });
                })
                .ok()
        });
    res
}

/// patch VDSOs when enabled
///
/// `guest` must be in one of ptrace's stopped states.
pub async fn vdso_patch<G, T>(guest: &mut G, subscriptions: &Subscription) -> Result<(), Error>
where
    G: Guest<T>,
    T: Tool,
{
    if let Some(vdso) = procfs::process::Process::new(guest.pid().as_raw())
        .map_or_else(
            |_| Vec::new(),
            |p| match p.maps() {
                Ok(maps) => maps.0,
                Err(_) => Vec::new(),
            },
        )
        .iter()
        .find(|e| e.pathname == procfs::process::MMapPath::Vdso)
    {
        let mut memory = guest.memory();

        // Allow write access to the vdso memory page.
        guest
            .inject_with_retry(
                Mprotect::new()
                    .with_addr(AddrMut::from_raw(vdso.address.0 as usize))
                    .with_len((vdso.address.1 - vdso.address.0) as usize)
                    .with_protection(
                        ProtFlags::PROT_READ | ProtFlags::PROT_WRITE | ProtFlags::PROT_EXEC,
                    ),
            )
            .await?;

        for (name, (offset, size, bytes, _sysno)) in subscribed_vdso_patches(subscriptions) {
            let start = vdso.address.0 + offset;
            assert!(bytes.len() <= *size);
            let rptr = AddrMut::from_raw(start as usize).ok_or(Errno::EFAULT)?;
            memory.write_exact(rptr, bytes)?;
            assert!(*size >= bytes.len());
            if *size > bytes.len() {
                let fill: Vec<u8> = std::iter::repeat_n(0x90u8, size - bytes.len()).collect();
                memory.write_exact(unsafe { rptr.add(bytes.len()) }, &fill)?;
            }
            debug!("{} patched {}@{:x}", guest.pid(), name, start);
        }

        guest
            .inject_with_retry(
                Mprotect::new()
                    .with_addr(AddrMut::from_raw(vdso.address.0 as usize))
                    .with_len((vdso.address.1 - vdso.address.0) as usize)
                    .with_protection(ProtFlags::PROT_READ | ProtFlags::PROT_EXEC),
            )
            .await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_arch = "x86_64")]
    mod getrandom {
        use super::*;

        type Getrandom = unsafe extern "C" fn(
            *mut libc::c_void,
            usize,
            libc::c_uint,
            *mut libc::c_void,
            usize,
        ) -> isize;

        struct ExecutableThunk(*mut libc::c_void);

        impl ExecutableThunk {
            fn new(code: &[u8]) -> Self {
                assert!(code.len() <= 4096);
                let memory = unsafe {
                    libc::mmap(
                        std::ptr::null_mut(),
                        4096,
                        libc::PROT_READ | libc::PROT_WRITE,
                        libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
                        -1,
                        0,
                    )
                };
                assert_ne!(memory, libc::MAP_FAILED);
                unsafe {
                    std::ptr::copy_nonoverlapping(code.as_ptr(), memory.cast(), code.len());
                    assert_eq!(
                        libc::mprotect(memory, 4096, libc::PROT_READ | libc::PROT_EXEC),
                        0
                    );
                }
                Self(memory)
            }

            fn function(&self) -> Getrandom {
                unsafe { std::mem::transmute(self.0) }
            }
        }

        impl Drop for ExecutableThunk {
            fn drop(&mut self) {
                assert_eq!(unsafe { libc::munmap(self.0, 4096) }, 0);
            }
        }

        #[test]
        fn exact_query_leaves_output_and_canaries_untouched() {
            let thunk = ExecutableThunk::new(vdso_syms::getrandom);
            let mut guarded_params = [0xa5_u8; 80];
            let result = unsafe {
                thunk.function()(
                    std::ptr::null_mut(),
                    0,
                    0,
                    guarded_params.as_mut_ptr().add(8).cast(),
                    usize::MAX,
                )
            };
            assert_eq!(result, -(libc::ENOSYS as isize));
            assert_eq!(guarded_params, [0xa5; 80]);
        }

        #[test]
        fn ordinary_and_each_near_query_use_the_real_syscall() {
            let thunk = ExecutableThunk::new(vdso_syms::getrandom);
            let function = thunk.function();
            let mut state = [0x5a_u8; 144];
            let opaque = state.as_mut_ptr().cast();
            let mut guarded_buffer = [0xa5_u8; 32];
            let buffer = unsafe { guarded_buffer.as_mut_ptr().add(8).cast() };
            assert_eq!(unsafe { function(buffer, 16, 0, opaque, state.len()) }, 16);
            assert_eq!(&guarded_buffer[..8], &[0xa5; 8]);
            assert_eq!(&guarded_buffer[24..], &[0xa5; 8]);
            assert_eq!(state, [0x5a; 144]);

            // Change exactly one of the four query conditions at a time.
            assert_eq!(unsafe { function(buffer, 0, 0, opaque, usize::MAX) }, 0);
            assert_eq!(
                unsafe { function(std::ptr::null_mut(), 1, 0, opaque, usize::MAX) },
                -(libc::EFAULT as isize),
            );
            assert_eq!(
                unsafe {
                    function(
                        std::ptr::null_mut(),
                        0,
                        libc::GRND_NONBLOCK,
                        opaque,
                        usize::MAX,
                    )
                },
                0,
            );
            assert_eq!(
                unsafe { function(std::ptr::null_mut(), 0, 0, opaque, 0) },
                0
            );
            assert_eq!(
                unsafe { function(std::ptr::null_mut(), 0, u32::MAX, opaque, usize::MAX) },
                -(libc::EINVAL as isize),
            );
            assert_eq!(state, [0x5a; 144]);
        }

        #[test]
        fn in_process_hook_word_preserves_query_and_number_load() {
            let (code, offset) = in_process_vdso_code(vdso_syms::getrandom, Sysno::getrandom);
            assert_eq!(code, vdso_syms::getrandom);
            assert_eq!(offset % 8, 0);
            assert!(offset + 8 <= code.len());
            assert_eq!(&code[offset..offset + 3], &[0x0f, 0x05, 0xc3]);
            // Actual execution before hook installation proves the number load
            // is part of the retained prefix, not supplied only by a callback.
            let before = ExecutableThunk::new(code);
            let mut buffer = [0_u8; 16];
            assert_eq!(
                unsafe {
                    before.function()(buffer.as_mut_ptr().cast(), 16, 0, std::ptr::null_mut(), 0)
                },
                16
            );

            // Model replacement of exactly the patch word. The real LiteInst
            // callback is exercised separately by its process fixture.
            let mut replaced = code.to_vec();
            replaced[offset..offset + 8].copy_from_slice(&[0xb8, 37, 0, 0, 0, 0xc3, 0x90, 0x90]);
            assert_eq!(&replaced[..offset], &code[..offset]);
            let after = ExecutableThunk::new(&replaced);
            let mut params = [0xa5_u8; 80];
            assert_eq!(
                unsafe {
                    after.function()(
                        std::ptr::null_mut(),
                        0,
                        0,
                        params.as_mut_ptr().add(8).cast(),
                        usize::MAX,
                    )
                },
                -(libc::ENOSYS as isize)
            );
            assert_eq!(params, [0xa5; 80]);
            assert_eq!(
                unsafe {
                    after.function()(buffer.as_mut_ptr().cast(), 16, 0, std::ptr::null_mut(), 144)
                },
                37
            );
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
        assert!(!vdso_get_symbols_info().is_empty());
    }

    #[test]
    fn vdso_patch_info_is_valid() {
        let info = &VDSO_PATCH_INFO;
        info.iter().for_each(|i| println!("info: {:x?}", i));
        assert!(!info.is_empty());
    }

    #[test]
    fn patch_requirement_tracks_vdso_syscall_subscriptions() {
        assert!(!is_patch_required(&Subscription::none()));
        assert!(!is_patch_required(&[Sysno::read].into_iter().collect()));

        for (_, _, _, sysno) in VDSO_PATCH_INFO.values() {
            assert!(
                is_patch_required(&[*sysno].into_iter().collect()),
                "a subscription for vDSO syscall {sysno:?} must require patching",
            );
        }

        let time_is_present = VDSO_PATCH_INFO
            .values()
            .any(|(_, _, _, sysno)| *sysno == Sysno::time);
        assert_eq!(
            is_patch_required(&[Sysno::time].into_iter().collect()),
            time_is_present,
            "time is patchable only on architectures whose vDSO table contains it",
        );
    }

    #[test]
    fn each_subscription_selects_only_its_own_vdso_symbols() {
        for (_, _, _, subscribed_sysno) in VDSO_PATCH_INFO.values() {
            let subscriptions = [*subscribed_sysno].into_iter().collect();
            let selected = subscribed_vdso_patches(&subscriptions).collect::<Vec<_>>();

            assert!(
                !selected.is_empty(),
                "{subscribed_sysno:?} selected nothing"
            );
            for (name, (_, _, _, selected_sysno)) in selected {
                assert_eq!(
                    *selected_sysno, *subscribed_sysno,
                    "subscription for {subscribed_sysno:?} also selected {name} ({selected_sysno:?})",
                );
            }
        }
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn getcpu_only_leaves_other_vdso_symbols_unselected() {
        let subscriptions = [Sysno::getcpu].into_iter().collect();
        let selected = subscribed_vdso_patches(&subscriptions)
            .map(|(name, _)| name)
            .collect::<Vec<_>>();

        assert_eq!(selected, vec!["__vdso_getcpu"]);
    }
}
