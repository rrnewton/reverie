/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! Provides APIs to disable VDSOs at runtime.
use std::collections::BTreeMap;
#[cfg(target_arch = "x86_64")]
use std::collections::BTreeSet;
use std::sync::LazyLock;

use goblin::elf::Elf;
#[cfg(target_arch = "x86_64")]
use iced_x86::Decoder;
#[cfg(target_arch = "x86_64")]
use iced_x86::DecoderOptions;
#[cfg(target_arch = "x86_64")]
use iced_x86::FlowControl;
#[cfg(target_arch = "x86_64")]
use iced_x86::OpKind;
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

#[cfg(target_arch = "x86_64")]
#[path = "vdso/getrandom_x86_64.rs"]
mod vdso_getrandom_x86_64;

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

#[cfg(target_arch = "x86_64")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct StoppedVdsoWordPatch {
    pub(crate) address: u64,
    pub(crate) expected: u64,
    pub(crate) replacement: u64,
}

#[cfg(target_arch = "x86_64")]
#[derive(Clone, Debug, Eq, PartialEq)]
struct StoppedVdsoGetrandomPlan {
    mapping_start: u64,
    expected_image: Box<[u8]>,
    normal_address: u64,
    normal_expected: [u8; 5],
    normal_replacement: [u8; 5],
}

#[cfg(target_arch = "x86_64")]
#[derive(Clone, Debug, Eq, PartialEq)]
struct StoppedVdsoLegacyPatch {
    name: &'static str,
    offset: usize,
    replacement: Box<[u8]>,
}

/// Complete stopped-target vDSO publication planned from the target's exact
/// original bytes.
///
/// The query-preserving getrandom redirect is published first as one aligned
/// word. Ordinary vDSO syscall thunks follow in symbol-table order. Every word
/// retains neighboring bytes from the same complete image, so completed writes
/// can be restored in reverse order.
#[cfg(target_arch = "x86_64")]
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct StoppedVdsoPlan {
    mapping_start: u64,
    expected_image: Box<[u8]>,
    getrandom: Option<StoppedVdsoGetrandomPlan>,
    legacy: Vec<StoppedVdsoLegacyPatch>,
}

#[cfg(target_arch = "x86_64")]
impl StoppedVdsoGetrandomPlan {
    fn expected_published_image(&self) -> Result<Box<[u8]>, Error> {
        let mut image = self.expected_image.clone();
        let normal = usize::try_from(
            self.normal_address
                .checked_sub(self.mapping_start)
                .ok_or(Errno::EFAULT)?,
        )
        .map_err(|_| Errno::EOVERFLOW)?;
        let end = normal
            .checked_add(self.normal_replacement.len())
            .ok_or(Errno::EOVERFLOW)?;
        if image.get(normal..end) != Some(self.normal_expected.as_slice()) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "stopped getrandom normal branch changed after planning",
            )
            .into());
        }
        image[normal..end].copy_from_slice(&self.normal_replacement);
        Ok(image)
    }

    fn publication_word(&self) -> Result<StoppedVdsoWordPatch, Error> {
        let published = self.expected_published_image()?;
        let address = self.normal_address & !7;
        let offset = usize::try_from(
            address
                .checked_sub(self.mapping_start)
                .ok_or(Errno::EFAULT)?,
        )
        .map_err(|_| Errno::EOVERFLOW)?;
        let end = offset.checked_add(8).ok_or(Errno::EOVERFLOW)?;
        let original: [u8; 8] = self
            .expected_image
            .get(offset..end)
            .ok_or(Errno::EFAULT)?
            .try_into()
            .map_err(|_| Errno::EFAULT)?;
        let replacement: [u8; 8] = published
            .get(offset..end)
            .ok_or(Errno::EFAULT)?
            .try_into()
            .map_err(|_| Errno::EFAULT)?;
        if original == replacement {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "stopped getrandom publication word is unchanged",
            )
            .into());
        }
        Ok(StoppedVdsoWordPatch {
            address,
            expected: u64::from_le_bytes(original),
            replacement: u64::from_le_bytes(replacement),
        })
    }
}

#[cfg(target_arch = "x86_64")]
impl StoppedVdsoPlan {
    pub(crate) fn mapping_start(&self) -> u64 {
        self.mapping_start
    }

    pub(crate) fn mapping_len(&self) -> u64 {
        self.expected_image.len() as u64
    }

    pub(crate) fn expected_image(&self) -> &[u8] {
        &self.expected_image
    }

    pub(crate) fn has_getrandom(&self) -> bool {
        self.getrandom.is_some()
    }

    pub(crate) fn legacy_patch_count(&self) -> usize {
        self.legacy.len()
    }

    pub(crate) fn expected_published_image(&self) -> Result<Box<[u8]>, Error> {
        let mut image = match &self.getrandom {
            Some(plan) => plan.expected_published_image()?,
            None => self.expected_image.clone(),
        };
        for patch in &self.legacy {
            let end = patch
                .offset
                .checked_add(patch.replacement.len())
                .ok_or(Errno::EOVERFLOW)?;
            image
                .get_mut(patch.offset..end)
                .ok_or(Errno::EFAULT)?
                .copy_from_slice(&patch.replacement);
        }
        Ok(image)
    }

    /// Return exact aligned publication words in getrandom-then-legacy order.
    pub(crate) fn publication_words(&self) -> Result<Vec<StoppedVdsoWordPatch>, Error> {
        let published = self.expected_published_image()?;
        let mut words = Vec::new();
        let mut addresses = BTreeSet::new();
        if let Some(plan) = &self.getrandom {
            let word = plan.publication_word()?;
            addresses.insert(word.address);
            words.push(word);
        }
        for patch in &self.legacy {
            let address = self
                .mapping_start
                .checked_add(patch.offset as u64)
                .ok_or(Errno::EOVERFLOW)?;
            let last_address = address
                .checked_add(patch.replacement.len() as u64 - 1)
                .ok_or(Errno::EOVERFLOW)?;
            let mut word_address = address & !7;
            let last_word = last_address & !7;
            loop {
                if !addresses.insert(word_address) {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        format!(
                            "stopped vDSO patch for {} overlaps another aligned word",
                            patch.name
                        ),
                    )
                    .into());
                }
                let offset = usize::try_from(
                    word_address
                        .checked_sub(self.mapping_start)
                        .ok_or(Errno::EFAULT)?,
                )
                .map_err(|_| Errno::EOVERFLOW)?;
                let end = offset.checked_add(8).ok_or(Errno::EOVERFLOW)?;
                let original: [u8; 8] = self
                    .expected_image
                    .get(offset..end)
                    .ok_or(Errno::EFAULT)?
                    .try_into()
                    .map_err(|_| Errno::EFAULT)?;
                let replacement: [u8; 8] = published
                    .get(offset..end)
                    .ok_or(Errno::EFAULT)?
                    .try_into()
                    .map_err(|_| Errno::EFAULT)?;
                if original != replacement {
                    words.push(StoppedVdsoWordPatch {
                        address: word_address,
                        expected: u64::from_le_bytes(original),
                        replacement: u64::from_le_bytes(replacement),
                    });
                }
                if word_address == last_word {
                    break;
                }
                word_address = word_address.checked_add(8).ok_or(Errno::EOVERFLOW)?;
            }
        }
        Ok(words)
    }
}

#[cfg(target_arch = "x86_64")]
fn getrandom_rewritten_instruction_interior(
    symbol_value: u64,
    plan: &vdso_getrandom_x86_64::PatchPlan,
    guard_address: u64,
    target: u64,
) -> Result<bool, Error> {
    let normal = symbol_value
        .checked_add(plan.normal_branch_offset as u64)
        .ok_or(Errno::EOVERFLOW)?;
    let fallback = symbol_value
        .checked_add(plan.fallback_branch_offset as u64)
        .ok_or(Errno::EOVERFLOW)?;
    for (start, len) in [
        (normal, plan.normal_branch_expected.len()),
        (fallback, plan.fallback_branch_expected.len()),
        (
            guard_address,
            vdso_getrandom_x86_64::SGX_GUARD_DISPLACED_LEN,
        ),
    ] {
        let end = start.checked_add(len as u64).ok_or(Errno::EOVERFLOW)?;
        if start < target && target < end {
            return Ok(true);
        }
    }
    Ok(false)
}

#[cfg(target_arch = "x86_64")]
fn reject_getrandom_direct_interior_entries(
    elf: &Elf<'_>,
    image: &[u8],
    load_end: u64,
    symbol_value: u64,
    symbol_size: u64,
    plan: &vdso_getrandom_x86_64::PatchPlan,
    guard_address: u64,
) -> Result<(), Error> {
    let symbol_end = symbol_value
        .checked_add(symbol_size)
        .ok_or(Errno::EOVERFLOW)?;
    let mut function_is_executable = false;
    let mut canonical_heads = BTreeSet::new();
    let mut executable_ranges = Vec::new();
    let mut direct_targets = Vec::new();
    let mut guard_is_executable = false;
    let mut guarded_indirect_calls = 0_usize;
    let guard_source_end = guard_address
        .checked_add(vdso_getrandom_x86_64::SGX_GUARD_SOURCE.len() as u64)
        .ok_or(Errno::EOVERFLOW)?;
    let guard_start = usize::try_from(guard_address).map_err(|_| Errno::EOVERFLOW)?;
    let guard_end = usize::try_from(guard_source_end).map_err(|_| Errno::EOVERFLOW)?;
    if guard_source_end > load_end
        || image.get(guard_start..guard_end)
            != Some(vdso_getrandom_x86_64::SGX_GUARD_SOURCE.as_slice())
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "current vDSO SGX callback producer or indirect call changed",
        )
        .into());
    }
    for section in &elf.section_headers {
        if section.sh_flags & u64::from(goblin::elf::section_header::SHF_EXECINSTR) == 0
            || section.sh_size == 0
        {
            continue;
        }
        let section_end = section
            .sh_addr
            .checked_add(section.sh_size)
            .ok_or(Errno::EOVERFLOW)?;
        let file_end = section
            .sh_offset
            .checked_add(section.sh_size)
            .ok_or(Errno::EOVERFLOW)?;
        if section.sh_addr != section.sh_offset
            || section_end > load_end
            || file_end > image.len() as u64
        {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "current vDSO has unsupported executable-section geometry",
            )
            .into());
        }
        if executable_ranges
            .iter()
            .any(|(start, end)| section.sh_addr < *end && *start < section_end)
        {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "current vDSO has overlapping executable sections",
            )
            .into());
        }
        function_is_executable |= section.sh_addr <= symbol_value && symbol_end <= section_end;
        guard_is_executable |= section.sh_addr <= guard_address && guard_source_end <= section_end;
        executable_ranges.push((section.sh_addr, section_end));
        let start = usize::try_from(section.sh_offset).map_err(|_| Errno::EOVERFLOW)?;
        let end = usize::try_from(file_end).map_err(|_| Errno::EOVERFLOW)?;
        let mut decoder = Decoder::with_ip(
            64,
            &image[start..end],
            section.sh_addr,
            DecoderOptions::NONE,
        );
        while decoder.can_decode() {
            let instruction = decoder.decode();
            if instruction.is_invalid() {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "current vDSO executable section does not decode completely",
                )
                .into());
            }
            canonical_heads.insert(instruction.ip());
            if matches!(
                instruction.flow_control(),
                FlowControl::IndirectCall | FlowControl::IndirectBranch
            ) {
                let expected_call = guard_address
                    .checked_add(vdso_getrandom_x86_64::SGX_GUARD_DISPLACED_LEN as u64)
                    .ok_or(Errno::EOVERFLOW)?;
                if instruction.flow_control() != FlowControl::IndirectCall
                    || instruction.ip() != expected_call
                    || instruction.len() != 2
                {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        "current vDSO contains an unsupported indirect call or jump",
                    )
                    .into());
                }
                guarded_indirect_calls += 1;
            }
            let target = match instruction.op0_kind() {
                OpKind::NearBranch16 | OpKind::NearBranch32 | OpKind::NearBranch64 => {
                    Some(instruction.near_branch_target())
                }
                OpKind::FarBranch16 | OpKind::FarBranch32 => {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        "current vDSO contains an unsupported direct far branch",
                    )
                    .into());
                }
                _ => None,
            };
            if let Some(target) = target
                && getrandom_rewritten_instruction_interior(
                    symbol_value,
                    plan,
                    guard_address,
                    target,
                )?
            {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "current vDSO directly enters a rewritten instruction interior",
                )
                .into());
            }
            direct_targets.extend(target);
        }
    }
    if !function_is_executable || !guard_is_executable {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "current vDSO getrandom function or SGX guard is not inside one executable section",
        )
        .into());
    }
    if guarded_indirect_calls != 1 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "current vDSO does not contain exactly one guarded indirect call",
        )
        .into());
    }
    for symbol in elf.dynsyms.iter().chain(elf.syms.iter()) {
        if symbol.st_shndx != goblin::elf::section_header::SHN_UNDEF as usize
            && symbol.st_type() == goblin::elf::sym::STT_GNU_IFUNC
        {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "current vDSO contains a defined GNU IFUNC",
            )
            .into());
        }
        let callable_type = matches!(
            symbol.st_type(),
            goblin::elf::sym::STT_FUNC | goblin::elf::sym::STT_NOTYPE
        );
        let exported_binding = matches!(
            symbol.st_bind(),
            goblin::elf::sym::STB_GLOBAL | goblin::elf::sym::STB_WEAK
        );
        let exported_visibility = matches!(
            symbol.st_visibility(),
            goblin::elf::sym::STV_DEFAULT | goblin::elf::sym::STV_PROTECTED
        );
        if symbol.st_shndx == goblin::elf::section_header::SHN_UNDEF as usize
            || !callable_type
            || !exported_binding
            || !exported_visibility
        {
            continue;
        }
        if getrandom_rewritten_instruction_interior(
            symbol_value,
            plan,
            guard_address,
            symbol.st_value,
        )? {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "current vDSO exports an entry inside a rewritten instruction",
            )
            .into());
        }
        // The admitted PT_LOAD starts at virtual address zero. A callable
        // export anywhere in that executable mapping must therefore be a head
        // from exactly one nonoverlapping executable-section decode; a symbol
        // in an RX gap cannot introduce an uninspected stream.
        if symbol.st_value < load_end && !canonical_heads.contains(&symbol.st_value) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "current vDSO exported callable symbol is not a canonical decoded instruction head",
            )
            .into());
        }
    }
    for target in direct_targets {
        if target < load_end && !canonical_heads.contains(&target) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "current vDSO direct branch target is not a canonical decoded instruction head",
            )
            .into());
        }
    }
    Ok(())
}

#[cfg(target_arch = "x86_64")]
fn liteinst_getrandom_symbol_layout(
    image: &[u8],
    guard_address: u64,
) -> Result<Option<(usize, vdso_getrandom_x86_64::PatchPlan)>, Error> {
    let elf = Elf::parse(image).map_err(|error| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("could not parse current vDSO for LiteInst getrandom: {error}"),
        )
    })?;
    if !elf.is_64
        || !elf.little_endian
        || elf.header.e_type != goblin::elf::header::ET_DYN
        || elf.header.e_machine != goblin::elf::header::EM_X86_64
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "current vDSO is not a little-endian x86-64 shared object",
        )
        .into());
    }

    let mut found = None;
    let mut saw_canonical = false;
    let mut saw_alias = false;
    for symbol in elf.dynsyms.iter() {
        let Some(name) = elf.dynstrtab.get_at(symbol.st_name) else {
            continue;
        };
        let (seen, expected_binding) = match name {
            "__vdso_getrandom" => (&mut saw_canonical, goblin::elf::sym::STB_GLOBAL),
            "getrandom" => (&mut saw_alias, goblin::elf::sym::STB_WEAK),
            _ => continue,
        };
        if *seen {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("current vDSO contains duplicate {name} symbols"),
            )
            .into());
        }
        *seen = true;
        if symbol.st_shndx == goblin::elf::section_header::SHN_UNDEF as usize
            || !symbol.is_function()
            || symbol.st_bind() != expected_binding
            || symbol.st_other & 0x03 != goblin::elf::sym::STV_DEFAULT
        {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("current vDSO {name} is not a defined exported function"),
            )
            .into());
        }
        let route = (symbol.st_value, symbol.st_size);
        if let Some(previous) = found {
            if previous != route {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "current vDSO getrandom aliases do not share one exact entry and size",
                )
                .into());
            }
        } else {
            found = Some(route);
        }
    }
    let Some((value, size)) = found else {
        vdso_getrandom_x86_64::plan(None)?;
        return Ok(None);
    };

    let mut executable_loads = elf.program_headers.iter().filter(|header| {
        header.p_type == goblin::elf::program_header::PT_LOAD
            && header.p_flags & goblin::elf::program_header::PF_X != 0
    });
    let Some(load) = executable_loads.next() else {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "current vDSO getrandom has no executable load segment",
        )
        .into());
    };
    let load_size = usize::try_from(load.p_filesz).map_err(|_| Errno::EOVERFLOW)?;
    if executable_loads.next().is_some()
        || load.p_offset != 0
        || load.p_vaddr != 0
        || load.p_filesz != load.p_memsz
        || load_size > image.len()
        || load.p_align != 0x1000
        || load.p_flags != (goblin::elf::program_header::PF_R | goblin::elf::program_header::PF_X)
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "current vDSO getrandom has unsupported executable load geometry",
        )
        .into());
    }
    let load_end = load
        .p_vaddr
        .checked_add(load.p_filesz)
        .ok_or(Errno::EOVERFLOW)?;
    let symbol_end = value.checked_add(size).ok_or(Errno::EOVERFLOW)?;
    if value < load.p_vaddr || symbol_end > load_end {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "current vDSO getrandom route lies outside its executable load segment",
        )
        .into());
    }
    let start =
        usize::try_from(value - load.p_vaddr + load.p_offset).map_err(|_| Errno::EOVERFLOW)?;
    let size = usize::try_from(size).map_err(|_| Errno::EOVERFLOW)?;
    let end = start.checked_add(size).ok_or(Errno::EOVERFLOW)?;
    let bytes = image.get(start..end).ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "current vDSO getrandom route lies outside its mapped image",
        )
    })?;
    let Some(plan) = vdso_getrandom_x86_64::plan(Some(bytes))? else {
        unreachable!("a present symbol always produces a plan");
    };
    let syscall_value = value
        .checked_add(plan.syscall_offset as u64)
        .ok_or(Errno::EOVERFLOW)?;
    let syscall_word_end = syscall_value
        .checked_add(plan.syscall_word.len() as u64)
        .ok_or(Errno::EOVERFLOW)?;
    if syscall_word_end > load_end {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "current vDSO getrandom raw fallback crosses its executable load segment",
        )
        .into());
    }
    let syscall_file_offset = start
        .checked_add(plan.syscall_offset)
        .ok_or(Errno::EOVERFLOW)?;
    let observed_syscall_word = image
        .get(
            syscall_file_offset
                ..syscall_file_offset
                    .checked_add(plan.syscall_word.len())
                    .ok_or(Errno::EOVERFLOW)?,
        )
        .ok_or(Errno::EFAULT)?;
    if observed_syscall_word != plan.syscall_word {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "current vDSO getrandom raw fallback syscall word changed",
        )
        .into());
    }
    for symbol in elf.dynsyms.iter().chain(elf.syms.iter()) {
        if symbol.st_shndx != goblin::elf::section_header::SHN_UNDEF as usize
            && getrandom_rewritten_instruction_interior(
                value,
                &plan,
                guard_address,
                symbol.st_value,
            )?
        {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "current vDSO exports an entry inside a rewritten instruction",
            )
            .into());
        }
    }
    reject_getrandom_direct_interior_entries(
        &elf,
        image,
        load_end,
        value,
        size as u64,
        &plan,
        guard_address,
    )?;
    Ok(Some((start, plan)))
}

#[cfg(target_arch = "x86_64")]
fn liteinst_getrandom_symbol(
    image: &[u8],
) -> Result<Option<(usize, vdso_getrandom_x86_64::PatchPlan)>, Error> {
    let plan = liteinst_getrandom_symbol_layout(
        image,
        vdso_getrandom_x86_64::SGX_TARGET_LOAD_OFFSET as u64,
    )?;
    if plan.is_none() {
        if vdso_getrandom_x86_64::complete_route_free_image_matches(image) {
            return Ok(None);
        }
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "current vDSO lacks both getrandom ABI names and is not an exact reviewed route-free image",
        )
        .into());
    }
    if !vdso_getrandom_x86_64::complete_image_matches(image) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "current vDSO is not the complete reviewed 8,192-byte image",
        )
        .into());
    }
    Ok(plan)
}

#[cfg(target_arch = "x86_64")]
fn stopped_getrandom_is_required(subscriptions: &Subscription) -> bool {
    is_symbol_patch_required(subscriptions, Sysno::getrandom)
}

#[cfg(target_arch = "x86_64")]
fn plan_stopped_getrandom(
    image: &[u8],
    mapping_start: u64,
) -> Result<Option<StoppedVdsoGetrandomPlan>, Error> {
    let Some((function_offset, plan)) = liteinst_getrandom_symbol(image)? else {
        return Ok(None);
    };
    let function_address = mapping_start
        .checked_add(function_offset as u64)
        .ok_or(Errno::EOVERFLOW)?;
    let normal_address = function_address
        .checked_add(plan.normal_branch_offset as u64)
        .ok_or(Errno::EOVERFLOW)?;
    Ok(Some(StoppedVdsoGetrandomPlan {
        mapping_start,
        expected_image: image.into(),
        normal_address,
        normal_expected: plan.normal_branch_expected,
        normal_replacement: plan.normal_branch_bytes,
    }))
}

#[cfg(target_arch = "x86_64")]
pub(crate) fn stopped_patch_required(subscriptions: &Subscription) -> bool {
    VDSO_SYMBOLS
        .iter()
        .any(|(_, _, sysno)| is_symbol_patch_required(subscriptions, *sysno))
        || stopped_getrandom_is_required(subscriptions)
}

#[cfg(target_arch = "x86_64")]
struct StoppedLegacyPatchCandidate {
    patch: StoppedVdsoLegacyPatch,
    symbol_value: u64,
    symbol_end: u64,
    patch_end: u64,
}

#[cfg(target_arch = "x86_64")]
fn stopped_legacy_symbol_is_callable(symbol: &goblin::elf::sym::Sym) -> bool {
    if symbol.st_shndx == goblin::elf::section_header::SHN_UNDEF as usize {
        return false;
    }
    match symbol.st_type() {
        goblin::elf::sym::STT_FUNC | goblin::elf::sym::STT_GNU_IFUNC => true,
        goblin::elf::sym::STT_NOTYPE => {
            matches!(
                symbol.st_bind(),
                goblin::elf::sym::STB_GLOBAL | goblin::elf::sym::STB_WEAK
            ) && matches!(
                symbol.st_visibility(),
                goblin::elf::sym::STV_DEFAULT | goblin::elf::sym::STV_PROTECTED
            )
        }
        _ => false,
    }
}

#[cfg(target_arch = "x86_64")]
fn plan_stopped_legacy_patches(
    image: &[u8],
    subscriptions: &Subscription,
) -> Result<Vec<StoppedVdsoLegacyPatch>, Error> {
    if !VDSO_SYMBOLS
        .iter()
        .any(|(_, _, sysno)| is_symbol_patch_required(subscriptions, *sysno))
    {
        return Ok(Vec::new());
    }
    let elf = Elf::parse(image).map_err(|error| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("could not parse target vDSO for stopped publication: {error}"),
        )
    })?;
    if !elf.is_64
        || !elf.little_endian
        || elf.header.e_type != goblin::elf::header::ET_DYN
        || elf.header.e_machine != goblin::elf::header::EM_X86_64
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "target vDSO is not a little-endian x86-64 shared object",
        )
        .into());
    }

    let mut selected = Vec::new();
    for (name, bytes, sysno) in VDSO_SYMBOLS {
        if !is_symbol_patch_required(subscriptions, *sysno) {
            continue;
        }
        let mut symbols = elf
            .dynsyms
            .iter()
            .filter(|symbol| elf.dynstrtab.get_at(symbol.st_name) == Some(*name));
        let Some(symbol) = symbols.next() else {
            continue;
        };
        if symbols.next().is_some()
            || symbol.st_shndx == goblin::elf::section_header::SHN_UNDEF as usize
            || !symbol.is_function()
            || symbol.st_bind() != goblin::elf::sym::STB_GLOBAL
            || symbol.st_visibility() != goblin::elf::sym::STV_DEFAULT
        {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("target vDSO {name} is not one unique defined global function"),
            )
            .into());
        }
        selected.push((*name, *bytes, symbol));
    }
    if selected.is_empty() {
        return Ok(Vec::new());
    }

    let mut executable_loads = elf.program_headers.iter().filter(|load| {
        load.p_type == goblin::elf::program_header::PT_LOAD
            && load.p_flags & goblin::elf::program_header::PF_X != 0
    });
    let Some(load) = executable_loads.next() else {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "target vDSO has no executable load segment",
        )
        .into());
    };
    let load_size = usize::try_from(load.p_filesz).map_err(|_| Errno::EOVERFLOW)?;
    if executable_loads.next().is_some()
        || load.p_offset != 0
        || load.p_vaddr != 0
        || load.p_filesz != load.p_memsz
        || load_size > image.len()
        || load.p_align != 0x1000
        || load.p_flags != (goblin::elf::program_header::PF_R | goblin::elf::program_header::PF_X)
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "target vDSO has unsupported executable load geometry",
        )
        .into());
    }
    let load_end = load
        .p_vaddr
        .checked_add(load.p_filesz)
        .ok_or(Errno::EOVERFLOW)?;

    let mut executable_ranges = Vec::new();
    let mut canonical_heads = BTreeSet::new();
    let mut decoded = BTreeMap::new();
    let mut direct_targets = Vec::new();
    for section in &elf.section_headers {
        if section.sh_flags & u64::from(goblin::elf::section_header::SHF_EXECINSTR) == 0
            || section.sh_size == 0
        {
            continue;
        }
        let section_end = section
            .sh_addr
            .checked_add(section.sh_size)
            .ok_or(Errno::EOVERFLOW)?;
        let file_end = section
            .sh_offset
            .checked_add(section.sh_size)
            .ok_or(Errno::EOVERFLOW)?;
        let expected_offset = load
            .p_offset
            .checked_add(
                section
                    .sh_addr
                    .checked_sub(load.p_vaddr)
                    .ok_or(Errno::EOVERFLOW)?,
            )
            .ok_or(Errno::EOVERFLOW)?;
        if section.sh_offset != expected_offset
            || section.sh_addr < load.p_vaddr
            || section_end > load_end
            || file_end > image.len() as u64
        {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "target vDSO has unsupported executable-section geometry",
            )
            .into());
        }
        if executable_ranges
            .iter()
            .any(|(start, end)| section.sh_addr < *end && *start < section_end)
        {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "target vDSO has overlapping executable sections",
            )
            .into());
        }
        executable_ranges.push((section.sh_addr, section_end));
        let start = usize::try_from(section.sh_offset).map_err(|_| Errno::EOVERFLOW)?;
        let end = usize::try_from(file_end).map_err(|_| Errno::EOVERFLOW)?;
        let mut decoder = Decoder::with_ip(
            64,
            &image[start..end],
            section.sh_addr,
            DecoderOptions::NONE,
        );
        while decoder.can_decode() {
            let instruction = decoder.decode();
            if instruction.is_invalid() || instruction.next_ip() > section_end {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "target vDSO executable section does not decode completely",
                )
                .into());
            }
            canonical_heads.insert(instruction.ip());
            decoded.insert(
                instruction.ip(),
                (
                    instruction.next_ip(),
                    instruction.mnemonic() == iced_x86::Mnemonic::Nop,
                ),
            );
            let target = match instruction.op0_kind() {
                OpKind::NearBranch16 | OpKind::NearBranch32 | OpKind::NearBranch64 => {
                    Some(instruction.near_branch_target())
                }
                OpKind::FarBranch16 | OpKind::FarBranch32 => {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        "target vDSO contains an unsupported direct far branch",
                    )
                    .into());
                }
                _ => None,
            };
            if let Some(target) = target {
                direct_targets.push((instruction.ip(), target));
            }
        }
    }
    if executable_ranges.is_empty() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "target vDSO has no executable section",
        )
        .into());
    }
    for (_, target) in &direct_targets {
        if *target < load_end && !canonical_heads.contains(target) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "target vDSO direct branch target is not a canonical decoded instruction head",
            )
            .into());
        }
    }

    let mut candidates = Vec::new();
    let mut occupied = Vec::<(usize, usize)>::new();
    for (name, bytes, symbol) in selected {
        let symbol_size = usize::try_from(symbol.st_size).map_err(|_| Errno::EOVERFLOW)?;
        let replacement_len = symbol_size.checked_add(15).ok_or(Errno::EOVERFLOW)? & !15;
        if replacement_len == 0 || bytes.len() > replacement_len {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("target vDSO {name} cannot contain its syscall thunk"),
            )
            .into());
        }
        if symbol.st_value & 15 != 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("target vDSO {name} entry is not 16-byte aligned"),
            )
            .into());
        }
        let symbol_end = symbol
            .st_value
            .checked_add(symbol.st_size)
            .ok_or(Errno::EOVERFLOW)?;
        let patch_end = symbol
            .st_value
            .checked_add(replacement_len as u64)
            .ok_or(Errno::EOVERFLOW)?;
        let mut containing_sections = executable_ranges.iter().filter(|(start, end)| {
            *start <= symbol.st_value && symbol_end <= *end && patch_end <= *end
        });
        let Some((_, section_end)) = containing_sections.next().copied() else {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("target vDSO {name} is not wholly inside one executable section"),
            )
            .into());
        };
        if containing_sections.next().is_some()
            || !canonical_heads.contains(&symbol.st_value)
            || (symbol_end != section_end && !canonical_heads.contains(&symbol_end))
            || (patch_end != section_end && !canonical_heads.contains(&patch_end))
        {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("target vDSO {name} range is not an exact decoded instruction range"),
            )
            .into());
        }
        let mut padding = symbol_end;
        while padding < patch_end {
            let Some((next, is_nop)) = decoded.get(&padding).copied() else {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("target vDSO {name} rounded tail is not decoded padding"),
                )
                .into());
            };
            if !is_nop || next > patch_end {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("target vDSO {name} rounded tail is not executable NOP padding"),
                )
                .into());
            }
            padding = next;
        }
        let offset_u64 = load
            .p_offset
            .checked_add(symbol.st_value)
            .ok_or(Errno::EOVERFLOW)?;
        let offset = usize::try_from(offset_u64).map_err(|_| Errno::EOVERFLOW)?;
        let end = offset
            .checked_add(replacement_len)
            .ok_or(Errno::EOVERFLOW)?;
        if end > image.len()
            || occupied
                .iter()
                .any(|(other_start, other_end)| offset < *other_end && *other_start < end)
        {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("target vDSO {name} patch range is invalid or overlaps"),
            )
            .into());
        }
        occupied.push((offset, end));
        let mut replacement = vec![0x90; replacement_len];
        replacement[..bytes.len()].copy_from_slice(bytes);
        candidates.push(StoppedLegacyPatchCandidate {
            patch: StoppedVdsoLegacyPatch {
                name,
                offset,
                replacement: replacement.into_boxed_slice(),
            },
            symbol_value: symbol.st_value,
            symbol_end,
            patch_end,
        });
    }

    for symbol in elf.dynsyms.iter().chain(elf.syms.iter()) {
        if symbol.st_shndx != goblin::elf::section_header::SHN_UNDEF as usize
            && symbol.st_type() == goblin::elf::sym::STT_GNU_IFUNC
        {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "target vDSO contains a defined GNU IFUNC",
            )
            .into());
        }
        if !stopped_legacy_symbol_is_callable(&symbol) {
            continue;
        }
        if symbol.st_value < load_end && !canonical_heads.contains(&symbol.st_value) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "target vDSO callable symbol is not a canonical decoded instruction head",
            )
            .into());
        }
        let callable_end = symbol
            .st_value
            .checked_add(symbol.st_size.max(1))
            .ok_or(Errno::EOVERFLOW)?;
        for candidate in &candidates {
            if symbol.st_value < candidate.patch_end && candidate.symbol_value < callable_end {
                let same_exact_route = symbol.st_value == candidate.symbol_value
                    && callable_end == candidate.symbol_end;
                if !same_exact_route {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        format!(
                            "target vDSO callable symbol overlaps stopped patch for {} without sharing its exact entry and size",
                            candidate.patch.name,
                        ),
                    )
                    .into());
                }
            }
        }
    }

    for (source, target) in direct_targets {
        let source_is_rewritten = candidates
            .iter()
            .any(|candidate| candidate.symbol_value <= source && source < candidate.patch_end);
        if source_is_rewritten {
            continue;
        }
        for candidate in &candidates {
            if candidate.symbol_value < target && target < candidate.patch_end {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!(
                        "target vDSO direct branch enters stopped patch for {} after its entry",
                        candidate.patch.name
                    ),
                )
                .into());
            }
        }
    }

    if !vdso_getrandom_x86_64::complete_image_matches(image) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "target vDSO is not the complete reviewed 8,192-byte image for stopped legacy publication",
        )
        .into());
    }

    let mut patches = candidates
        .into_iter()
        .map(|candidate| candidate.patch)
        .collect::<Vec<_>>();
    patches.sort_by_key(|patch| patch.offset);
    Ok(patches)
}

/// Plan every selected stopped-target vDSO rewrite from one retained complete
/// target image. No target byte or permission is changed here.
#[cfg(target_arch = "x86_64")]
pub(crate) fn plan_stopped_vdso(
    image: &[u8],
    mapping_start: u64,
    subscriptions: &Subscription,
) -> Result<StoppedVdsoPlan, Error> {
    if mapping_start & 7 != 0 || image.len() < 8 || image.len() % 8 != 0 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "target vDSO mapping is not an aligned complete word range",
        )
        .into());
    }
    let getrandom = if stopped_getrandom_is_required(subscriptions) {
        plan_stopped_getrandom(image, mapping_start)?
    } else {
        None
    };
    let legacy = plan_stopped_legacy_patches(image, subscriptions)?;
    Ok(StoppedVdsoPlan {
        mapping_start,
        expected_image: image.into(),
        getrandom,
        legacy,
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

    let mut syscall_sites = Vec::new();
    for (name, (offset, size, _bytes, _sysno)) in subscribed_vdso_patches(subscriptions) {
        let symbol = start + *offset as usize;
        let number = match name {
            "__vdso_time" => libc::SYS_time,
            "__vdso_clock_gettime" => libc::SYS_clock_gettime,
            "__vdso_getcpu" => libc::SYS_getcpu,
            "__vdso_gettimeofday" => libc::SYS_gettimeofday,
            "__vdso_clock_getres" => libc::SYS_clock_getres,
            _ => continue,
        };
        assert!(*size >= 3);
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
    const LEGACY_FIXTURE_TEXT: usize = 0x100;
    #[cfg(target_arch = "x86_64")]
    const LEGACY_FIXTURE_SYMBOL: usize = 0x120;
    #[cfg(target_arch = "x86_64")]
    const LEGACY_FIXTURE_DYNSTR: usize = 0x200;
    #[cfg(target_arch = "x86_64")]
    const LEGACY_FIXTURE_DYNSYM: usize = 0x240;
    #[cfg(target_arch = "x86_64")]
    const LEGACY_FIXTURE_STRTAB: usize = 0x300;
    #[cfg(target_arch = "x86_64")]
    const LEGACY_FIXTURE_SYMTAB: usize = 0x340;
    #[cfg(target_arch = "x86_64")]
    const LEGACY_FIXTURE_SHSTRTAB: usize = 0x400;
    #[cfg(target_arch = "x86_64")]
    const LEGACY_FIXTURE_SHOFF: usize = 0x500;

    #[cfg(target_arch = "x86_64")]
    fn put_u16(image: &mut [u8], offset: usize, value: u16) {
        image[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
    }

    #[cfg(target_arch = "x86_64")]
    fn put_u32(image: &mut [u8], offset: usize, value: u32) {
        image[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    }

    #[cfg(target_arch = "x86_64")]
    fn put_u64(image: &mut [u8], offset: usize, value: u64) {
        image[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
    }

    #[cfg(target_arch = "x86_64")]
    #[allow(clippy::too_many_arguments)]
    fn put_section(
        image: &mut [u8],
        index: usize,
        name: u32,
        section_type: u32,
        flags: u64,
        address: u64,
        offset: u64,
        size: u64,
        link: u32,
        info: u32,
        alignment: u64,
        entry_size: u64,
    ) {
        let header = LEGACY_FIXTURE_SHOFF + index * 64;
        put_u32(image, header, name);
        put_u32(image, header + 4, section_type);
        put_u64(image, header + 8, flags);
        put_u64(image, header + 16, address);
        put_u64(image, header + 24, offset);
        put_u64(image, header + 32, size);
        put_u32(image, header + 40, link);
        put_u32(image, header + 44, info);
        put_u64(image, header + 48, alignment);
        put_u64(image, header + 56, entry_size);
    }

    #[cfg(target_arch = "x86_64")]
    #[allow(clippy::too_many_arguments)]
    fn put_symbol(
        image: &mut [u8],
        table: usize,
        index: usize,
        name: u32,
        binding: u8,
        symbol_type: u8,
        visibility: u8,
        section: u16,
        value: u64,
        size: u64,
    ) {
        let symbol = table + index * 24;
        put_u32(image, symbol, name);
        image[symbol + 4] = binding << 4 | symbol_type;
        image[symbol + 5] = visibility;
        put_u16(image, symbol + 6, section);
        put_u64(image, symbol + 8, value);
        put_u64(image, symbol + 16, size);
    }

    #[cfg(target_arch = "x86_64")]
    fn put_relative_jump(image: &mut [u8], source: usize, target: usize) {
        image[source] = 0xe9;
        let displacement = i32::try_from(target)
            .unwrap()
            .checked_sub(i32::try_from(source + 5).unwrap())
            .unwrap();
        image[source + 1..source + 5].copy_from_slice(&displacement.to_le_bytes());
    }

    #[cfg(target_arch = "x86_64")]
    fn legacy_stopped_fixture() -> Vec<u8> {
        const IMAGE_LEN: usize = 0x800;
        const TEXT_SIZE: usize = 0x80;
        const DYNSYM_ENTRIES: usize = 3;
        const SYMTAB_ENTRIES: usize = 2;
        const DYNSTR: &[u8] = b"\0__vdso_time\0alias\0";
        const STRTAB: &[u8] = b"\0sym_alias\0";
        const SHSTRTAB: &[u8] = b"\0.text\0.dynstr\0.dynsym\0.strtab\0.symtab\0.shstrtab\0";

        let mut image = vec![0_u8; IMAGE_LEN];
        image[..7].copy_from_slice(b"\x7fELF\x02\x01\x01");
        put_u16(&mut image, 16, goblin::elf::header::ET_DYN);
        put_u16(&mut image, 18, goblin::elf::header::EM_X86_64);
        put_u32(&mut image, 20, 1);
        put_u64(&mut image, 32, 64);
        put_u64(&mut image, 40, LEGACY_FIXTURE_SHOFF as u64);
        put_u16(&mut image, 52, 64);
        put_u16(&mut image, 54, 56);
        put_u16(&mut image, 56, 1);
        put_u16(&mut image, 58, 64);
        put_u16(&mut image, 60, 7);
        put_u16(&mut image, 62, 6);

        put_u32(&mut image, 64, goblin::elf::program_header::PT_LOAD);
        put_u32(
            &mut image,
            68,
            goblin::elf::program_header::PF_R | goblin::elf::program_header::PF_X,
        );
        put_u64(&mut image, 96, (LEGACY_FIXTURE_TEXT + TEXT_SIZE) as u64);
        put_u64(&mut image, 104, (LEGACY_FIXTURE_TEXT + TEXT_SIZE) as u64);
        put_u64(&mut image, 112, 0x1000);

        image[LEGACY_FIXTURE_TEXT..LEGACY_FIXTURE_TEXT + TEXT_SIZE].fill(0x90);
        put_relative_jump(&mut image, LEGACY_FIXTURE_TEXT, LEGACY_FIXTURE_SYMBOL);
        put_relative_jump(&mut image, LEGACY_FIXTURE_SYMBOL, 0x160);
        image[0x160] = 0xc3;

        image[LEGACY_FIXTURE_DYNSTR..LEGACY_FIXTURE_DYNSTR + DYNSTR.len()].copy_from_slice(DYNSTR);
        image[LEGACY_FIXTURE_STRTAB..LEGACY_FIXTURE_STRTAB + STRTAB.len()].copy_from_slice(STRTAB);
        image[LEGACY_FIXTURE_SHSTRTAB..LEGACY_FIXTURE_SHSTRTAB + SHSTRTAB.len()]
            .copy_from_slice(SHSTRTAB);
        put_symbol(
            &mut image,
            LEGACY_FIXTURE_DYNSYM,
            1,
            1,
            goblin::elf::sym::STB_GLOBAL,
            goblin::elf::sym::STT_FUNC,
            goblin::elf::sym::STV_DEFAULT,
            1,
            LEGACY_FIXTURE_SYMBOL as u64,
            5,
        );

        put_section(
            &mut image,
            1,
            1,
            goblin::elf::section_header::SHT_PROGBITS,
            u64::from(
                goblin::elf::section_header::SHF_ALLOC | goblin::elf::section_header::SHF_EXECINSTR,
            ),
            LEGACY_FIXTURE_TEXT as u64,
            LEGACY_FIXTURE_TEXT as u64,
            TEXT_SIZE as u64,
            0,
            0,
            16,
            0,
        );
        put_section(
            &mut image,
            2,
            7,
            goblin::elf::section_header::SHT_STRTAB,
            0,
            0,
            LEGACY_FIXTURE_DYNSTR as u64,
            DYNSTR.len() as u64,
            0,
            0,
            1,
            0,
        );
        put_section(
            &mut image,
            3,
            15,
            goblin::elf::section_header::SHT_DYNSYM,
            0,
            0,
            LEGACY_FIXTURE_DYNSYM as u64,
            (DYNSYM_ENTRIES * 24) as u64,
            2,
            1,
            8,
            24,
        );
        put_section(
            &mut image,
            4,
            23,
            goblin::elf::section_header::SHT_STRTAB,
            0,
            0,
            LEGACY_FIXTURE_STRTAB as u64,
            STRTAB.len() as u64,
            0,
            0,
            1,
            0,
        );
        put_section(
            &mut image,
            5,
            31,
            goblin::elf::section_header::SHT_SYMTAB,
            0,
            0,
            LEGACY_FIXTURE_SYMTAB as u64,
            (SYMTAB_ENTRIES * 24) as u64,
            4,
            1,
            8,
            24,
        );
        put_section(
            &mut image,
            6,
            39,
            goblin::elf::section_header::SHT_STRTAB,
            0,
            0,
            LEGACY_FIXTURE_SHSTRTAB as u64,
            SHSTRTAB.len() as u64,
            0,
            0,
            1,
            0,
        );
        image
    }

    #[cfg(target_arch = "x86_64")]
    fn legacy_stopped_error(image: &[u8]) -> String {
        plan_stopped_legacy_patches(image, &[Sysno::time].into_iter().collect::<Subscription>())
            .unwrap_err()
            .to_string()
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

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn stopped_legacy_patch_admits_exact_function_and_nop_padding() {
        let image = vdso_getrandom_x86_64::reviewed_image_for_test();
        let patches = plan_stopped_legacy_patches(
            &image,
            &[Sysno::time].into_iter().collect::<Subscription>(),
        )
        .unwrap();

        assert_eq!(patches.len(), 1);
        assert_eq!(patches[0].name, "__vdso_time");
        assert_eq!(patches[0].offset, 0xaa0);
        assert_eq!(patches[0].replacement.len(), 48);
        assert_eq!(&patches[0].replacement[..8], vdso_syms::time);
        assert!(patches[0].replacement[8..].iter().all(|byte| *byte == 0x90));
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn stopped_legacy_patch_admits_alias_sharing_exact_route() {
        let image = vdso_getrandom_x86_64::reviewed_image_for_test();
        let elf = Elf::parse(&image).unwrap();
        let symbol = |name| {
            elf.dynsyms
                .iter()
                .find(|symbol| elf.dynstrtab.get_at(symbol.st_name) == Some(name))
                .unwrap()
        };
        let canonical = symbol("__vdso_time");
        let alias = symbol("time");

        assert_eq!(canonical.st_bind(), goblin::elf::sym::STB_GLOBAL);
        assert_eq!(alias.st_bind(), goblin::elf::sym::STB_WEAK);
        assert_eq!(canonical.st_value, alias.st_value);
        assert_eq!(canonical.st_size, alias.st_size);

        assert_eq!(
            plan_stopped_legacy_patches(
                &image,
                &[Sysno::time].into_iter().collect::<Subscription>(),
            )
            .unwrap()
            .len(),
            1
        );
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn stopped_legacy_patch_rejects_changed_indirect_call_target() {
        let mut image = vdso_getrandom_x86_64::reviewed_image_for_test();
        let target = vdso_getrandom_x86_64::SGX_INDIRECT_CALL_OFFSET + 1;
        assert_eq!(image[target], 0xd0);
        image[target] = 0xd1;

        assert!(legacy_stopped_error(&image).contains(
            "target vDSO is not the complete reviewed 8,192-byte image for stopped legacy publication"
        ));
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn stopped_legacy_patch_rejects_dynamic_symbol_alias() {
        let mut image = legacy_stopped_fixture();
        put_symbol(
            &mut image,
            LEGACY_FIXTURE_DYNSYM,
            2,
            13,
            goblin::elf::sym::STB_WEAK,
            goblin::elf::sym::STT_FUNC,
            goblin::elf::sym::STV_DEFAULT,
            1,
            (LEGACY_FIXTURE_SYMBOL + 5) as u64,
            4,
        );

        assert!(
            legacy_stopped_error(&image).contains(
                "callable symbol overlaps stopped patch for __vdso_time without sharing its exact entry and size"
            )
        );
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn stopped_legacy_patch_rejects_symtab_only_interior_entry() {
        let mut image = legacy_stopped_fixture();
        put_symbol(
            &mut image,
            LEGACY_FIXTURE_SYMTAB,
            1,
            1,
            goblin::elf::sym::STB_LOCAL,
            goblin::elf::sym::STT_FUNC,
            goblin::elf::sym::STV_DEFAULT,
            1,
            (LEGACY_FIXTURE_SYMBOL + 8) as u64,
            1,
        );

        assert!(
            legacy_stopped_error(&image).contains(
                "callable symbol overlaps stopped patch for __vdso_time without sharing its exact entry and size"
            )
        );
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn stopped_legacy_patch_rejects_external_branch_into_rounded_tail() {
        let mut image = legacy_stopped_fixture();
        put_relative_jump(&mut image, LEGACY_FIXTURE_TEXT, LEGACY_FIXTURE_SYMBOL + 6);

        assert!(
            legacy_stopped_error(&image)
                .contains("direct branch enters stopped patch for __vdso_time after its entry")
        );
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn stopped_legacy_patch_rejects_branch_into_instruction_interior() {
        let mut image = legacy_stopped_fixture();
        put_relative_jump(&mut image, LEGACY_FIXTURE_TEXT, LEGACY_FIXTURE_SYMBOL + 2);

        assert!(
            legacy_stopped_error(&image)
                .contains("direct branch target is not a canonical decoded instruction head")
        );
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn stopped_legacy_patch_rejects_non_nop_rounded_tail() {
        let mut image = legacy_stopped_fixture();
        image[LEGACY_FIXTURE_SYMBOL + 5] = 0xc3;

        assert!(
            legacy_stopped_error(&image).contains("rounded tail is not executable NOP padding")
        );
    }
}
