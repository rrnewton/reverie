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
use tracing::warn;

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

    // `__vdso_getrandom(buffer, len, flags, opaque_state, opaque_len)`.
    //
    // The kernel's implementation keys a per-thread ChaCha20 state with one
    // `getrandom(key, 32, 0)` syscall and re-keys whenever the kernel's crng
    // generation changes, i.e. at host-time-dependent points
    // (https://github.com/rrnewton/reverie/issues/841). This replacement never
    // touches the opaque state:
    //
    // * The parameter query `(NULL, 0, 0, params, ~0UL)` returns -ENOSYS
    //   without writing `params`. glibc 2.41+ (`__getrandom_early_init`) then
    //   leaves its state size at zero and serves every getrandom() with the
    //   getrandom syscall.
    // * Any other call is the getrandom syscall on the caller's first three
    //   arguments, which the function ABI already passes in rdi/rsi/rdx. This
    //   is the kernel vDSO's own fallback, and it keeps a libc that queried
    //   the unpatched vDSO before the patch (LiteInst patches from a
    //   constructor, after glibc's early init) working: its draws become
    //   syscalls instead of failing with -ENOSYS.
    const getrandom_code: BufferAligned<40> = BufferAligned::<40>([
        0x49, 0x83, 0xf8, 0xff, // cmp $-1, %r8
        0x75, 0x16, // jne syscall_path
        0x48, 0x85, 0xff, // test %rdi, %rdi
        0x75, 0x11, // jne syscall_path
        0x48, 0x85, 0xf6, // test %rsi, %rsi
        0x75, 0x0c, // jne syscall_path
        0x85, 0xd2, // test %edx, %edx
        0x75, 0x08, // jne syscall_path
        0x48, 0xc7, 0xc0, 0xda, 0xff, 0xff, 0xff, // mov $-ENOSYS, %rax
        0xc3, // retq
        // syscall_path:
        0xb8, 0x3e, 0x01, 0x00, 0x00, // mov SYS_getrandom, %eax
        0x0f, 0x05, // syscall
        0xc3, // retq
        0x90, 0x90, 0x90, 0x90, // padding
    ]);

    pub const getrandom: &[u8; 40] = &getrandom_code.0;

    /// Offset of the `syscall` instruction within [`getrandom`]. Every branch
    /// in the stub targets an address before it, so an in-guest hook may
    /// overwrite the bytes from here on.
    pub const GETRANDOM_SYSCALL_OFFSET: usize = 33;

    // The replacement for an entry point with no syscall equivalent, or one
    // this module does not know: it returns -ENOSYS without touching memory.
    const enosys_code: BufferAligned<8> = BufferAligned::<8>([
        0x48, 0xc7, 0xc0, 0xda, 0xff, 0xff, 0xff, // mov $-ENOSYS, %rax
        0xc3, // retq
    ]);

    pub const enosys: &[u8; 8] = &enosys_code.0;
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

    // `__kernel_getrandom`: the same contract as the x86_64 stub (see the
    // comment there). The parameter query `(NULL, 0, 0, params, ~0UL)`
    // returns -ENOSYS; every other call is the getrandom syscall on x0-x2.
    const getrandom_code: BufferAligned<48> = BufferAligned::<48>([
        0x5f, 0x24, 0x03, 0xd5, // bti c
        0x9f, 0x04, 0x00, 0xb1, // cmn x4, #1
        0xc1, 0x00, 0x00, 0x54, // b.ne syscall_path
        0xa0, 0x00, 0x00, 0xb5, // cbnz x0, syscall_path
        0x81, 0x00, 0x00, 0xb5, // cbnz x1, syscall_path
        0x62, 0x00, 0x00, 0x35, // cbnz w2, syscall_path
        0xa0, 0x04, 0x80, 0x92, // mov x0, #-38 (-ENOSYS)
        0xc0, 0x03, 0x5f, 0xd6, // ret
        // syscall_path:
        0xc8, 0x22, 0x80, 0xd2, // mov x8, 278 (#__NR_getrandom)
        0x01, 0x00, 0x00, 0xd4, // svc 0
        0xc0, 0x03, 0x5f, 0xd6, // ret
        0x1f, 0x20, 0x03, 0xd5, // nop
    ]);

    pub const getrandom: &[u8; 48] = &getrandom_code.0;

    // See the x86_64 `enosys` stub.
    const enosys_code: BufferAligned<16> = BufferAligned::<16>([
        0x5f, 0x24, 0x03, 0xd5, // bti c
        0xa0, 0x04, 0x80, 0x92, // mov x0, #-38 (-ENOSYS)
        0xc0, 0x03, 0x5f, 0xd6, // ret
        0x1f, 0x20, 0x03, 0xd5, // nop
    ]);

    pub const enosys: &[u8; 16] = &enosys_code.0;
}

/// What reverie does with a vDSO entry point it knows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum KnownEntry {
    /// A userspace fast path for the syscall, replaced by a stub that issues it.
    Syscall(&'static [u8], Sysno),
    /// An entry point with no syscall equivalent, replaced by
    /// [`vdso_syms::enosys`].
    Enosys,
}

/// Every vDSO entry point reverie knows, by its kernel name.
///
/// This is not the list of entry points that get handled. The patch plan covers
/// every function the vDSO's dynamic symbol table exports; one missing here is
/// replaced by [`vdso_syms::enosys`] and logged. The x86_64 vDSO also exports
/// `clock_gettime`, `time` and so on at the same addresses as the `__vdso_`
/// names; such aliases share the entry point, and its replacement.
///
/// Which replacement is safe depends on how libc calls the entry point. From
/// glibc 2.42 source:
/// - `clock_gettime`: `__clock_gettime64` returns any nonzero vDSO result as an
///   error, without trying the syscall (`sysdeps/unix/sysv/linux/clock_gettime.c`
///   lines 41-46).
/// - `gettimeofday`, and x86_64 `time`: IFUNCs that resolve to the vDSO function
///   itself, so its return value goes straight to the caller (`gettimeofday.c`
///   lines 42-45, `time.c` lines 38-40; `x86/gettimeofday.c`, `x86/time.c` and
///   `aarch64/gettimeofday.c` select them with `USE_IFUNC_*`).
/// - `clock_getres` and `getcpu`: `INLINE_VSYSCALL`, which retries with the
///   syscall when the vDSO returns -ENOSYS (`sysdep-vdso.h` lines 42 and 46).
/// - `getrandom`: see its stub.
/// - aarch64 `__kernel_rt_sigreturn`: glibc never calls it, but glibc does not
///   set `SA_RESTORER` (`aarch64/libc_sigaction.c`), so the kernel returns from
///   every signal handler through it.
///
/// An -ENOSYS stub would turn the first two into visible failures and the last
/// into a crash on every signal return, so every syscall fast path keeps a stub
/// that issues its syscall. -ENOSYS is for entry points glibc does not call.
/// glibc only calls vDSO functions it names (`HAVE_*_VSYSCALL` in each
/// architecture's `sysdep.h`), so a future glibc calling an entry point added
/// after this table either retries with the syscall (`INLINE_VSYSCALL`) or
/// fails visibly; it never runs the kernel's code unobserved.
#[cfg(target_arch = "x86_64")]
const VDSO_SYMBOLS: &[(&str, KnownEntry)] = &[
    (
        "__vdso_time",
        KnownEntry::Syscall(vdso_syms::time, Sysno::time),
    ),
    (
        "__vdso_clock_gettime",
        KnownEntry::Syscall(vdso_syms::clock_gettime, Sysno::clock_gettime),
    ),
    (
        "__vdso_getcpu",
        KnownEntry::Syscall(vdso_syms::getcpu, Sysno::getcpu),
    ),
    (
        "__vdso_gettimeofday",
        KnownEntry::Syscall(vdso_syms::gettimeofday, Sysno::gettimeofday),
    ),
    (
        "__vdso_clock_getres",
        KnownEntry::Syscall(vdso_syms::clock_getres, Sysno::clock_getres),
    ),
    (
        "__vdso_getrandom",
        KnownEntry::Syscall(vdso_syms::getrandom, Sysno::getrandom),
    ),
    // SGX enclave entry (`arch/x86/entry/vdso/vsgx.S`). glibc does not call it,
    // and a tool cannot observe what an enclave does. The kernel's version
    // already returns negative errnos (-EINVAL for an invalid leaf), so SGX
    // runtimes handle a negative result.
    ("__vdso_sgx_enter_enclave", KnownEntry::Enosys),
];

#[cfg(target_arch = "aarch64")]
const VDSO_SYMBOLS: &[(&str, KnownEntry)] = &[
    (
        "__kernel_clock_getres",
        KnownEntry::Syscall(vdso_syms::clock_getres, Sysno::clock_getres),
    ),
    (
        "__kernel_clock_gettime",
        KnownEntry::Syscall(vdso_syms::clock_gettime, Sysno::clock_gettime),
    ),
    (
        "__kernel_gettimeofday",
        KnownEntry::Syscall(vdso_syms::gettimeofday, Sysno::gettimeofday),
    ),
    (
        "__kernel_rt_sigreturn",
        KnownEntry::Syscall(vdso_syms::rt_sigreturn, Sysno::rt_sigreturn),
    ),
    (
        "__kernel_getrandom",
        KnownEntry::Syscall(vdso_syms::getrandom, Sysno::getrandom),
    ),
];

/// Rounds up `value` so that it is a multiple of `alignment`.
fn align_up(value: usize, alignment: usize) -> usize {
    (value + alignment - 1) & alignment.wrapping_neg()
}

/// One entry point exported by a vDSO's dynamic symbol table.
#[derive(Clone, Debug, PartialEq, Eq)]
struct VdsoExport {
    name: String,
    /// The symbol's value: its offset into the vDSO, which is linked at 0.
    offset: u64,
    /// The symbol's `st_size`.
    size: usize,
    /// The end of the section that contains the symbol, if the image has
    /// section headers.
    section_end: Option<u64>,
}

/// What a vDSO entry point is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum VdsoEntryKind {
    Known(KnownEntry),
    /// Not in [`VDSO_SYMBOLS`].
    Unknown,
}

/// One vDSO entry point, with every name exported at its address.
#[derive(Clone, Debug, PartialEq, Eq)]
struct VdsoEntry {
    /// The exported names, the known one (if any) first.
    names: Vec<String>,
    offset: u64,
    /// The bytes a stub must fit in: `st_size` rounded up to 16, the function
    /// alignment of the x86_64 and aarch64 vDSOs, cut short at the next entry
    /// point and at the end of the containing section. The rounding is an
    /// assumption about the image, so only a stub longer than `st_size` relies
    /// on it; see [`VdsoEntry::patch_len`].
    size: usize,
    /// The symbol's `st_size`.
    symbol_size: usize,
    kind: VdsoEntryKind,
}

impl VdsoEntry {
    /// The stub that replaces this entry point under `subscriptions`, or `None`
    /// to leave it native.
    ///
    /// A syscall fast path is replaced only when its own syscall is subscribed.
    /// Rewriting an entry point nobody subscribed to converts a pure userspace
    /// vDSO call into a real syscall for no gain: the tool never sees it, and
    /// the guest pays the kernel crossing anyway. That cost is not hypothetical
    /// -- QEMU's main loop calls `clock_gettime` continuously *while holding the
    /// Big QEMU Lock*, so every needless crossing is taken with a lock held that
    /// vCPU threads are waiting on.
    ///
    /// Every other entry point is replaced by [`vdso_syms::enosys`] whenever the
    /// tool subscribes to any syscall, because an unknown entry point could be
    /// the fast path of any of them. A tool that subscribes to no syscall
    /// observes nothing a vDSO function could bypass.
    fn replacement(&self, subscriptions: &Subscription) -> Option<&'static [u8]> {
        match self.kind {
            VdsoEntryKind::Known(KnownEntry::Syscall(stub, sysno)) => subscriptions
                .iter_syscalls()
                .any(|syscall| syscall == sysno)
                .then_some(stub),
            VdsoEntryKind::Known(KnownEntry::Enosys) | VdsoEntryKind::Unknown => subscriptions
                .iter_syscalls()
                .next()
                .is_some()
                .then_some(vdso_syms::enosys),
        }
    }

    /// The bytes a `stub_len`-byte stub and its NOP fill overwrite: the whole
    /// function, and padding only where the stub itself is longer. Filling to
    /// `size` would write over whatever follows the function when the image is
    /// not padded as assumed; on this x86_64 host the last entry point ends 4
    /// bytes before `.altinstructions`, which `size` rounded into.
    fn patch_len(&self, stub_len: usize) -> usize {
        debug_assert!(stub_len <= self.size);
        self.symbol_size.max(stub_len)
    }

    fn describe(&self) -> String {
        format!("{}@{:#x}", self.names.join("/"), self.offset)
    }
}

/// Every entry point exported by the vDSO image `image`.
///
/// This fails closed on the symbol type: only data (`STT_OBJECT`, `STT_TLS`,
/// `STT_COMMON`) and bookkeeping (`STT_SECTION`, `STT_FILE`) symbols are
/// skipped, so an export of any other type is an entry point. That includes
/// aarch64's `__kernel_rt_sigreturn`, which is `STT_NOTYPE`. Local, undefined
/// and absolute symbols are not exported functions; older vDSOs export their
/// version names, such as `LINUX_2.6`, as absolute symbols.
fn vdso_exports(image: &[u8]) -> Result<Vec<VdsoExport>, String> {
    use goblin::elf::section_header::SHN_ABS;
    use goblin::elf::section_header::SHN_UNDEF;
    use goblin::elf::sym::STB_LOCAL;
    use goblin::elf::sym::STT_COMMON;
    use goblin::elf::sym::STT_FILE;
    use goblin::elf::sym::STT_OBJECT;
    use goblin::elf::sym::STT_SECTION;
    use goblin::elf::sym::STT_TLS;

    let elf = Elf::parse(image).map_err(|error| format!("cannot parse the vDSO: {error}"))?;
    let mut exports = Vec::new();
    for sym in elf.dynsyms.iter() {
        if sym.st_bind() == STB_LOCAL
            || sym.st_shndx == SHN_UNDEF as usize
            || sym.st_shndx == SHN_ABS as usize
            || matches!(
                sym.st_type(),
                STT_OBJECT | STT_TLS | STT_COMMON | STT_SECTION | STT_FILE
            )
        {
            continue;
        }
        let name = elf.dynstrtab.get_at(sym.st_name).ok_or_else(|| {
            format!(
                "vDSO symbol name offset {} is outside its string table",
                sym.st_name
            )
        })?;
        exports.push(VdsoExport {
            name: name.to_owned(),
            offset: sym.st_value,
            size: sym.st_size as usize,
            section_end: elf
                .section_headers
                .get(sym.st_shndx)
                .map(|section| section.sh_addr + section.sh_size),
        });
    }
    Ok(exports)
}

/// Groups `exports` by address into entry points and classifies each against
/// [`VDSO_SYMBOLS`].
///
/// Refuses, rather than guess, whenever a stub cannot be placed safely: two
/// known names that need different stubs at one address, names at one address
/// that disagree on the size, entry points that overlap, or a stub larger than
/// its entry point.
fn classify_vdso_exports(exports: &[VdsoExport]) -> Result<Vec<VdsoEntry>, String> {
    let mut by_offset: BTreeMap<u64, Vec<&VdsoExport>> = BTreeMap::new();
    for export in exports {
        by_offset.entry(export.offset).or_default().push(export);
    }

    let offsets: Vec<u64> = by_offset.keys().copied().collect();
    let mut entries = Vec::new();
    for (index, (offset, group)) in by_offset.into_iter().enumerate() {
        let first = group[0];
        if let Some(other) = group.iter().find(|export| export.size != first.size) {
            return Err(format!(
                "vDSO exports {} ({} bytes) and {} ({} bytes) at one address {offset:#x}",
                first.name, first.size, other.name, other.size
            ));
        }

        let mut names: Vec<String> = Vec::new();
        let mut known: Option<KnownEntry> = None;
        for export in &group {
            match VDSO_SYMBOLS.iter().find(|(name, _)| *name == export.name) {
                Some((name, entry)) => {
                    if let Some(previous) = known
                        && previous != *entry
                    {
                        return Err(format!(
                            "vDSO exports {} and {name} at one address {offset:#x}, \
                             but they need different stubs",
                            names[0]
                        ));
                    }
                    if known.is_none() {
                        names.insert(0, export.name.clone());
                    } else {
                        names.push(export.name.clone());
                    }
                    known = Some(*entry);
                }
                None => names.push(export.name.clone()),
            }
        }

        let next = offsets.get(index + 1).copied();
        if let Some(next) = next
            && offset + first.size as u64 > next
        {
            return Err(format!(
                "vDSO entry point {}@{offset:#x} ({} bytes) overlaps the one at {next:#x}",
                names[0], first.size
            ));
        }
        if let Some(section_end) = first.section_end
            && offset + first.size as u64 > section_end
        {
            return Err(format!(
                "vDSO entry point {}@{offset:#x} ({} bytes) runs past the end of \
                 its section at {section_end:#x}",
                names[0], first.size
            ));
        }
        let mut size = align_up(first.size, 16);
        if let Some(next) = next {
            size = size.min((next - offset) as usize);
        }
        if let Some(section_end) = first.section_end {
            size = size.min((section_end - offset) as usize);
        }

        let kind = known.map_or(VdsoEntryKind::Unknown, VdsoEntryKind::Known);
        let stub_len = match kind {
            VdsoEntryKind::Known(KnownEntry::Syscall(stub, _)) => stub.len(),
            _ => vdso_syms::enosys.len(),
        };
        if stub_len > size {
            return Err(format!(
                "vDSO entry point {}@{offset:#x} has {size} bytes, \
                 too few for its {stub_len}-byte stub",
                names[0]
            ));
        }
        entries.push(VdsoEntry {
            names,
            offset,
            size,
            symbol_size: first.size,
            kind,
        });
    }
    Ok(entries)
}

/// The classified entry points of a vDSO image.
#[derive(Debug)]
struct VdsoTable {
    /// The length of the `[vdso]` mapping the image was read from.
    mapping_len: u64,
    entries: Vec<VdsoEntry>,
}

/// This process's own vDSO, classified, or `None` if it has no vDSO.
///
/// The ptrace backend assumes every guest maps the same image, which the kernel
/// shares between all 64-bit processes; [`vdso_patch`] refuses a guest whose
/// mapping has a different length. In-guest backends read their own vDSO.
static VDSO_TABLE: LazyLock<Result<Option<VdsoTable>, String>> = LazyLock::new(|| {
    let maps = procfs::process::Process::myself()
        .and_then(|process| process.maps())
        .map_err(|error| format!("cannot read this process's mappings: {error}"))?;
    let Some(vdso) = maps
        .iter()
        .find(|map| map.pathname == procfs::process::MMapPath::Vdso)
    else {
        return Ok(None);
    };
    let mapping_len = vdso.address.1 - vdso.address.0;
    // SAFETY: the kernel keeps the vDSO mapped, readable and unmodified for the
    // life of the process; this runs before any patch of this process's vDSO.
    let image =
        unsafe { std::slice::from_raw_parts(vdso.address.0 as *const u8, mapping_len as usize) };
    let entries = classify_vdso_exports(&vdso_exports(image)?)?;
    for entry in &entries {
        match entry.kind {
            VdsoEntryKind::Unknown => warn!(
                "vDSO exports unknown entry point {}; it is replaced by an -ENOSYS stub \
                 whenever the tool subscribes to any syscall",
                entry.describe()
            ),
            VdsoEntryKind::Known(kind) => debug!("vDSO entry point {}: {kind:?}", entry.describe()),
        }
    }
    Ok(Some(VdsoTable {
        mapping_len,
        entries,
    }))
});

/// The vDSO table, or the reason it could not be built.
fn vdso_table() -> Result<Option<&'static VdsoTable>, Error> {
    VDSO_TABLE
        .as_ref()
        .map(Option::as_ref)
        .map_err(|error| Error::from(std::io::Error::other(error.clone())))
}

/// The entry points `subscriptions` selects for replacement, with their stubs,
/// in address order.
///
/// Both the in-process and stopped-guest patch paths consume this, so their
/// selection is implemented in one place.
fn vdso_replacements(
    subscriptions: &Subscription,
) -> Result<Vec<(&'static VdsoEntry, &'static [u8])>, Error> {
    if subscriptions.iter_syscalls().next().is_none() {
        return Ok(Vec::new());
    }
    let Some(table) = vdso_table()? else {
        return Ok(Vec::new());
    };
    Ok(table
        .entries
        .iter()
        .filter_map(|entry| Some((entry, entry.replacement(subscriptions)?)))
        .collect())
}

/// Reports every entry point of `table` that `replacements` leaves native: a
/// warning the first time in this process, then at debug level.
fn log_native_entries(table: &VdsoTable, replacements: &[(&VdsoEntry, &[u8])]) {
    static WARNED: std::sync::Once = std::sync::Once::new();
    let native: Vec<String> = table
        .entries
        .iter()
        .filter(|entry| {
            !replacements
                .iter()
                .any(|(replaced, _)| std::ptr::eq(*replaced, *entry))
        })
        .map(VdsoEntry::describe)
        .collect();
    if native.is_empty() {
        return;
    }
    let native = native.join(", ");
    let mut warned = false;
    WARNED.call_once(|| {
        warned = true;
        warn!(
            "vDSO entry points left native because the tool does not subscribe to their \
             syscalls, so it does not observe these calls: {native}"
        );
    });
    if !warned {
        debug!("vDSO entry points left native: {native}");
    }
}

/// Does `subscriptions` require patching the vDSO? A vDSO that could not be
/// read or classified does, so that [`vdso_patch`] reports why.
pub fn is_patch_required(subscriptions: &Subscription) -> bool {
    !matches!(vdso_replacements(subscriptions), Ok(replacements) if replacements.is_empty())
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
/// Returns each rewritten syscall fast path's entry address and syscall number
/// so an in-guest patching backend can install its ordinary syscall trampoline
/// while the process is still single-threaded. The two-byte syscall is
/// deliberately placed at the aligned symbol entry: LiteInst needs a full patch
/// word after the hook address, which is not guaranteed at the tail of an
/// eight-byte pseudo-vDSO function. `__vdso_getrandom` is the exception: it
/// keeps the table's whole stub, which answers the parameter query without a
/// syscall, and its site is the stub's `syscall`, followed only by `ret` and
/// padding. Entry points with no syscall equivalent, known or not, get the
/// -ENOSYS stub, which needs no hook. This shares the authoritative vDSO table
/// with ptrace's stopped-guest path instead of maintaining a backend-specific
/// list.
#[cfg(target_arch = "x86_64")]
pub fn patch_current_vdso(subscriptions: &Subscription) -> Result<Vec<VdsoSyscallSite>, Error> {
    let replacements = vdso_replacements(subscriptions)?;
    if replacements.is_empty() {
        return Ok(Vec::new());
    }
    if let Some(table) = vdso_table()? {
        log_native_entries(table, &replacements);
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
    for (entry, bytes) in replacements {
        let symbol = start + entry.offset as usize;
        let size = entry.size;
        match entry.kind {
            VdsoEntryKind::Known(KnownEntry::Syscall(_, Sysno::getrandom)) => {
                // The parameter query must be answered before any syscall, so
                // this entry keeps its whole stub and is hooked at the stub's
                // `syscall`. Every byte after that `syscall` is a `ret` or
                // padding, which leaves the hook its full patch word.
                assert!(size >= bytes.len() + 8);
                let patch_len = entry.patch_len(bytes.len() + 8);
                unsafe {
                    core::ptr::copy_nonoverlapping(bytes.as_ptr(), symbol as *mut u8, bytes.len());
                    core::ptr::write_bytes(
                        (symbol + bytes.len()) as *mut u8,
                        0x90,
                        patch_len - bytes.len(),
                    );
                }
                syscall_sites.push(VdsoSyscallSite {
                    address: (symbol + vdso_syms::GETRANDOM_SYSCALL_OFFSET) as u64,
                    number: libc::SYS_getrandom,
                    mapping_start: start as u64,
                    mapping_len: len as u64,
                });
            }
            VdsoEntryKind::Known(KnownEntry::Syscall(_, sysno)) => {
                // The hook replaces a whole patch word at the `syscall`.
                assert!(size >= 8);
                let patch_len = entry.patch_len(8);
                unsafe {
                    core::ptr::write(symbol as *mut u8, 0x0f);
                    core::ptr::write((symbol + 1) as *mut u8, 0x05);
                    core::ptr::write((symbol + 2) as *mut u8, 0xc3);
                    core::ptr::write_bytes((symbol + 3) as *mut u8, 0x90, patch_len - 3);
                }
                syscall_sites.push(VdsoSyscallSite {
                    address: symbol as u64,
                    number: i64::from(sysno.id()),
                    mapping_start: start as u64,
                    mapping_len: len as u64,
                });
            }
            VdsoEntryKind::Known(KnownEntry::Enosys) | VdsoEntryKind::Unknown => {
                assert!(size >= bytes.len());
                let patch_len = entry.patch_len(bytes.len());
                unsafe {
                    core::ptr::copy_nonoverlapping(bytes.as_ptr(), symbol as *mut u8, bytes.len());
                    core::ptr::write_bytes(
                        (symbol + bytes.len()) as *mut u8,
                        0x90,
                        patch_len - bytes.len(),
                    );
                }
            }
        }
        debug!("patched vDSO entry point {}", entry.describe());
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

/// Test-only: a hook for the tracee with the given raw PID.
#[cfg(test)]
type VdsoHookForTest = (i32, Box<dyn Fn()>);

#[cfg(test)]
thread_local! {
    /// Test-only: runs in [`vdso_patch`] of the tracee with the given raw PID,
    /// once its vDSO is writable, before it is patched.
    static VDSO_WRITABLE_FOR_TEST: std::cell::RefCell<Option<VdsoHookForTest>> =
        const { std::cell::RefCell::new(None) };
}

/// Test-only: installs a [`VDSO_WRITABLE_FOR_TEST`] hook for the tracee with
/// raw PID `pid` until the returned guard is dropped, including when the
/// patch future is dropped part way.
#[cfg(test)]
pub(crate) fn vdso_writable_hook_for_test(pid: i32, hook: Box<dyn Fn()>) -> impl Drop {
    struct Uninstall;
    impl Drop for Uninstall {
        fn drop(&mut self) {
            VDSO_WRITABLE_FOR_TEST.with(|slot| slot.borrow_mut().take());
        }
    }
    VDSO_WRITABLE_FOR_TEST.with(|slot| *slot.borrow_mut() = Some((pid, hook)));
    Uninstall
}

/// patch VDSOs when enabled
///
/// `guest` must be in one of ptrace's stopped states.
pub async fn vdso_patch<G, T>(guest: &mut G, subscriptions: &Subscription) -> Result<(), Error>
where
    G: Guest<T>,
    T: Tool,
{
    let replacements = vdso_replacements(subscriptions)?;
    let Some(table) = vdso_table()? else {
        return Ok(());
    };
    if replacements.is_empty() {
        return Ok(());
    }
    log_native_entries(table, &replacements);
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
        // The plan was made from the tracer's own vDSO. A guest whose vDSO
        // differs (a 32-bit or foreign-ABI guest, say) would have its code
        // overwritten at the wrong offsets, so refuse it.
        let guest_len = vdso.address.1 - vdso.address.0;
        if guest_len != table.mapping_len {
            return Err(Error::from(std::io::Error::other(format!(
                "guest {} maps a {guest_len}-byte vDSO, but the patch plan is for the \
                 tracer's {}-byte vDSO",
                guest.pid(),
                table.mapping_len
            ))));
        }

        let mut memory = guest.memory();

        // Allow write access to the vdso memory page.
        guest
            .inject_with_retry(
                Mprotect::new()
                    .with_addr(AddrMut::from_raw(vdso.address.0 as usize))
                    .with_len(guest_len as usize)
                    .with_protection(
                        ProtFlags::PROT_READ | ProtFlags::PROT_WRITE | ProtFlags::PROT_EXEC,
                    ),
            )
            .await?;
        #[cfg(test)]
        VDSO_WRITABLE_FOR_TEST.with(|hook| {
            if let Some((pid, hook)) = hook.borrow().as_ref()
                && *pid == guest.pid().as_raw()
            {
                hook();
            }
        });

        for (entry, bytes) in replacements {
            let start = vdso.address.0 + entry.offset;
            assert!(bytes.len() <= entry.size);
            let patch_len = entry.patch_len(bytes.len());
            let rptr = AddrMut::from_raw(start as usize).ok_or(Errno::EFAULT)?;
            memory.write_exact(rptr, bytes)?;
            if patch_len > bytes.len() {
                let fill: Vec<u8> = std::iter::repeat_n(0x90u8, patch_len - bytes.len()).collect();
                memory.write_exact(unsafe { rptr.add(bytes.len()) }, &fill)?;
            }
            debug!(
                "{} patched {}@{:x}",
                guest.pid(),
                entry.names.join("/"),
                start
            );
        }

        guest
            .inject_with_retry(
                Mprotect::new()
                    .with_addr(AddrMut::from_raw(vdso.address.0 as usize))
                    .with_len(guest_len as usize)
                    .with_protection(ProtFlags::PROT_READ | ProtFlags::PROT_EXEC),
            )
            .await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

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

    fn host_table() -> &'static VdsoTable {
        VDSO_TABLE
            .as_ref()
            .expect("the host vDSO must classify cleanly")
            .as_ref()
            .expect("this host must map a vDSO")
    }

    /// The first syscall fast path in [`VDSO_SYMBOLS`], so the synthetic tests
    /// run unchanged on every architecture.
    fn a_known_syscall() -> (&'static str, &'static [u8], Sysno) {
        VDSO_SYMBOLS
            .iter()
            .find_map(|(name, entry)| match entry {
                KnownEntry::Syscall(stub, sysno) => Some((*name, *stub, *sysno)),
                KnownEntry::Enosys => None,
            })
            .unwrap()
    }

    fn export(name: &str, offset: u64, size: usize) -> VdsoExport {
        VdsoExport {
            name: name.to_owned(),
            offset,
            size,
            section_end: None,
        }
    }

    #[test]
    fn host_vdso_entry_points_are_all_classified() {
        let table = host_table();
        assert!(!table.entries.is_empty());
        // SAFETY: as in VDSO_TABLE; nothing in this test binary patches its
        // own vDSO.
        let exports = vdso_exports(
            procfs::process::Process::myself()
                .unwrap()
                .maps()
                .unwrap()
                .iter()
                .find(|map| map.pathname == procfs::process::MMapPath::Vdso)
                .map(|map| unsafe {
                    std::slice::from_raw_parts(
                        map.address.0 as *const u8,
                        (map.address.1 - map.address.0) as usize,
                    )
                })
                .unwrap(),
        )
        .unwrap();
        for export in &exports {
            assert_eq!(
                table
                    .entries
                    .iter()
                    .filter(|entry| entry.names.contains(&export.name))
                    .count(),
                1,
                "{} must be in exactly one entry point",
                export.name
            );
        }
        for entry in &table.entries {
            println!(
                "{}: {:?} ({} bytes)",
                entry.describe(),
                entry.kind,
                entry.size
            );
        }
    }

    /// The fail-closed property itself: an entry point this module has never
    /// heard of is replaced whenever the tool observes any syscall, and never
    /// silently left running the kernel's code.
    #[test]
    fn unknown_vdso_entry_point_is_stubbed() {
        let (known, stub, sysno) = a_known_syscall();
        let entries = classify_vdso_exports(&[
            export(known, 0x800, 100),
            export("future_alias", 0x800, 100),
            export("__vdso_future_call", 0x900, 40),
        ])
        .unwrap();

        assert_eq!(entries.len(), 2);
        let known_entry = &entries[0];
        assert_eq!(known_entry.names, vec![known, "future_alias"]);
        assert_eq!(known_entry.offset, 0x800);
        assert_eq!(known_entry.size, 112);
        assert_eq!(
            known_entry.kind,
            VdsoEntryKind::Known(KnownEntry::Syscall(stub, sysno))
        );

        let unknown = &entries[1];
        assert_eq!(unknown.names, vec!["__vdso_future_call"]);
        assert_eq!(unknown.offset, 0x900);
        assert_eq!(unknown.size, 48);
        assert_eq!(unknown.kind, VdsoEntryKind::Unknown);

        let unrelated: Subscription = [if sysno == Sysno::read {
            Sysno::write
        } else {
            Sysno::read
        }]
        .into_iter()
        .collect();
        for subscriptions in [Subscription::all(), unrelated.clone()] {
            assert_eq!(
                unknown.replacement(&subscriptions),
                Some(&vdso_syms::enosys[..]),
                "an unknown vDSO entry point must be replaced by the -ENOSYS stub",
            );
        }
        assert_eq!(unknown.replacement(&Subscription::none()), None);

        assert_eq!(
            known_entry.replacement(&[sysno].into_iter().collect()),
            Some(stub)
        );
        assert_eq!(known_entry.replacement(&Subscription::all()), Some(stub));
        assert_eq!(known_entry.replacement(&unrelated), None);
        assert_eq!(known_entry.replacement(&Subscription::none()), None);
    }

    #[test]
    fn unknown_name_listed_first_at_a_known_address_is_still_known() {
        let (known, stub, sysno) = a_known_syscall();
        let entries =
            classify_vdso_exports(&[export("alias_first", 0x800, 64), export(known, 0x800, 64)])
                .unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].names, vec![known, "alias_first"]);
        assert_eq!(
            entries[0].kind,
            VdsoEntryKind::Known(KnownEntry::Syscall(stub, sysno))
        );
    }

    #[test]
    fn last_entry_point_gets_its_aligned_size() {
        let entries = classify_vdso_exports(&[export("__vdso_future_call", 0x900, 9)]).unwrap();
        assert_eq!(entries[0].size, 16);
    }

    #[test]
    fn last_entry_point_stops_at_the_end_of_its_section() {
        let mut last = export("__vdso_future_call", 0x900, 9);
        last.section_end = Some(0x90c);
        let entries = classify_vdso_exports(&[last]).unwrap();
        assert_eq!(entries[0].size, 12);
    }

    #[test]
    fn entry_point_past_the_end_of_its_section_is_refused() {
        let mut last = export("__vdso_future_call", 0x900, 16);
        last.section_end = Some(0x90c);
        let error = classify_vdso_exports(&[last]).unwrap_err();
        assert!(error.contains("past the end of its section"), "{error}");
    }

    /// A stub overwrites the function, and alignment padding only where the
    /// stub is longer than the function.
    #[test]
    fn patch_covers_the_function_and_no_more_of_its_padding_than_the_stub() {
        let entries = classify_vdso_exports(&[
            export("__vdso_future_call", 0x900, 20),
            export("__vdso_other_call", 0x980, 4),
        ])
        .unwrap();
        assert_eq!((entries[0].size, entries[0].patch_len(8)), (32, 20));
        assert_eq!((entries[1].size, entries[1].patch_len(8)), (16, 8));
    }

    /// No host entry point's patch reaches past the end of its section; the
    /// host's last export, rounded up to 16, would.
    #[test]
    fn host_vdso_patches_stay_inside_their_sections() {
        let maps = procfs::process::Process::myself().unwrap().maps().unwrap();
        let vdso = maps
            .iter()
            .find(|map| map.pathname == procfs::process::MMapPath::Vdso)
            .unwrap();
        // SAFETY: as in VDSO_TABLE; nothing in this test binary patches its
        // own vDSO.
        let image = unsafe {
            std::slice::from_raw_parts(
                vdso.address.0 as *const u8,
                (vdso.address.1 - vdso.address.0) as usize,
            )
        };
        let elf = Elf::parse(image).unwrap();
        for entry in &host_table().entries {
            let section = elf
                .section_headers
                .iter()
                .find(|section| {
                    section.sh_addr <= entry.offset
                        && entry.offset < section.sh_addr + section.sh_size
                })
                .unwrap_or_else(|| panic!("no section contains {}", entry.describe()));
            assert!(
                entry.offset + entry.size as u64 <= section.sh_addr + section.sh_size,
                "{} may overwrite {} bytes, past the end of its section at {:#x}",
                entry.describe(),
                entry.size,
                section.sh_addr + section.sh_size
            );
        }
    }

    #[test]
    fn entry_point_too_small_for_its_stub_is_refused() {
        let (known, stub, _) = a_known_syscall();
        let error = classify_vdso_exports(&[
            export(known, 0x800, 1),
            export("next", 0x800 + stub.len() as u64 - 1, 16),
        ])
        .unwrap_err();
        assert!(error.contains("too few"), "{error}");

        let error = classify_vdso_exports(&[
            export("__vdso_future_call", 0x800, 1),
            export("next", 0x800 + vdso_syms::enosys.len() as u64 - 1, 16),
        ])
        .unwrap_err();
        assert!(error.contains("too few"), "{error}");
    }

    #[test]
    fn overlapping_entry_points_are_refused() {
        let error = classify_vdso_exports(&[
            export("__vdso_future_call", 0x800, 0x40),
            export("__vdso_other_call", 0x820, 0x40),
        ])
        .unwrap_err();
        assert!(error.contains("overlaps"), "{error}");
    }

    #[test]
    fn names_disagreeing_on_size_are_refused() {
        let error = classify_vdso_exports(&[
            export("__vdso_future_call", 0x800, 0x40),
            export("future_alias", 0x800, 0x20),
        ])
        .unwrap_err();
        assert!(error.contains("one address"), "{error}");
    }

    #[test]
    fn known_names_needing_different_stubs_at_one_address_are_refused() {
        let syscalls: Vec<&str> = VDSO_SYMBOLS
            .iter()
            .filter(|(_, entry)| matches!(entry, KnownEntry::Syscall(..)))
            .map(|(name, _)| *name)
            .collect();
        let error = classify_vdso_exports(&[
            export(syscalls[0], 0x800, 0x40),
            export(syscalls[1], 0x800, 0x40),
        ])
        .unwrap_err();
        assert!(error.contains("different stubs"), "{error}");
    }

    /// A subscription to no vDSO syscall requires patching exactly when the
    /// host vDSO exports an entry point with no syscall equivalent (on x86_64,
    /// `__vdso_sgx_enter_enclave`), because that entry point is stubbed for
    /// any tool that observes syscalls.
    #[test]
    fn patch_requirement_tracks_vdso_syscall_subscriptions() {
        assert!(!is_patch_required(&Subscription::none()));

        let table = host_table();
        let has_non_syscall_entry = table
            .entries
            .iter()
            .any(|entry| !matches!(entry.kind, VdsoEntryKind::Known(KnownEntry::Syscall(..))));
        assert_eq!(
            is_patch_required(&[Sysno::read].into_iter().collect()),
            has_non_syscall_entry,
        );

        for entry in &table.entries {
            if let VdsoEntryKind::Known(KnownEntry::Syscall(_, sysno)) = entry.kind {
                assert!(
                    is_patch_required(&[sysno].into_iter().collect()),
                    "a subscription for vDSO syscall {sysno:?} must require patching",
                );
            }
        }
    }

    #[test]
    fn each_subscription_selects_only_its_own_vdso_syscall_entries() {
        let table = host_table();
        for entry in &table.entries {
            let VdsoEntryKind::Known(KnownEntry::Syscall(_, subscribed_sysno)) = entry.kind else {
                continue;
            };
            let subscriptions = [subscribed_sysno].into_iter().collect();
            let selected = vdso_replacements(&subscriptions).unwrap();

            assert!(
                selected
                    .iter()
                    .any(|(selected, _)| selected.offset == entry.offset),
                "{subscribed_sysno:?} did not select its own entry point"
            );
            for (selected, _) in selected {
                match selected.kind {
                    VdsoEntryKind::Known(KnownEntry::Syscall(_, selected_sysno)) => assert_eq!(
                        selected_sysno,
                        subscribed_sysno,
                        "subscription for {subscribed_sysno:?} also selected {}",
                        selected.describe()
                    ),
                    VdsoEntryKind::Known(KnownEntry::Enosys) | VdsoEntryKind::Unknown => {}
                }
            }
        }
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn getcpu_only_leaves_other_vdso_syscall_entries_unselected() {
        let subscriptions = [Sysno::getcpu].into_iter().collect();
        let selected = vdso_replacements(&subscriptions)
            .unwrap()
            .into_iter()
            .filter(|(entry, _)| {
                matches!(entry.kind, VdsoEntryKind::Known(KnownEntry::Syscall(..)))
            })
            .map(|(entry, _)| entry.names[0].as_str())
            .collect::<Vec<_>>();

        assert_eq!(selected, vec!["__vdso_getcpu"]);
    }
}
