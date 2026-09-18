//! Rapid first-byte activation for exact x86-64 instruction puns.
//!
//! Registration performs decoding, relocation, and exact-address executable
//! allocation once. Thereafter activation writes only the instruction's first
//! byte: the inactive opcode or E9. The four rel32 bytes are the unchanged
//! original instruction bytes, so both states are complete instruction streams.

use core::fmt;
use core::sync::atomic::AtomicU8;
use core::sync::atomic::Ordering;

use iced_x86::Instruction;
use iced_x86::OpKind;

use crate::patcher::NEAR_JUMP_BYTES;
use crate::patcher::NEAR_JUMP_OPCODE;
use crate::scanner::InstructionScanner;
use crate::scanner::ScanError;
use crate::scanner::ScanResult;
use crate::trampoline::ExecutableTrampoline;
use crate::trampoline::HookCallback;
use crate::trampoline::TrampolineError;
use crate::trampoline::TrampolinePlan;

/// Registration-time failures for a rapid first-byte probe.
#[derive(Debug)]
pub enum RapidToggleError {
    /// The code region cannot be decoded completely.
    InvalidCodeRegion {
        /// Scanner failure.
        source: ScanError,
    },
    /// The supplied scan does not describe the current code region.
    RegionMismatch {
        /// Requested executable address.
        address: u64,
    },
    /// The five-byte pun window is outside the supplied region.
    PatchWindowOutOfRange {
        /// Requested executable address.
        address: u64,
    },
    /// The inactive instruction already begins with E9.
    AlreadyNearJump {
        /// Requested executable address.
        address: u64,
    },
    /// The original tail bytes imply an unrepresentable destination.
    TargetOutOfRange {
        /// Requested executable address.
        address: u64,
        /// Signed displacement encoded by original bytes 1 through 4.
        displacement: i32,
    },
    /// The writable opcode alias is null.
    NullWritableAddress,
    /// Writable and executable aliases have different cache-line offsets.
    AliasAlignmentMismatch,
    /// Another instruction begins inside the five-byte pun window.
    PatchWindowContainsInstructionHead {
        /// Requested executable address.
        address: u64,
        /// Interior instruction head discovered by iced-x86.
        instruction_head: u64,
    },
    /// A decoded direct branch targets the interior of the pun window.
    PatchWindowContainsBranchTarget {
        /// Requested executable address.
        address: u64,
        /// Address of the branch instruction.
        branch_address: u64,
        /// Interior branch target that would become displacement data.
        target_address: u64,
    },
    /// The writable alias does not contain the planned inactive opcode.
    ExpectedOpcodeMismatch {
        /// Planned inactive opcode.
        expected: u8,
        /// Byte observed through the writable alias.
        observed: u8,
    },
    /// The writable alias does not contain the planned five-byte pun window.
    ExpectedWindowMismatch {
        /// Complete five-byte window captured during planning.
        expected: [u8; NEAR_JUMP_BYTES],
        /// Complete five-byte window observed during installation.
        observed: [u8; NEAR_JUMP_BYTES],
    },
    /// Another live patch owns part of the five-byte pun window.
    OverlappingPatchSite,
    /// Registering process-wide ownership raced an active writer.
    ReservationContended,
    /// Installing the process-wide SIGTRAP router failed.
    SignalHandlerInstall {
        /// Operating-system error number.
        errno: i32,
    },
    /// Planning or allocating the exact trampoline failed.
    Trampoline(TrampolineError),
    /// Rapid probes are unavailable on this target.
    UnsupportedPlatform,
}

impl fmt::Display for RapidToggleError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidCodeRegion { source } => {
                write!(formatter, "invalid code region: {source}")
            }
            Self::RegionMismatch { address } => {
                write!(formatter, "scan and current code disagree at {address:#x}")
            }
            Self::PatchWindowOutOfRange { address } => {
                write!(
                    formatter,
                    "five-byte pun at {address:#x} is outside the code region"
                )
            }
            Self::AlreadyNearJump { address } => {
                write!(
                    formatter,
                    "instruction at {address:#x} already begins with E9"
                )
            }
            Self::TargetOutOfRange {
                address,
                displacement,
            } => write!(
                formatter,
                "tail displacement {displacement} at {address:#x} has no representable target"
            ),
            Self::NullWritableAddress => formatter.write_str("writable opcode address is null"),
            Self::AliasAlignmentMismatch => {
                formatter.write_str("writable and executable aliases have different line offsets")
            }
            Self::PatchWindowContainsInstructionHead {
                address,
                instruction_head,
            } => write!(
                formatter,
                "five-byte pun at {address:#x} contains instruction head {instruction_head:#x}"
            ),
            Self::PatchWindowContainsBranchTarget {
                address,
                branch_address,
                target_address,
            } => write!(
                formatter,
                "direct branch at {branch_address:#x} targets {target_address:#x} inside five-byte pun at {address:#x}"
            ),
            Self::ExpectedOpcodeMismatch { expected, observed } => write!(
                formatter,
                "writable opcode is {observed:#04x}, expected {expected:#04x}"
            ),
            Self::ExpectedWindowMismatch { expected, observed } => write!(
                formatter,
                "writable five-byte pun window is {observed:02x?}, expected {expected:02x?}"
            ),
            Self::OverlappingPatchSite => {
                formatter.write_str("another live patch owns the five-byte pun window")
            }
            Self::ReservationContended => {
                formatter.write_str("patch-site reservation is contended")
            }
            Self::SignalHandlerInstall { errno } => {
                write!(formatter, "failed to install SIGTRAP router: errno {errno}")
            }
            Self::Trampoline(source) => write!(formatter, "trampoline setup failed: {source}"),
            Self::UnsupportedPlatform => {
                formatter.write_str("rapid probes require Linux on x86-64")
            }
        }
    }
}

impl std::error::Error for RapidToggleError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::InvalidCodeRegion { source } => Some(source),
            Self::Trampoline(source) => Some(source),
            _ => None,
        }
    }
}

impl From<TrampolineError> for RapidToggleError {
    fn from(source: TrampolineError) -> Self {
        Self::Trampoline(source)
    }
}

/// Immutable proof that one instruction supports first-byte toggling.
#[derive(Clone, Debug)]
pub struct RapidTogglePlan {
    trampoline: TrampolinePlan,
    execute_address: u64,
    target_address: u64,
    original_window: [u8; NEAR_JUMP_BYTES],
    inactive_opcode: u8,
    cache_line_bytes: usize,
}

impl RapidTogglePlan {
    /// Plans a rapid probe from current code and an M1 scan.
    ///
    /// Original bytes 1 through 4 are interpreted as a signed rel32. The
    /// trampoline must be mapped at that exact implied address, leaving those
    /// bytes unchanged in both inactive and active states.
    ///
    /// Planning rejects instruction heads and decoded direct branch targets in
    /// bytes 1 through 4. The caller must provide a code-only region containing
    /// every direct branch whose target can enter the window and must separately
    /// exclude indirect or external entries that a linear scan cannot discover.
    pub fn from_scan(
        scanner: &InstructionScanner,
        scan: &ScanResult,
        code: &[u8],
        region_base: u64,
        execute_address: u64,
        hook: HookCallback,
    ) -> Result<Self, RapidToggleError> {
        let verified = scanner
            .scan(code, region_base)
            .map_err(|source| RapidToggleError::InvalidCodeRegion { source })?;
        if !scan.matches_snapshot(&verified) {
            return Err(RapidToggleError::RegionMismatch {
                address: execute_address,
            });
        }
        let site = verified
            .site(execute_address)
            .ok_or(RapidToggleError::RegionMismatch {
                address: execute_address,
            })?;
        let relative =
            execute_address
                .checked_sub(region_base)
                .ok_or(RapidToggleError::RegionMismatch {
                    address: execute_address,
                })?;
        let offset = usize::try_from(relative).map_err(|_| RapidToggleError::RegionMismatch {
            address: execute_address,
        })?;
        if offset != site.offset() {
            return Err(RapidToggleError::RegionMismatch {
                address: execute_address,
            });
        }
        let end =
            offset
                .checked_add(NEAR_JUMP_BYTES)
                .ok_or(RapidToggleError::PatchWindowOutOfRange {
                    address: execute_address,
                })?;
        let window = code
            .get(offset..end)
            .ok_or(RapidToggleError::PatchWindowOutOfRange {
                address: execute_address,
            })?;
        let original_window: [u8; NEAR_JUMP_BYTES] =
            window
                .try_into()
                .map_err(|_| RapidToggleError::PatchWindowOutOfRange {
                    address: execute_address,
                })?;
        let interior_start =
            execute_address
                .checked_add(1)
                .ok_or(RapidToggleError::PatchWindowOutOfRange {
                    address: execute_address,
                })?;
        let window_end = execute_address.checked_add(NEAR_JUMP_BYTES as u64).ok_or(
            RapidToggleError::PatchWindowOutOfRange {
                address: execute_address,
            },
        )?;
        if let Some(instruction_head) = verified.sites().range(interior_start..window_end).next() {
            return Err(RapidToggleError::PatchWindowContainsInstructionHead {
                address: execute_address,
                instruction_head: *instruction_head.0,
            });
        }
        for scanned in verified.instructions() {
            let Some(target_address) = direct_near_branch_target(scanned.instruction()) else {
                continue;
            };
            if (interior_start..window_end).contains(&target_address) {
                return Err(RapidToggleError::PatchWindowContainsBranchTarget {
                    address: execute_address,
                    branch_address: scanned.address(),
                    target_address,
                });
            }
        }

        let inactive_opcode = original_window[0];
        if inactive_opcode == NEAR_JUMP_OPCODE {
            return Err(RapidToggleError::AlreadyNearJump {
                address: execute_address,
            });
        }
        let displacement = i32::from_le_bytes(original_window[1..].try_into().map_err(|_| {
            RapidToggleError::PatchWindowOutOfRange {
                address: execute_address,
            }
        })?);
        let next_ip = window_end;
        let target = i128::from(next_ip) + i128::from(displacement);
        let target_address =
            u64::try_from(target).map_err(|_| RapidToggleError::TargetOutOfRange {
                address: execute_address,
                displacement,
            })?;
        let trampoline = TrampolinePlan::from_scan(&verified, execute_address, hook)?;

        Ok(Self {
            trampoline,
            execute_address,
            target_address,
            original_window,
            inactive_opcode,
            cache_line_bytes: scanner.cache_line_size().get(),
        })
    }

    /// Returns the executable opcode address.
    pub const fn execute_address(&self) -> u64 {
        self.execute_address
    }

    /// Returns the exact trampoline address encoded by original tail bytes.
    pub const fn target_address(&self) -> u64 {
        self.target_address
    }

    /// Returns the opcode restored by deactivation.
    pub const fn inactive_opcode(&self) -> u8 {
        self.inactive_opcode
    }

    /// Returns the immutable five-byte pun window captured during planning.
    pub const fn original_window(&self) -> [u8; NEAR_JUMP_BYTES] {
        self.original_window
    }
}

fn direct_near_branch_target(instruction: &Instruction) -> Option<u64> {
    match instruction.op0_kind() {
        OpKind::NearBranch16 | OpKind::NearBranch32 | OpKind::NearBranch64 => {
            Some(instruction.near_branch_target())
        }
        _ => None,
    }
}

/// Installed probe whose hot path changes one atomic opcode byte.
pub struct RapidProbe {
    plan: RapidTogglePlan,
    trampoline: ExecutableTrampoline,
    opcode: &'static AtomicU8,
    _reservation: &'static crate::trap::TrapSite,
}

impl RapidProbe {
    /// Allocates the exact trampoline and binds the writable opcode alias.
    ///
    /// The probe begins inactive because registration does not change code.
    ///
    /// # Safety
    ///
    /// writable_address must alias the complete five-byte executable window at
    /// plan.execute_address, share its cache-line offset, permit atomic stores
    /// to its first byte, and remain mapped for the process lifetime. During
    /// installation, the caller must exclude unregistered writers; registered
    /// overlaps are rejected before the window is read. After installation, no
    /// unregistered writer may modify any byte in that window. The executable
    /// site and hook callback must also remain valid for the process lifetime.
    pub unsafe fn install(
        plan: RapidTogglePlan,
        writable_address: *mut u8,
    ) -> Result<Self, RapidToggleError> {
        #[cfg(not(all(target_os = "linux", target_arch = "x86_64")))]
        {
            let _ = (plan, writable_address);
            return Err(RapidToggleError::UnsupportedPlatform);
        }

        #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
        {
            if writable_address.is_null() {
                return Err(RapidToggleError::NullWritableAddress);
            }
            let execute_address = usize::try_from(plan.execute_address).map_err(|_| {
                RapidToggleError::TargetOutOfRange {
                    address: plan.execute_address,
                    displacement: 0,
                }
            })?;
            if execute_address % plan.cache_line_bytes
                != writable_address as usize % plan.cache_line_bytes
            {
                return Err(RapidToggleError::AliasAlignmentMismatch);
            }
            let reservation_end = execute_address.checked_add(NEAR_JUMP_BYTES).ok_or(
                RapidToggleError::TargetOutOfRange {
                    address: plan.execute_address,
                    displacement: 0,
                },
            )?;
            let pending =
                crate::trap::reserve(execute_address, execute_address, reservation_end, 0)
                    .map_err(map_trap_error)?;
            // SAFETY: the caller supplies a live, atomically addressable byte.
            let opcode = unsafe { &*writable_address.cast::<AtomicU8>() };
            let mut observed_window = [0_u8; NEAR_JUMP_BYTES];
            observed_window[0] = opcode.load(Ordering::Acquire);
            for (offset, observed) in observed_window.iter_mut().enumerate().skip(1) {
                // SAFETY: the caller guarantees the complete five-byte alias
                // remains mapped and is not concurrently modified.
                *observed = unsafe { writable_address.add(offset).read_volatile() };
            }
            if observed_window[0] != plan.inactive_opcode {
                return Err(RapidToggleError::ExpectedOpcodeMismatch {
                    expected: plan.inactive_opcode,
                    observed: observed_window[0],
                });
            }
            if observed_window != plan.original_window {
                return Err(RapidToggleError::ExpectedWindowMismatch {
                    expected: plan.original_window,
                    observed: observed_window,
                });
            }

            let trampoline =
                ExecutableTrampoline::allocate_at(&plan.trampoline, plan.target_address)?;
            let reservation = match pending.commit() {
                Ok(reservation) => reservation,
                Err(error) => {
                    trampoline.discard();
                    return Err(map_trap_error(error));
                }
            };
            trampoline.publish_program_counter_mappings();

            Ok(Self {
                plan,
                trampoline,
                opcode,
                _reservation: reservation,
            })
        }
    }

    /// Publishes the requested state with one atomic byte store.
    ///
    /// Other cores may observe the new instruction asynchronously, matching
    /// the eventual-toggle semantics of the PLDI rapid-probe protocol.
    #[inline(always)]
    pub fn set_enabled(&self, enabled: bool) {
        let opcode = if enabled {
            NEAR_JUMP_OPCODE
        } else {
            self.plan.inactive_opcode
        };
        self.opcode.store(opcode, Ordering::Release);
    }

    /// Activates hook dispatch with one atomic byte store.
    #[inline(always)]
    pub fn enable(&self) {
        self.set_enabled(true);
    }

    /// Restores the original instruction opcode with one atomic byte store.
    #[inline(always)]
    pub fn disable(&self) {
        self.set_enabled(false);
    }

    /// Returns whether this core currently observes the active opcode.
    #[inline(always)]
    pub fn is_enabled(&self) -> bool {
        self.opcode.load(Ordering::Acquire) == NEAR_JUMP_OPCODE
    }

    /// Returns the immutable pun plan.
    pub const fn plan(&self) -> &RapidTogglePlan {
        &self.plan
    }

    /// Returns the exact executable trampoline.
    pub const fn trampoline(&self) -> &ExecutableTrampoline {
        &self.trampoline
    }
}

fn map_trap_error(error: crate::trap::TrapError) -> RapidToggleError {
    match error {
        crate::trap::TrapError::Overlap => RapidToggleError::OverlappingPatchSite,
        crate::trap::TrapError::Contended => RapidToggleError::ReservationContended,
        crate::trap::TrapError::Install(errno) => RapidToggleError::SignalHandlerInstall { errno },
        crate::trap::TrapError::Unsupported => RapidToggleError::UnsupportedPlatform,
    }
}

#[cfg(test)]
mod tests {
    use super::RapidToggleError;
    use super::RapidTogglePlan;
    use crate::scanner::InstructionScanner;
    use crate::trampoline::HookContext;

    unsafe extern "C" fn noop_hook(_context: *mut HookContext) {}

    #[test]
    fn original_tail_bytes_determine_the_exact_target() {
        let scanner = InstructionScanner::default();
        let base = 0x10_0000_u64;
        let displacement = 0x1234_5678_i32;
        let mut code = [0_u8; 5];
        code[0] = 0xB8;
        code[1..].copy_from_slice(&displacement.to_le_bytes());
        let scan = scanner.scan(&code, base).unwrap();
        let plan =
            RapidTogglePlan::from_scan(&scanner, &scan, &code, base, base, noop_hook).unwrap();

        assert_eq!(plan.inactive_opcode(), 0xB8);
        assert_eq!(
            plan.target_address(),
            (i128::from(base + 5) + i128::from(displacement)) as u64
        );
        assert_eq!(plan.original_window(), code);
    }

    #[test]
    fn rejects_an_existing_near_jump_opcode() {
        let scanner = InstructionScanner::default();
        let base = 0x20_0000_u64;
        let code = [0xE9, 0, 0, 0, 0];
        let scan = scanner.scan(&code, base).unwrap();
        let error =
            RapidTogglePlan::from_scan(&scanner, &scan, &code, base, base, noop_hook).unwrap_err();

        assert!(matches!(
            error,
            RapidToggleError::AlreadyNearJump { address } if address == base
        ));
    }

    #[test]
    fn rejects_stale_scan_even_when_instruction_lengths_match() {
        let scanner = InstructionScanner::default();
        let base = 0x30_0000_u64;
        let original = [0xB8, 1, 0, 0, 0];
        let changed = [0xB8, 2, 0, 0, 0];
        let stale = scanner.scan(&original, base).unwrap();
        let error = RapidTogglePlan::from_scan(&scanner, &stale, &changed, base, base, noop_hook)
            .unwrap_err();

        assert!(matches!(
            error,
            RapidToggleError::RegionMismatch { address } if address == base
        ));
    }

    #[test]
    fn rejects_semantically_equivalent_byte_staleness() {
        let scanner = InstructionScanner::default();
        let base = 0x31_0000_u64;
        let original = [0x40, 0x05, 1, 0, 0, 0];
        let changed = [0x42, 0x05, 1, 0, 0, 0];
        let stale = scanner.scan(&original, base).unwrap();
        let error = RapidTogglePlan::from_scan(&scanner, &stale, &changed, base, base, noop_hook)
            .unwrap_err();

        assert!(matches!(
            error,

            RapidToggleError::RegionMismatch { address } if address == base
        ));
    }
    #[test]
    fn rejects_an_instruction_head_inside_the_pun_window() {
        let scanner = InstructionScanner::default();
        let base = 0x40_0000_u64;
        let code = [0x90; 5];
        let scan = scanner.scan(&code, base).unwrap();
        let error =
            RapidTogglePlan::from_scan(&scanner, &scan, &code, base, base, noop_hook).unwrap_err();

        assert!(matches!(
            error,
            RapidToggleError::PatchWindowContainsInstructionHead {
                address,
                instruction_head,
            } if address == base && instruction_head == base + 1
        ));
    }

    #[test]
    fn rejects_a_direct_branch_target_inside_the_pun_window() {
        let scanner = InstructionScanner::default();
        let base = 0x50_0000_u64;
        // mov eax, 0; jmp base+1
        let code = [0xB8, 0, 0, 0, 0, 0xEB, 0xFA];
        let scan = scanner.scan(&code, base).unwrap();
        let error =
            RapidTogglePlan::from_scan(&scanner, &scan, &code, base, base, noop_hook).unwrap_err();

        assert!(matches!(
            error,
            RapidToggleError::PatchWindowContainsBranchTarget {
                address,
                branch_address,
                target_address,
            } if address == base
                && branch_address == base + 5
                && target_address == base + 1
        ));
    }

    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    mod live {
        use core::sync::atomic::AtomicBool;
        use core::sync::atomic::AtomicU8;
        use core::sync::atomic::AtomicU64;
        use core::sync::atomic::Ordering;
        use std::sync::Arc;
        use std::sync::Barrier;
        use std::sync::Mutex;
        use std::sync::MutexGuard;
        use std::thread;
        use std::time::Duration;
        use std::time::Instant;

        use super::HookContext;
        use super::InstructionScanner;
        use super::RapidToggleError;
        use super::RapidTogglePlan;
        use crate::patcher::NEAR_JUMP_BYTES;
        use crate::patcher::NEAR_JUMP_OPCODE;
        use crate::rapid::RapidProbe;
        use crate::trampoline::TrampolineError;

        const PAGE_BYTES: usize = 4096;
        const SITE_OFFSET: usize = 60;
        const TARGET_OFFSET: usize = 128;
        static TEST_LOCK: Mutex<()> = Mutex::new(());
        static CALLBACKS: AtomicU64 = AtomicU64::new(0);

        unsafe extern "C" fn record_hook(_context: *mut HookContext) {
            CALLBACKS.fetch_add(1, Ordering::Relaxed);
        }

        fn serial_guard() -> MutexGuard<'static, ()> {
            TEST_LOCK
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
        }

        struct DualMapping {
            writable: *mut u8,
            executable: *mut u8,
        }

        impl DualMapping {
            fn new() -> Self {
                let name = b"liteinst2-rapid-test\0";
                // SAFETY: valid NUL-terminated name and flags.
                let fd = unsafe {
                    libc::syscall(
                        libc::SYS_memfd_create,
                        name.as_ptr().cast::<libc::c_char>(),
                        libc::MFD_CLOEXEC,
                    ) as libc::c_int
                };
                assert!(fd >= 0, "memfd_create failed");
                // SAFETY: fd is valid and PAGE_BYTES is representable.
                assert_eq!(unsafe { libc::ftruncate(fd, PAGE_BYTES as libc::off_t) }, 0);
                // SAFETY: maps a shared writable, non-executable alias.
                let writable = unsafe {
                    libc::mmap(
                        core::ptr::null_mut(),
                        PAGE_BYTES,
                        libc::PROT_READ | libc::PROT_WRITE,
                        libc::MAP_SHARED,
                        fd,
                        0,
                    )
                };
                assert_ne!(writable, libc::MAP_FAILED);
                // SAFETY: maps the same bytes through a read-execute alias.
                let executable = unsafe {
                    libc::mmap(
                        core::ptr::null_mut(),
                        PAGE_BYTES,
                        libc::PROT_READ | libc::PROT_EXEC,
                        libc::MAP_SHARED,
                        fd,
                        0,
                    )
                };
                assert_ne!(executable, libc::MAP_FAILED);
                // SAFETY: both VMAs retain the backing object.
                assert_eq!(unsafe { libc::close(fd) }, 0);
                Self {
                    writable: writable.cast(),
                    executable: executable.cast(),
                }
            }

            fn executable_address(&self, offset: usize) -> u64 {
                // SAFETY: fixtures keep offsets within the page.
                unsafe { self.executable.add(offset) as usize as u64 }
            }

            fn writable_address(&self, offset: usize) -> *mut u8 {
                // SAFETY: fixtures keep offsets within the page.
                unsafe { self.writable.add(offset) }
            }

            fn write(&self, offset: usize, bytes: &[u8]) {
                assert!(offset + bytes.len() <= PAGE_BYTES);
                // SAFETY: writable contains the complete fixture range.
                unsafe {
                    core::ptr::copy_nonoverlapping(
                        bytes.as_ptr(),
                        self.writable.add(offset),
                        bytes.len(),
                    );
                }
            }

            fn bytes(&self, offset: usize, len: usize) -> &[u8] {
                // SAFETY: process-lifetime mapping contains the requested bytes.
                unsafe { core::slice::from_raw_parts(self.writable.add(offset), len) }
            }
        }

        struct Placeholder {
            address: usize,
        }

        impl Placeholder {
            fn reserve_near(next_ip: u64) -> Self {
                let page_base = next_ip as usize & !(PAGE_BYTES - 1);
                for step in 1..=2047_usize {
                    let distance = step * 1024 * 1024;
                    for candidate in [
                        page_base.checked_add(distance),
                        page_base.checked_sub(distance),
                    ]
                    .into_iter()
                    .flatten()
                    {
                        if candidate < PAGE_BYTES {
                            continue;
                        }
                        let displacement = candidate as i128 - i128::from(next_ip);
                        if i32::try_from(displacement).is_err() {
                            continue;
                        }
                        // SAFETY: MAP_FIXED_NOREPLACE never replaces a live VMA.
                        let mapping = unsafe {
                            libc::mmap(
                                candidate as *mut libc::c_void,
                                PAGE_BYTES,
                                libc::PROT_NONE,
                                libc::MAP_PRIVATE | libc::MAP_ANONYMOUS | libc::MAP_FIXED_NOREPLACE,
                                -1,
                                0,
                            )
                        };
                        if mapping == libc::MAP_FAILED {
                            continue;
                        }
                        if mapping as usize == candidate {
                            return Self { address: candidate };
                        }
                        // SAFETY: mapping is the VMA just returned by mmap.
                        unsafe { libc::munmap(mapping, PAGE_BYTES) };
                    }
                }
                panic!("no rel32 placeholder page available");
            }

            fn release(self) -> usize {
                // SAFETY: this object exclusively owns the placeholder VMA.
                assert_eq!(
                    unsafe { libc::munmap(self.address as *mut libc::c_void, PAGE_BYTES) },
                    0
                );
                self.address
            }
        }

        fn plan_fixture(mapping: &DualMapping) -> (RapidTogglePlan, Placeholder, Vec<u8>, u32) {
            let execute_address = mapping.executable_address(SITE_OFFSET);
            let placeholder = Placeholder::reserve_near(execute_address + 5);
            let target_address = placeholder.address + TARGET_OFFSET;
            let displacement =
                i32::try_from(target_address as i128 - i128::from(execute_address + 5)).unwrap();
            let mut code = vec![0xB8];
            code.extend_from_slice(&displacement.to_le_bytes());
            code.extend_from_slice(&[0xC3, 0x90, 0x90]);
            mapping.write(SITE_OFFSET, &code);
            let scanner = InstructionScanner::default();
            let scan = scanner.scan(&code, execute_address).unwrap();
            let plan = RapidTogglePlan::from_scan(
                &scanner,
                &scan,
                &code,
                execute_address,
                execute_address,
                record_hook,
            )
            .unwrap();
            assert_eq!(plan.target_address(), target_address as u64);
            (plan, placeholder, code, displacement as u32)
        }

        fn install_fixture(mapping: &DualMapping) -> (RapidProbe, Vec<u8>, u32) {
            let (plan, placeholder, code, expected) = plan_fixture(mapping);
            placeholder.release();
            // SAFETY: test mappings and callback intentionally live for process lifetime.
            let probe = unsafe {
                RapidProbe::install(plan, mapping.writable_address(SITE_OFFSET)).unwrap()
            };
            (probe, code, expected)
        }

        fn permissions(address: u64) -> Option<String> {
            std::fs::read_to_string("/proc/self/maps")
                .unwrap()
                .lines()
                .find_map(|line| {
                    let mut fields = line.split_whitespace();
                    let range = fields.next()?;
                    let permissions = fields.next()?;
                    let (start, end) = range.split_once('-')?;
                    let start = u64::from_str_radix(start, 16).ok()?;
                    let end = u64::from_str_radix(end, 16).ok()?;
                    (start <= address && address < end).then(|| permissions.to_owned())
                })
        }

        #[test]
        fn toggling_changes_only_the_opcode_and_preserves_semantics() {
            let _guard = serial_guard();
            CALLBACKS.store(0, Ordering::Relaxed);
            let mapping = DualMapping::new();
            let (probe, code, expected) = install_fixture(&mapping);
            let execute_address = mapping.executable_address(SITE_OFFSET);
            // SAFETY: fixture is mov eax, imm32; ret.
            let function: unsafe extern "C" fn() -> u32 =
                unsafe { core::mem::transmute(execute_address as usize) };
            let tail = mapping.bytes(SITE_OFFSET + 1, 4).to_vec();

            assert_eq!(unsafe { function() }, expected);
            assert!(!probe.is_enabled());
            probe.enable();
            assert!(probe.is_enabled());
            assert_eq!(mapping.bytes(SITE_OFFSET, 1)[0], NEAR_JUMP_OPCODE);
            assert_eq!(mapping.bytes(SITE_OFFSET + 1, 4), tail);
            assert_eq!(unsafe { function() }, expected);
            assert_eq!(CALLBACKS.load(Ordering::Relaxed), 1);
            probe.disable();
            assert!(!probe.is_enabled());
            assert_eq!(mapping.bytes(SITE_OFFSET, 1)[0], code[0]);
            assert_eq!(mapping.bytes(SITE_OFFSET + 1, 4), tail);
            assert_eq!(unsafe { function() }, expected);
            assert_eq!(CALLBACKS.load(Ordering::Relaxed), 1);
            probe.disable();
            probe.enable();
            probe.enable();
            probe.disable();

            assert_eq!(probe.trampoline().address(), probe.plan().target_address());
            let target_permissions =
                permissions(probe.trampoline().address()).expect("trampoline must have a VMA");
            assert!(target_permissions.contains('x'));
            assert!(!target_permissions.contains('w'));
        }

        #[test]
        fn occupied_exact_destination_fails_without_replacing_it() {
            let _guard = serial_guard();
            let mapping = DualMapping::new();
            let (plan, placeholder, _code, _expected) = plan_fixture(&mapping);
            // SAFETY: mapping contract is valid; exact target is intentionally occupied.
            let error =
                match unsafe { RapidProbe::install(plan, mapping.writable_address(SITE_OFFSET)) } {
                    Err(error) => error,
                    Ok(_) => panic!("occupied exact target unexpectedly installed"),
                };
            assert!(matches!(
                error,
                RapidToggleError::Trampoline(TrampolineError::ExactAddressUnavailable { .. })
            ));
            assert_eq!(
                permissions(placeholder.address as u64).as_deref(),
                Some("---p")
            );
            placeholder.release();
        }

        #[test]
        fn install_rejects_each_mutated_window_byte() {
            let _guard = serial_guard();

            for window_offset in 0..NEAR_JUMP_BYTES {
                let mapping = DualMapping::new();
                let (plan, placeholder, code, _expected) = plan_fixture(&mapping);
                let target_address = plan.target_address();
                placeholder.release();
                let expected: [u8; NEAR_JUMP_BYTES] = code[..NEAR_JUMP_BYTES].try_into().unwrap();
                let mut observed = expected;
                observed[window_offset] = observed[window_offset].wrapping_add(1);
                mapping.write(SITE_OFFSET + window_offset, &[observed[window_offset]]);

                // SAFETY: the mapping contract is valid. The intentionally
                // stale window must be rejected before allocating a trampoline.
                let error = match unsafe {
                    RapidProbe::install(plan, mapping.writable_address(SITE_OFFSET))
                } {
                    Err(error) => error,
                    Ok(_) => panic!("mutated window byte {window_offset} was accepted"),
                };
                if window_offset == 0 {
                    assert!(matches!(
                        error,
                        RapidToggleError::ExpectedOpcodeMismatch {
                            expected: expected_opcode,
                            observed: observed_opcode,
                        } if expected_opcode == expected[0] && observed_opcode == observed[0]
                    ));
                } else {
                    assert!(matches!(
                        error,
                        RapidToggleError::ExpectedWindowMismatch {
                            expected: error_expected,
                            observed: error_observed,
                        } if error_expected == expected && error_observed == observed
                    ));
                }
                assert_eq!(permissions(target_address), None);
            }
        }

        #[test]
        fn tail_overlap_failure_discards_non_page_aligned_trampoline() {
            let _guard = serial_guard();
            let mapping = DualMapping::new();
            let (plan, placeholder, code, _expected) = plan_fixture(&mapping);
            let target_address = plan.target_address();
            placeholder.release();
            let execute_address = usize::try_from(mapping.executable_address(SITE_OFFSET)).unwrap();
            let tail_address = execute_address + NEAR_JUMP_BYTES - 1;
            let blocker =
                crate::trap::register(tail_address, tail_address, tail_address + 1, 0).unwrap();
            blocker.begin().unwrap();
            let tail = mapping
                .writable_address(SITE_OFFSET + NEAR_JUMP_BYTES - 1)
                .cast::<AtomicU8>();
            let original_tail = code[NEAR_JUMP_BYTES - 1];
            // SAFETY: blocker owns this registered tail byte while WRITING.
            unsafe { &*tail }.store(original_tail ^ 0xff, Ordering::Release);

            // SAFETY: the mapping contract is valid; the trap registry rejects
            // the plan before reading because an active registered writer owns
            // the tail byte.
            let error =
                match unsafe { RapidProbe::install(plan, mapping.writable_address(SITE_OFFSET)) } {
                    Err(error) => error,
                    Ok(_) => panic!("overlapping rapid probe unexpectedly installed"),
                };
            // SAFETY: blocker still owns the tail byte until finish.
            unsafe { &*tail }.store(original_tail, Ordering::Release);
            blocker.finish();
            assert!(matches!(error, RapidToggleError::OverlappingPatchSite));
            assert_eq!(permissions(target_address), None);
        }

        #[test]
        fn many_threads_toggle_while_other_threads_execute() {
            let _guard = serial_guard();
            CALLBACKS.store(0, Ordering::Relaxed);
            let mapping = DualMapping::new();
            let (probe, _code, expected) = install_fixture(&mapping);
            let probe = Arc::new(probe);
            let execute_address = mapping.executable_address(SITE_OFFSET) as usize;
            let stop = Arc::new(AtomicBool::new(false));
            let executors_ready = Arc::new(Barrier::new(5));
            let mut executors = Vec::new();
            for _ in 0..4 {
                let stop = Arc::clone(&stop);
                let ready = Arc::clone(&executors_ready);
                executors.push(thread::spawn(move || {
                    // SAFETY: fixture and mappings are process-lifetime.
                    let function: unsafe extern "C" fn() -> u32 =
                        unsafe { core::mem::transmute(execute_address) };
                    ready.wait();
                    while !stop.load(Ordering::Relaxed) {
                        assert_eq!(unsafe { function() }, expected);
                    }
                }));
            }
            executors_ready.wait();
            probe.enable();
            let deadline = Instant::now() + Duration::from_secs(2);
            while CALLBACKS.load(Ordering::Relaxed) == 0 && Instant::now() < deadline {
                thread::yield_now();
            }
            assert!(CALLBACKS.load(Ordering::Relaxed) > 0);
            probe.disable();

            let togglers_ready = Arc::new(Barrier::new(5));
            let mut togglers = Vec::new();
            for thread_index in 0..4 {
                let probe = Arc::clone(&probe);
                let ready = Arc::clone(&togglers_ready);
                togglers.push(thread::spawn(move || {
                    ready.wait();
                    for iteration in 0..250_000 {
                        probe.set_enabled((iteration + thread_index) & 1 == 0);
                    }
                }));
            }
            togglers_ready.wait();
            for toggler in togglers {
                toggler.join().unwrap();
            }
            probe.disable();
            stop.store(true, Ordering::Relaxed);
            for executor in executors {
                executor.join().unwrap();
            }
            assert!(!probe.is_enabled());
        }

        #[test]
        #[ignore = "release-mode latency benchmark"]
        fn benchmark_single_byte_toggle_latency() {
            let _guard = serial_guard();
            let mapping = DualMapping::new();
            let (probe, _code, _expected) = install_fixture(&mapping);
            for _ in 0..100_000 {
                probe.enable();
                probe.disable();
            }

            let iterations = 10_000_000_u64;
            let start = Instant::now();
            for _ in 0..iterations {
                probe.enable();
                probe.disable();
            }
            let elapsed = start.elapsed();
            let nanos_per_toggle = elapsed.as_nanos() as f64 / (iterations * 2) as f64;
            eprintln!(
                "rapid toggle: {nanos_per_toggle:.3} ns/store over {} stores",
                iterations * 2
            );
            assert!(
                nanos_per_toggle < 100.0,
                "toggle latency {nanos_per_toggle:.3} ns exceeds 100 ns"
            );
            probe.disable();
        }
    }
}
