//! Instruction decoding and cache-line crossing discovery.
//!
//! The scanner decodes a caller-supplied region linearly as x86-64 code. It
//! fails closed on the first invalid or truncated instruction and never returns
//! a partial patch-site map.

use std::collections::BTreeMap;
use std::fmt;

use iced_x86::Decoder;
use iced_x86::DecoderOptions;
use iced_x86::Instruction;

use crate::cache_line::CacheLineSize;

/// Cache-line size used by the default x86-64 scanner.
pub const DEFAULT_CACHE_LINE_BYTES: usize = 64;

/// Cache-line geometry relevant to later modification planning.
///
/// This classification does not by itself prove that a write is atomic. The
/// patcher must also enforce its maximum atomic store width.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ModificationStrategy {
    /// The complete instruction is contained in one cache line.
    SingleCacheLine,
    /// The instruction spans two cache lines.
    ///
    /// The front half contains the instruction head and must be guarded. The
    /// back half is written first, then the front half is published last.
    GuardedCrossLine(CacheLineSplit),
}

impl ModificationStrategy {
    /// Returns `true` when the instruction spans two cache lines.
    pub const fn crosses_cache_line(self) -> bool {
        matches!(self, Self::GuardedCrossLine(_))
    }

    /// Returns cross-line metadata, when present.
    pub const fn split(self) -> Option<CacheLineSplit> {
        match self {
            Self::SingleCacheLine => None,
            Self::GuardedCrossLine(split) => Some(split),
        }
    }
}

/// One side of a cache-line-crossing instruction.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PatchHalf {
    /// Bytes from the instruction head through the end of its first cache line.
    Front,
    /// Remaining instruction bytes in the following cache line.
    Back,
}

/// Exact geometry and publication order for a crossing instruction.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CacheLineSplit {
    boundary_address: u64,
    front_len: usize,
    back_len: usize,
}

impl CacheLineSplit {
    /// Returns the first address in the second cache line.
    pub const fn boundary_address(self) -> u64 {
        self.boundary_address
    }

    /// Returns the number of instruction bytes in the first cache line.
    pub const fn front_len(self) -> usize {
        self.front_len
    }

    /// Returns the number of instruction bytes in the second cache line.
    pub const fn back_len(self) -> usize {
        self.back_len
    }

    /// Returns the half whose instruction heads must be guarded.
    pub const fn guarded_half(self) -> PatchHalf {
        PatchHalf::Front
    }

    /// Returns the half written after the guard becomes visible.
    pub const fn first_write_half(self) -> PatchHalf {
        PatchHalf::Back
    }

    /// Returns the half whose final write publishes the completed instruction.
    pub const fn publication_half(self) -> PatchHalf {
        PatchHalf::Front
    }
}

/// A decoded instruction and its location in the scanned region.
#[derive(Clone, Debug)]
pub struct ScannedInstruction {
    offset: usize,
    instruction: Instruction,
}

impl ScannedInstruction {
    /// Returns the instruction byte offset from the region base.
    pub const fn offset(&self) -> usize {
        self.offset
    }

    /// Returns the instruction virtual address.
    pub const fn address(&self) -> u64 {
        self.instruction.ip()
    }

    /// Returns the instruction length in bytes.
    pub const fn len(&self) -> usize {
        self.instruction.len()
    }

    /// Returns `true` when the instruction has zero bytes.
    ///
    /// Valid x86 instructions are never empty. This method mirrors standard
    /// collection APIs and is useful to generic callers.
    pub const fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Returns the iced-x86 instruction.
    pub const fn instruction(&self) -> &Instruction {
        &self.instruction
    }
}

/// Scanner-level metadata for one decoded instruction head.
///
/// Every valid decoded instruction is a candidate at this stage. Later
/// relocation and pun-selection passes may reject a candidate.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PatchableSite {
    address: u64,
    offset: usize,
    instruction_len: usize,
    modification: ModificationStrategy,
}

impl PatchableSite {
    /// Returns the instruction virtual address.
    pub const fn address(self) -> u64 {
        self.address
    }

    /// Returns the instruction byte offset from the region base.
    pub const fn offset(self) -> usize {
        self.offset
    }

    /// Returns the instruction length in bytes.
    pub const fn instruction_len(self) -> usize {
        self.instruction_len
    }

    /// Returns the required cache-line modification strategy.
    pub const fn modification(self) -> ModificationStrategy {
        self.modification
    }
}

/// Complete result of scanning one contiguous code region.
#[derive(Clone, Debug, Default)]
pub struct ScanResult {
    // AUTONOMOUS-BOT-IMPLEMENTED
    // TODO-HUMAN-REVIEW(#11): Review exact snapshot retention and comparison.
    snapshot: Vec<u8>,
    instructions: Vec<ScannedInstruction>,
    sites: BTreeMap<u64, PatchableSite>,
}

impl ScanResult {
    /// Returns the exact code bytes represented by this scan.
    pub fn snapshot(&self) -> &[u8] {
        &self.snapshot
    }

    /// Returns decoded instructions in address order.
    pub fn instructions(&self) -> &[ScannedInstruction] {
        &self.instructions
    }

    /// Returns scanner-level patch candidates keyed by instruction address.
    pub const fn sites(&self) -> &BTreeMap<u64, PatchableSite> {
        &self.sites
    }

    /// Returns the candidate at `address`, when it is an instruction head.
    pub fn site(&self, address: u64) -> Option<&PatchableSite> {
        self.sites.get(&address)
    }

    /// Returns whether both scans represent the same exact code snapshot.
    pub(crate) fn matches_snapshot(&self, other: &Self) -> bool {
        self.snapshot == other.snapshot
            && self.sites == other.sites
            && self.instructions.len() == other.instructions.len()
            && self
                .instructions
                .iter()
                .zip(&other.instructions)
                .all(|(expected, current)| {
                    expected.offset() == current.offset()
                        && expected.instruction() == current.instruction()
                })
    }

    /// Iterates over cache-line-crossing candidates in address order.
    pub fn crossing_sites(&self) -> impl Iterator<Item = &PatchableSite> {
        self.sites
            .values()
            .filter(|site| site.modification.crosses_cache_line())
    }
}

/// Failures that prevent a complete and trustworthy scan.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ScanError {
    /// A requested instruction prefix is longer than the available byte window.
    PrefixTooShort {
        /// Minimum number of complete instruction bytes requested.
        required: usize,
        /// Number of bytes available to the decoder.
        available: usize,
    },
    /// The half-open virtual-address range cannot be represented by `u64`.
    AddressRangeOverflow {
        /// Region start address.
        base_address: u64,
        /// Region size in bytes.
        byte_len: usize,
    },
    /// A decoded address cannot be represented as a native pointer.
    AddressNotRepresentable {
        /// Instruction address reported by the decoder.
        address: u64,
    },
    /// One instruction would span more than two configured cache lines.
    UnsupportedCacheLineGeometry {
        /// Address of the instruction.
        address: u64,
        /// Instruction size in bytes.
        instruction_len: usize,
        /// Configured cache-line size in bytes.
        cache_line_bytes: usize,
    },
    /// iced-x86 rejected an instruction encoding.
    InvalidInstruction {
        /// Address at which decoding failed.
        address: u64,
        /// Byte offset from the region base.
        offset: usize,
    },
}

impl fmt::Display for ScanError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::PrefixTooShort {
                required,
                available,
            } => write!(
                formatter,
                "instruction prefix needs {required} bytes but only {available} are available"
            ),
            Self::AddressRangeOverflow {
                base_address,
                byte_len,
            } => write!(
                formatter,
                "code region at {base_address:#x} with length {byte_len} overflows the address space"
            ),
            Self::AddressNotRepresentable { address } => {
                write!(
                    formatter,
                    "instruction address {address:#x} is not representable"
                )
            }
            Self::UnsupportedCacheLineGeometry {
                address,
                instruction_len,
                cache_line_bytes,
            } => write!(
                formatter,
                "{instruction_len}-byte instruction at {address:#x} spans more than two {cache_line_bytes}-byte cache lines"
            ),
            Self::InvalidInstruction { address, offset } => write!(
                formatter,
                "invalid x86-64 instruction at {address:#x} (region offset {offset})"
            ),
        }
    }
}

impl std::error::Error for ScanError {}

/// Linear x86-64 instruction scanner.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InstructionScanner {
    cache_line: CacheLineSize,
}

impl InstructionScanner {
    /// Creates a scanner using `cache_line` for crossing classification.
    pub const fn new(cache_line: CacheLineSize) -> Self {
        Self { cache_line }
    }

    /// Returns the configured cache-line size.
    pub const fn cache_line_size(self) -> CacheLineSize {
        self.cache_line
    }

    /// Decodes all instructions in `code` starting at `base_address`.
    ///
    /// The input must contain code only and end on an instruction boundary.
    /// Invalid or truncated input returns an error without exposing partial
    /// results.
    pub fn scan(&self, code: &[u8], base_address: u64) -> Result<ScanResult, ScanError> {
        let byte_len = u64::try_from(code.len()).map_err(|_| ScanError::AddressRangeOverflow {
            base_address,
            byte_len: code.len(),
        })?;
        base_address
            .checked_add(byte_len)
            .ok_or(ScanError::AddressRangeOverflow {
                base_address,
                byte_len: code.len(),
            })?;

        let mut decoder = Decoder::with_ip(64, code, base_address, DecoderOptions::NONE);
        let mut instructions = Vec::new();
        let mut sites = BTreeMap::new();

        while decoder.can_decode() {
            let offset = decoder.position();
            let instruction = decoder.decode();
            let address = instruction.ip();

            if instruction.is_invalid() {
                return Err(ScanError::InvalidInstruction { address, offset });
            }

            let native_address = usize::try_from(address)
                .map_err(|_| ScanError::AddressNotRepresentable { address })?;
            let instruction_len = instruction.len();
            let modification = match self
                .cache_line
                .split_offset(native_address, instruction_len)
            {
                Some(front_len) => {
                    let back_len = instruction_len - front_len;
                    if back_len > self.cache_line.get() {
                        return Err(ScanError::UnsupportedCacheLineGeometry {
                            address,
                            instruction_len,
                            cache_line_bytes: self.cache_line.get(),
                        });
                    }
                    let boundary_address = address.checked_add(front_len as u64).ok_or(
                        ScanError::AddressRangeOverflow {
                            base_address,
                            byte_len: code.len(),
                        },
                    )?;
                    ModificationStrategy::GuardedCrossLine(CacheLineSplit {
                        boundary_address,
                        front_len,
                        back_len,
                    })
                }
                None => ModificationStrategy::SingleCacheLine,
            };

            let site = PatchableSite {
                address,
                offset,
                instruction_len,
                modification,
            };
            sites.insert(address, site);
            instructions.push(ScannedInstruction {
                offset,
                instruction,
            });
        }

        Ok(ScanResult {
            snapshot: code.to_vec(),
            instructions,
            sites,
        })
    }

    /// Decodes the shortest complete instruction prefix containing at least
    /// `minimum_bytes` bytes.
    ///
    /// The caller may provide extra readable bytes without proving that the
    /// complete slice is code or ends on an instruction boundary. This is
    /// useful when registering a live patch from an instruction pointer: the
    /// returned snapshot contains only fully decoded instructions and can be
    /// passed to later planners without retaining the speculative tail.
    pub fn scan_prefix(
        &self,
        code: &[u8],
        base_address: u64,
        minimum_bytes: usize,
    ) -> Result<ScanResult, ScanError> {
        if code.len() < minimum_bytes {
            return Err(ScanError::PrefixTooShort {
                required: minimum_bytes,
                available: code.len(),
            });
        }
        let byte_len = u64::try_from(code.len()).map_err(|_| ScanError::AddressRangeOverflow {
            base_address,
            byte_len: code.len(),
        })?;
        base_address
            .checked_add(byte_len)
            .ok_or(ScanError::AddressRangeOverflow {
                base_address,
                byte_len: code.len(),
            })?;

        let mut decoder = Decoder::with_ip(64, code, base_address, DecoderOptions::NONE);
        while decoder.position() < minimum_bytes {
            let offset = decoder.position();
            let instruction = decoder.decode();
            if instruction.is_invalid() {
                return Err(ScanError::InvalidInstruction {
                    address: instruction.ip(),
                    offset,
                });
            }
        }
        self.scan(&code[..decoder.position()], base_address)
    }
}

impl Default for InstructionScanner {
    fn default() -> Self {
        let cache_line =
            CacheLineSize::new(DEFAULT_CACHE_LINE_BYTES).expect("default cache line is non-zero");
        Self::new(cache_line)
    }
}

#[cfg(test)]
mod tests {
    use iced_x86::Mnemonic;

    use super::InstructionScanner;
    use super::ModificationStrategy;
    use super::PatchHalf;
    use super::ScanError;

    #[test]
    fn known_instruction_fixture_finds_both_crossers() {
        let mut code = vec![0x90; 63];
        code.extend_from_slice(&[0x66, 0x90]);
        code.resize(125, 0x90);
        code.extend_from_slice(&[0x48, 0x83, 0xC0, 0x01]);

        let result = InstructionScanner::default().scan(&code, 0).unwrap();

        assert_eq!(result.instructions().len(), 125);
        assert_eq!(result.sites().len(), result.instructions().len());
        assert_eq!(
            result
                .instructions()
                .last()
                .unwrap()
                .instruction()
                .mnemonic(),
            Mnemonic::Add
        );

        let crossers: Vec<_> = result.crossing_sites().copied().collect();
        assert_eq!(crossers.len(), 2);
        assert_eq!(crossers[0].address(), 63);
        assert_eq!(crossers[1].address(), 125);

        let first = crossers[0].modification().split().unwrap();
        assert_eq!(first.boundary_address(), 64);
        assert_eq!((first.front_len(), first.back_len()), (1, 1));
        assert_eq!(first.guarded_half(), PatchHalf::Front);
        assert_eq!(first.first_write_half(), PatchHalf::Back);
        assert_eq!(first.publication_half(), PatchHalf::Front);

        let second = crossers[1].modification().split().unwrap();
        assert_eq!(second.boundary_address(), 128);
        assert_eq!((second.front_len(), second.back_len()), (3, 1));
    }

    #[test]
    fn instruction_ending_at_boundary_is_single_line() {
        let mut code = vec![0x90; 61];
        code.extend_from_slice(&[0x48, 0x89, 0xC0]);

        let result = InstructionScanner::default().scan(&code, 0).unwrap();
        let site = result.site(61).unwrap();

        assert_eq!(site.instruction_len(), 3);
        assert_eq!(site.modification(), ModificationStrategy::SingleCacheLine);
        assert_eq!(result.crossing_sites().count(), 0);
    }

    #[test]
    fn nonzero_base_address_controls_alignment() {
        let code = [0x66, 0x90, 0xC3];
        let result = InstructionScanner::default().scan(&code, 0x103F).unwrap();

        let site = result.site(0x103F).unwrap();
        let split = site.modification().split().unwrap();
        assert_eq!(site.offset(), 0);
        assert_eq!(split.boundary_address(), 0x1040);
        assert_eq!((split.front_len(), split.back_len()), (1, 1));
    }

    #[test]
    fn invalid_tail_fails_without_a_partial_result() {
        let code = [0x90, 0xF3, 0x0F];
        let error = InstructionScanner::default()
            .scan(&code, 0x2000)
            .unwrap_err();

        assert_eq!(
            error,
            ScanError::InvalidInstruction {
                address: 0x2001,
                offset: 1,
            }
        );
    }

    #[test]
    fn overflowing_region_is_rejected_before_decoding() {
        let error = InstructionScanner::default()
            .scan(&[0x90], u64::MAX)
            .unwrap_err();

        assert_eq!(
            error,
            ScanError::AddressRangeOverflow {
                base_address: u64::MAX,
                byte_len: 1,
            }
        );
    }

    #[test]
    fn prefix_scan_stops_on_the_first_boundary_after_the_minimum() {
        let scanner = InstructionScanner::default();
        // syscall (2), four-byte add, then a deliberately truncated MOV.
        let code = [0x0F, 0x05, 0x48, 0x83, 0xC0, 0x01, 0xB8];

        let result = scanner.scan_prefix(&code, 0x1000, 5).unwrap();

        assert_eq!(result.snapshot(), &code[..6]);
        assert_eq!(result.instructions().len(), 2);
    }

    #[test]
    fn geometry_spanning_more_than_two_lines_is_rejected() {
        let cache_line = crate::cache_line::CacheLineSize::new(1).unwrap();
        let scanner = InstructionScanner::new(cache_line);
        let error = scanner.scan(&[0x48, 0x83, 0xC0, 0x01], 0).unwrap_err();

        assert_eq!(
            error,
            ScanError::UnsupportedCacheLineGeometry {
                address: 0,
                instruction_len: 4,
                cache_line_bytes: 1,
            }
        );
    }

    #[test]
    fn empty_region_has_no_instructions_or_sites() {
        let result = InstructionScanner::default().scan(&[], u64::MAX).unwrap();

        assert!(result.instructions().is_empty());
        assert!(result.sites().is_empty());
    }
}
