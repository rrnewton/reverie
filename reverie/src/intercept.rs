/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! Interception strategy selection (W5 `REVERIE_INTERCEPT`).
//!
//! One typed, refusable decision per run: how guest syscalls are
//! intercepted. Selection is config-phase only (builder call wins
//! over the environment); unknown values, non-UTF-8 values, and
//! strategies unavailable on this arch refuse by name before the
//! guest starts. Kernel floor for every strategy is 6.x.

use std::ffi::OsString;
use std::fmt;

/// Environment variable selecting the interception strategy.
pub const INTERCEPT_ENV: &str = "REVERIE_INTERCEPT";

/// How guest syscalls are intercepted for one run.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum InterceptStrategy {
    /// seccomp `RET_TRAP`/`SIGSYS`, no patching. The v1 default on
    /// both arches and the only upstream aarch64 trap mechanism.
    #[default]
    Trap,
    /// Syscall User Dispatch. x86_64-only upstream (arm64 selects
    /// neither `GENERIC_ENTRY` nor `ARCH_SUPPORTS_SYSCALL_USER_DISPATCH`);
    /// selectable only after a successful runtime probe, with seccomp
    /// fallback at selection time (see `resolve`).
    SudTrap,
    /// Trap on first use, patch thereafter (liteinst trap-and-patch).
    TrapAndPatch,
    /// Patch at load/census time; trap as fallback for unpatchable sites.
    PatchEager,
    /// No in-guest interception; a ptrace backend owns events.
    PtraceExternal,
}

/// Why interception selection refused to construct a run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InterceptSelectError {
    /// The value names no known strategy.
    UnknownValue(String),
    /// The value is not valid UTF-8 (never a silent default).
    NonUnicodeValue,
    /// The strategy cannot run on this architecture/kernel.
    Unavailable {
        /// Strategy that was requested.
        strategy: InterceptStrategy,
        /// Named reason (arch, kernel config, or failed probe).
        reason: &'static str,
    },
}

impl fmt::Display for InterceptSelectError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownValue(value) => {
                write!(
                    f,
                    "unknown {INTERCEPT_ENV} value {value:?} (want trap|sud-trap|trap-and-patch|patch-eager|ptrace-external)"
                )
            }
            Self::NonUnicodeValue => {
                write!(
                    f,
                    "non-UTF-8 {INTERCEPT_ENV} value (want trap|sud-trap|trap-and-patch|patch-eager|ptrace-external)"
                )
            }
            Self::Unavailable { strategy, reason } => {
                write!(f, "intercept strategy {strategy:?} unavailable: {reason}")
            }
        }
    }
}

impl std::error::Error for InterceptSelectError {}

impl InterceptStrategy {
    /// Parse an explicit value; `None`/empty means "no preference".
    pub fn parse(value: Option<&str>) -> Result<Option<Self>, InterceptSelectError> {
        Ok(match value {
            None | Some("") => None,
            Some("trap") => Some(Self::Trap),
            Some("sud-trap") => Some(Self::SudTrap),
            Some("trap-and-patch") => Some(Self::TrapAndPatch),
            Some("patch-eager") => Some(Self::PatchEager),
            Some("ptrace-external") => Some(Self::PtraceExternal),
            Some(other) => {
                return Err(InterceptSelectError::UnknownValue(other.to_owned()));
            }
        })
    }

    fn from_env_os(value: Option<OsString>) -> Result<Option<Self>, InterceptSelectError> {
        match value {
            None => Ok(None),
            Some(value) => match value.to_str() {
                Some(value) => Self::parse(Some(value)),
                None => Err(InterceptSelectError::NonUnicodeValue),
            },
        }
    }

    /// Read the environment preference once.
    pub fn from_env() -> Result<Option<Self>, InterceptSelectError> {
        Self::from_env_os(std::env::var_os(INTERCEPT_ENV))
    }

    /// Select the run's strategy: explicit builder choice wins over
    /// the environment; both absent means the backend default
    /// (`Trap` — the v1 default on both arches, §3 of the press doc).
    pub fn select(builder: Option<Self>) -> Result<Self, InterceptSelectError> {
        if let Some(strategy) = builder {
            return Ok(strategy);
        }
        Ok(Self::from_env()?.unwrap_or_default())
    }

    /// The selection-layer mechanism this strategy selects. Strategies
    /// are operator-facing; mechanisms are what cells register.
    pub fn mechanism(self) -> crate::selection::Mechanism {
        match self {
            Self::Trap => crate::selection::Mechanism::SeccompTrap,
            Self::SudTrap => crate::selection::Mechanism::SudTrap,
            Self::TrapAndPatch => crate::selection::Mechanism::InPlacePatching,
            Self::PatchEager => crate::selection::Mechanism::OfflineRewrite,
            Self::PtraceExternal => crate::selection::Mechanism::PtraceStops,
        }
    }

    /// Resolve a selected strategy against this arch and a SUD probe
    /// result (`sud_probe` = `PR_SET_SYSCALL_USER_DISPATCH` trial
    /// succeeded). Probed SUD absence degrades `SudTrap` to `Trap`
    /// (mechanism downgrade, never semantics); every other
    /// unavailability refuses by name.
    pub fn resolve(self, sud_probe: bool) -> Result<Self, InterceptSelectError> {
        match self {
            Self::SudTrap => {
                #[cfg(not(target_arch = "x86_64"))]
                {
                    let _ = sud_probe;
                    return Err(InterceptSelectError::Unavailable {
                        strategy: self,
                        reason: "SUD is x86-only upstream (no arm64 GENERIC_ENTRY)",
                    });
                }
                #[cfg(target_arch = "x86_64")]
                Ok(if sud_probe { Self::SudTrap } else { Self::Trap })
            }
            other => Ok(other),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_table() {
        assert_eq!(InterceptStrategy::parse(None).unwrap(), None);
        assert_eq!(InterceptStrategy::parse(Some("")).unwrap(), None);
        assert_eq!(
            InterceptStrategy::parse(Some("trap")).unwrap(),
            Some(InterceptStrategy::Trap)
        );
        assert_eq!(
            InterceptStrategy::parse(Some("sud-trap")).unwrap(),
            Some(InterceptStrategy::SudTrap)
        );
        assert_eq!(
            InterceptStrategy::parse(Some("trap-and-patch")).unwrap(),
            Some(InterceptStrategy::TrapAndPatch)
        );
        assert_eq!(
            InterceptStrategy::parse(Some("patch-eager")).unwrap(),
            Some(InterceptStrategy::PatchEager)
        );
        assert_eq!(
            InterceptStrategy::parse(Some("ptrace-external")).unwrap(),
            Some(InterceptStrategy::PtraceExternal)
        );
        let err = InterceptStrategy::parse(Some("ebpf")).unwrap_err();
        assert_eq!(err, InterceptSelectError::UnknownValue("ebpf".into()));
        assert!(err.to_string().contains(INTERCEPT_ENV));
    }

    #[test]
    fn builder_wins_over_env() {
        // Builder present: env never consulted (even if invalid).
        assert_eq!(
            InterceptStrategy::select(Some(InterceptStrategy::TrapAndPatch)).unwrap(),
            InterceptStrategy::TrapAndPatch
        );
    }

    #[test]
    fn sud_resolve_falls_back_only_on_probe_absence() {
        assert_eq!(
            InterceptStrategy::Trap.resolve(false).unwrap(),
            InterceptStrategy::Trap
        );
        #[cfg(target_arch = "x86_64")]
        {
            assert_eq!(
                InterceptStrategy::SudTrap.resolve(true).unwrap(),
                InterceptStrategy::SudTrap
            );
            assert_eq!(
                InterceptStrategy::SudTrap.resolve(false).unwrap(),
                InterceptStrategy::Trap
            );
        }
        #[cfg(not(target_arch = "x86_64"))]
        {
            let err = InterceptStrategy::SudTrap.resolve(true).unwrap_err();
            assert!(matches!(err, InterceptSelectError::Unavailable { .. }));
        }
    }

    #[cfg(unix)]
    #[test]
    fn non_utf8_env_is_named_refusal() {
        use std::os::unix::ffi::OsStringExt;
        let bad = OsString::from_vec(vec![0xff]);
        assert_eq!(
            InterceptStrategy::from_env_os(Some(bad)),
            Err(InterceptSelectError::NonUnicodeValue)
        );
        assert_eq!(InterceptStrategy::from_env_os(None).unwrap(), None);
    }
}
