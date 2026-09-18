//! Conservative strategy selection for hook registration.
//!
//! The planner makes the expensive registration choice explicit: use an exact
//! one-byte pun when the existing instruction bytes encode a safe target,
//! otherwise relocate a complete instruction window, and finally tell the
//! caller that a trap fallback is required. Trap delivery and policy remain a
//! client concern rather than a dependency of this patching crate.

use crate::patcher::PatchError;
use crate::rapid::RapidToggleError;
use crate::rapid::RapidTogglePlan;
use crate::scanner::InstructionScanner;
use crate::scanner::ScanResult;
use crate::trampoline::HookCallback;
use crate::trampoline::TrampolineError;
use crate::trampoline::TrampolinePlan;

/// Whether an installed hook observes or replaces the first instruction.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HookSemantics {
    /// Invoke the callback, then execute every displaced instruction.
    Observe,
    /// Invoke the callback instead of the first instruction, then execute tail
    /// instructions consumed by the patch window.
    ReplaceFirst,
}

/// A fail-closed request for the client to retain its trap path.
#[derive(Debug)]
pub struct TrapFallback {
    address: u64,
    rapid_error: Option<RapidToggleError>,
    relocation_error: TrampolineError,
}

impl TrapFallback {
    /// Returns the requested instruction address.
    pub const fn address(&self) -> u64 {
        self.address
    }

    /// Returns why exact one-byte punning was unavailable.
    ///
    /// Replace-first hooks do not attempt an exact observing pun and therefore
    /// return None here.
    pub const fn rapid_error(&self) -> Option<&RapidToggleError> {
        self.rapid_error.as_ref()
    }

    /// Returns why relocation could not produce a complete patch window.
    pub const fn relocation_error(&self) -> &TrampolineError {
        &self.relocation_error
    }
}

/// Registration strategy selected without mutating executable memory.
#[derive(Debug)]
pub enum PunPlan {
    /// Prefer a one-byte opcode toggle, with relocation ready if exact
    /// trampoline allocation later reports a collision.
    RapidPreferred {
        /// Exact one-byte pun plan.
        rapid: RapidTogglePlan,
        /// Relocation fallback, when a complete window is available.
        relocated: Option<TrampolinePlan>,
    },
    /// Publish a five-byte jump and relocate complete instructions.
    Relocated(TrampolinePlan),
    /// Neither safe strategy applies; the client must keep trapping.
    TrapRequired(TrapFallback),
}

/// Selects the least invasive safe strategy for one instruction head.
pub fn plan_hook(
    scanner: &InstructionScanner,
    scan: &ScanResult,
    code: &[u8],
    region_base: u64,
    execute_address: u64,
    hook: HookCallback,
    semantics: HookSemantics,
) -> PunPlan {
    let verified_scan = match scanner.scan(code, region_base) {
        Ok(verified) if scan.matches_snapshot(&verified) => verified,
        Ok(_) => {
            return PunPlan::TrapRequired(TrapFallback {
                address: execute_address,
                rapid_error: None,
                relocation_error: PatchError::RegionMismatch {
                    address: execute_address,
                }
                .into(),
            });
        }
        Err(source) => {
            return PunPlan::TrapRequired(TrapFallback {
                address: execute_address,
                rapid_error: None,
                relocation_error: PatchError::InvalidCodeRegion { source }.into(),
            });
        }
    };

    let relocated = match semantics {
        HookSemantics::Observe => TrampolinePlan::from_scan(&verified_scan, execute_address, hook),
        HookSemantics::ReplaceFirst => {
            TrampolinePlan::from_scan_replacing_first(&verified_scan, execute_address, hook)
        }
    };

    let rapid_error = match semantics {
        HookSemantics::Observe => match RapidTogglePlan::from_scan(
            scanner,
            scan,
            code,
            region_base,
            execute_address,
            hook,
        ) {
            Ok(rapid) => {
                return PunPlan::RapidPreferred {
                    rapid,
                    relocated: relocated.ok(),
                };
            }
            Err(error) => Some(error),
        },
        HookSemantics::ReplaceFirst => None,
    };

    match relocated {
        Ok(plan) => PunPlan::Relocated(plan),
        Err(relocation_error) => PunPlan::TrapRequired(TrapFallback {
            address: execute_address,
            rapid_error,
            relocation_error,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::HookSemantics;
    use super::PunPlan;
    use super::plan_hook;
    use crate::scanner::InstructionScanner;
    use crate::trampoline::HookContext;

    unsafe extern "C" fn noop(_context: *mut HookContext) {}

    #[test]
    fn exact_five_byte_instruction_prefers_rapid_punning() {
        let base = 0x10_0000;
        let code = [0xB8, 0, 0, 0, 0, 0xC3, 0x90, 0x90];
        let scanner = InstructionScanner::default();
        let scan = scanner.scan(&code, base).unwrap();

        let plan = plan_hook(
            &scanner,
            &scan,
            &code,
            base,
            base,
            noop,
            HookSemantics::Observe,
        );

        match plan {
            PunPlan::RapidPreferred {
                rapid: _,
                relocated,
            } => assert!(relocated.is_some()),
            _ => panic!("exact site should retain a relocation fallback"),
        }
    }

    #[test]
    fn replace_first_syscall_uses_multi_instruction_relocation() {
        let base = 0x20_0000;
        let code = [0x0F, 0x05, 0x90, 0x90, 0x90, 0xC3, 0x90, 0x90];
        let scanner = InstructionScanner::default();
        let scan = scanner.scan(&code, base).unwrap();

        let plan = plan_hook(
            &scanner,
            &scan,
            &code,
            base,
            base,
            noop,
            HookSemantics::ReplaceFirst,
        );

        match plan {
            PunPlan::Relocated(plan) => {
                assert!(plan.replaces_first());
                assert_eq!(plan.displaced_len(), 5);
            }
            _ => panic!("syscall replacement should relocate a complete window"),
        }
    }

    #[test]
    fn incomplete_window_requests_client_trap_fallback() {
        let base = 0x30_0000;
        let code = [0xC3];
        let scanner = InstructionScanner::default();
        let scan = scanner.scan(&code, base).unwrap();

        let plan = plan_hook(
            &scanner,
            &scan,
            &code,
            base,
            base,
            noop,
            HookSemantics::Observe,
        );

        match plan {
            PunPlan::TrapRequired(fallback) => {
                assert_eq!(fallback.address(), base);
                assert!(fallback.rapid_error().is_some());
            }
            _ => panic!("incomplete window must retain trapping"),
        }
    }

    #[test]
    fn replace_first_rejects_a_stale_scan() {
        let base = 0x40_0000;
        let original = [0x0F, 0x05, 0x90, 0x90, 0x90, 0xC3, 0x90, 0x90];
        let mutated = [0x0F, 0x05, 0x91, 0x90, 0x90, 0xC3, 0x90, 0x90];
        let scanner = InstructionScanner::default();
        let stale_scan = scanner.scan(&original, base).unwrap();

        let plan = plan_hook(
            &scanner,
            &stale_scan,
            &mutated,
            base,
            base,
            noop,
            HookSemantics::ReplaceFirst,
        );

        assert!(matches!(plan, PunPlan::TrapRequired(_)));
    }
}
