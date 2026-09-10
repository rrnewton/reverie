//! Provides APIs to disable VDSOs at runtime.
use std::collections::BTreeMap;
use std::sync::LazyLock;

use goblin::elf::Elf;
use nix::unistd;

use crate::Errno;
use crate::Error;
use crate::Subscription;
use crate::syscalls::Sysno;

/// Raw arguments to the five-argument Linux vDSO getrandom function.
///
/// This type only preserves the function arguments. A backend that recognizes
/// the exact guest vDSO ABI owns classifying them as an entropy request or a
/// parameters query.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct VdsoGetrandomArgs {
    buffer: usize,
    count: usize,
    flags: u32,
    opaque_state: usize,
    opaque_len: usize,
}

impl VdsoGetrandomArgs {
    /// Preserves the five raw C arguments without dereferencing either address.
    pub const fn from_raw(
        buffer: usize,
        count: usize,
        flags: u32,
        opaque_state: usize,
        opaque_len: usize,
    ) -> Self {
        Self {
            buffer,
            count,
            flags,
            opaque_state,
            opaque_len,
        }
    }

    /// Returns the output buffer, or `None` when its raw address is null.
    pub fn buffer(&self) -> Option<crate::syscalls::AddrMut<'_, u8>> {
        crate::syscalls::AddrMut::from_raw(self.buffer)
    }

    /// Returns the requested output byte count.
    pub const fn count(&self) -> usize {
        self.count
    }

    /// Returns the 32-bit vDSO function flags.
    pub const fn flags(&self) -> u32 {
        self.flags
    }

    /// Returns the opaque state buffer, or `None` when its raw address is null.
    pub fn opaque_state(&self) -> Option<crate::syscalls::AddrMut<'_, u8>> {
        crate::syscalls::AddrMut::from_raw(self.opaque_state)
    }

    /// Returns the opaque state buffer length.
    pub const fn opaque_len(&self) -> usize {
        self.opaque_len
    }
}

/// A classified vDSO getrandom request.
///
/// Only a backend that has qualified the exact guest vDSO ABI may select
/// `ParametersQuery`; a zero byte count alone does not imply that variant.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum VdsoGetrandomRequest {
    /// A regular entropy request.
    Entropy(VdsoGetrandomArgs),
    /// An exact ABI parameters-query sentinel request.
    ParametersQuery(VdsoGetrandomArgs),
}

impl VdsoGetrandomRequest {
    /// Returns all five preserved C arguments.
    pub const fn args(&self) -> &VdsoGetrandomArgs {
        match self {
            Self::Entropy(args) | Self::ParametersQuery(args) => args,
        }
    }
}

/// A typed vDSO function event delivered to a Reverie Tool.
///
/// No backend delivers this event yet; the type is an inactive extension seam.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum VdsoEvent {
    /// A five-argument vDSO getrandom call.
    Getrandom(VdsoGetrandomRequest),
}

/// A coherent snapshot of a Tool's virtual RNG input domain.
///
/// These are semantic values, not a kernel memory layout, host readiness claim,
/// opaque-state image, or permission to resume guest execution.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct VdsoRngSnapshot {
    /// Whether the Tool's initialized entropy source is available.
    pub ready: bool,
    /// Cache-validity generation within the Tool's declared RNG domain.
    pub generation: u64,
}

/// A typed refusal for a vDSO event with no Tool/backend implementation.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum UnsupportedVdsoEvent {
    /// The five-argument vDSO getrandom event is unsupported.
    #[error("vDSO getrandom events are unsupported")]
    Getrandom,
    /// The Tool does not provide virtual RNG input snapshots.
    #[error("vDSO RNG snapshots are unsupported")]
    RngSnapshot,
}

impl From<UnsupportedVdsoEvent> for Error {
    fn from(error: UnsupportedVdsoEvent) -> Self {
        Self::Tool(anyhow::Error::new(error))
    }
}

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
pub type VdsoPatch = (u64, usize, &'static [u8], Sysno);

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
pub fn subscribed_vdso_patches(
    subscriptions: &Subscription,
) -> impl Iterator<Item = (&'static str, &'static VdsoPatch)> + '_ {
    VDSO_PATCH_INFO
        .iter()
        .filter(|(_, (_, _, _, sysno))| is_symbol_patch_required(subscriptions, *sysno))
        .map(|(name, patch)| (*name, patch))
}

/// Whether any available vDSO symbol is selected by the subscription.
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rng_snapshot_preserves_availability_and_full_generation() {
        let snapshot = VdsoRngSnapshot {
            ready: false,
            generation: u64::MAX,
        };
        assert!(!snapshot.ready);
        assert_eq!(snapshot.generation, u64::MAX);
        assert_ne!(
            snapshot,
            VdsoRngSnapshot {
                ready: true,
                ..snapshot
            }
        );
    }

    #[test]
    fn rng_snapshot_default_is_a_typed_control_plane_refusal() {
        let error = <() as crate::Tool>::vdso_rng_snapshot(&(), &()).unwrap_err();
        let Error::Tool(error) = error else {
            panic!("snapshot refusal was not carried by Error::Tool");
        };
        assert_eq!(
            error.downcast_ref::<UnsupportedVdsoEvent>(),
            Some(&UnsupportedVdsoEvent::RngSnapshot)
        );
        assert!(
            Error::from(UnsupportedVdsoEvent::RngSnapshot)
                .into_errno()
                .is_err()
        );
    }

    #[test]
    fn getrandom_arguments_preserve_words_widths_and_nullability() {
        let args = VdsoGetrandomArgs::from_raw(
            0x1234_5678,
            usize::MAX,
            0xfedc_ba98,
            0x8765_4321,
            usize::MAX - 1,
        );

        assert_eq!(args.buffer().unwrap().as_raw(), 0x1234_5678);
        assert_eq!(args.count(), usize::MAX);
        assert_eq!(args.flags(), 0xfedc_ba98);
        assert_eq!(args.opaque_state().unwrap().as_raw(), 0x8765_4321);
        assert_eq!(args.opaque_len(), usize::MAX - 1);

        let null = VdsoGetrandomArgs::from_raw(0, 7, 3, 0, 11);
        assert!(null.buffer().is_none());
        assert!(null.opaque_state().is_none());
        assert_eq!(null.count(), 7);
        assert_eq!(null.flags(), 3);
        assert_eq!(null.opaque_len(), 11);
    }

    #[test]
    fn getrandom_request_kinds_retain_all_five_arguments() {
        let args = VdsoGetrandomArgs::from_raw(1, 2, 3, 4, 5);

        assert_eq!(*VdsoGetrandomRequest::Entropy(args).args(), args);
        assert_eq!(*VdsoGetrandomRequest::ParametersQuery(args).args(), args);
        assert_ne!(
            VdsoGetrandomRequest::Entropy(args),
            VdsoGetrandomRequest::ParametersQuery(args)
        );
    }

    #[test]
    fn unsupported_getrandom_remains_a_typed_tool_error() {
        let error = Error::from(UnsupportedVdsoEvent::Getrandom);
        let Error::Tool(error) = error else {
            panic!("unsupported vDSO event was not carried by Error::Tool");
        };

        assert!(matches!(
            error.downcast_ref::<UnsupportedVdsoEvent>(),
            Some(UnsupportedVdsoEvent::Getrandom)
        ));
    }

    #[test]
    fn signed_function_returns_remain_normal_results() {
        let returned: Result<i64, Error> = Ok(-i64::from(libc::EINVAL));
        assert_eq!(returned.ok(), Some(-i64::from(libc::EINVAL)));
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
