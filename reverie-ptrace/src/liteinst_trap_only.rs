/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! Runtime-free LiteInst launch mode ("trap-only").
//!
//! Trap-only LiteInst loads nothing into the guest: no preload, no runtime,
//! no handshake. It launches through the ordinary ptrace tracer and keeps the
//! dynamic LiteInst runtime configuration absent, so every lifecycle branch
//! that distinguishes the preload hybrid takes its plain-ptrace arm.
//!
//! Patching state is kept separately, in one [`SiteTable`] per guest address
//! space. A later increment rewrites first-seen x86_64 `syscall` sites to
//! `int 0x80` in place, from the tracer, so that later executions arrive as
//! `AUDIT_ARCH_I386` seccomp stops. That scheme needs the kernel's IA-32
//! syscall entry, so a trap-only launch probes for it and fails closed when it
//! is missing. There is no fallback to plain ptrace under the LiteInst label.
//!
//! In this increment the only patching state is [`SitePatching::Off`]: no
//! guest byte is ever written, and a trap-only run is observably the ordinary
//! ptrace run. The seccomp filter is unchanged; it still kills any
//! non-x86_64 syscall.

use std::collections::BTreeMap;
use std::fmt;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::OnceLock;

use crate::LiteinstInstrumentationStats;

/// Whether a trap-only LiteInst run rewrites syscall sites in the guest.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum SitePatching {
    /// Never write a guest byte. Every syscall takes the ordinary ptrace
    /// seccomp path, so the run is the ptrace run.
    Off,
}

impl fmt::Display for SitePatching {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Off => formatter.write_str("off"),
        }
    }
}

/// Patched syscall sites of one guest address space.
///
/// Each entry maps a site address to the two original instruction bytes that
/// the patch replaced, so that the bytes can be restored before the guest
/// changes or discards the page. With [`SitePatching::Off`] the table is
/// never populated.
#[derive(Clone, Debug)]
pub struct SiteTable {
    patching: SitePatching,
    sites: BTreeMap<u64, [u8; 2]>,
}

impl SiteTable {
    /// Creates an empty table for a fresh address space.
    pub fn new(patching: SitePatching) -> Self {
        Self {
            patching,
            sites: BTreeMap::new(),
        }
    }

    /// Returns the patching state of this address space.
    pub fn patching(&self) -> SitePatching {
        self.patching
    }

    /// Returns the number of sites whose bytes are currently patched.
    pub fn patched_sites(&self) -> usize {
        self.sites.len()
    }
}

/// Trap-only launch configuration carried by a `TracerBuilder`.
#[derive(Clone, Debug)]
pub(crate) struct LiteinstTrapOnlyConfig {
    pub(crate) root_sites: Arc<Mutex<SiteTable>>,
    pub(crate) instrumentation_stats: Option<Arc<Mutex<LiteinstInstrumentationStats>>>,
    #[cfg(test)]
    pub(crate) ia32_probe_override: Option<Ia32EmulationProbe>,
}

impl LiteinstTrapOnlyConfig {
    pub(crate) fn new(patching: SitePatching, collect_stats: bool) -> Self {
        Self {
            root_sites: Arc::new(Mutex::new(SiteTable::new(patching))),
            instrumentation_stats: collect_stats
                .then(|| Arc::new(Mutex::new(LiteinstInstrumentationStats::default()))),
            #[cfg(test)]
            ia32_probe_override: None,
        }
    }

    /// Returns the IA-32 syscall-entry probe result for this launch.
    pub(crate) fn ia32_probe(&self) -> Ia32EmulationProbe {
        #[cfg(test)]
        if let Some(probe) = self.ia32_probe_override.clone() {
            return probe;
        }
        probe_ia32_emulation()
    }
}

/// Observer for the trap-only state of a running tracer.
#[derive(Clone, Debug)]
pub struct LiteinstTrapOnlyHandle {
    root_sites: Arc<Mutex<SiteTable>>,
}

impl LiteinstTrapOnlyHandle {
    pub(crate) fn from_config(config: &LiteinstTrapOnlyConfig) -> Self {
        Self {
            root_sites: Arc::clone(&config.root_sites),
        }
    }

    /// Returns the patching state of the root address space.
    pub fn patching(&self) -> SitePatching {
        self.lock().patching()
    }

    /// Returns the number of sites currently patched in the root address space.
    pub fn patched_sites(&self) -> usize {
        self.lock().patched_sites()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, SiteTable> {
        self.root_sites
            .lock()
            .expect("LiteInst trap-only site table lock poisoned")
    }
}

/// Result of probing whether this host services `int 0x80` from 64-bit code.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Ia32EmulationProbe {
    /// An `int 0x80` getpid returned the caller's PID.
    Available,
    /// `int 0x80` is not serviced; the text says what the probe observed.
    Unavailable(String),
}

/// A trap-only LiteInst launch was refused because `int 0x80` is unusable.
///
/// Trap-only patching routes later executions of a site through the IA-32
/// syscall entry. Without `CONFIG_IA32_EMULATION`, or with it disabled by the
/// `ia32_emulation=` boot parameter, that entry faults. The launch fails
/// closed instead of running plain ptrace under the LiteInst label.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
#[error(
    "LiteInst trap-only launch refused: IA-32 syscall emulation (int 0x80) is unavailable \
     on this host, which requires CONFIG_IA32_EMULATION and no ia32_emulation=false boot \
     parameter ({observation})"
)]
pub struct Ia32EmulationUnavailable {
    /// What the probe observed.
    pub observation: String,
}

/// Converts a probe result into the trap-only admission decision.
pub(crate) fn require_ia32_emulation(
    probe: Ia32EmulationProbe,
) -> Result<(), Ia32EmulationUnavailable> {
    match probe {
        Ia32EmulationProbe::Available => Ok(()),
        Ia32EmulationProbe::Unavailable(observation) => {
            Err(Ia32EmulationUnavailable { observation })
        }
    }
}

/// Probes, once per process, whether `int 0x80` is serviced.
///
/// The boot parameter and kernel configuration cannot change while this
/// process runs, so the first answer is cached.
pub fn probe_ia32_emulation() -> Ia32EmulationProbe {
    static PROBE: OnceLock<Ia32EmulationProbe> = OnceLock::new();
    PROBE.get_or_init(probe_ia32_emulation_uncached).clone()
}

#[cfg(not(target_arch = "x86_64"))]
fn probe_ia32_emulation_uncached() -> Ia32EmulationProbe {
    Ia32EmulationProbe::Unavailable("int 0x80 exists only on x86_64 hosts".into())
}

/// Executes an IA-32 `getpid` through `int 0x80` in a forked child.
///
/// A missing IA-32 entry raises a general-protection fault, which the child
/// receives as `SIGSEGV`. The child only performs async-signal-safe work
/// between `fork` and `_exit`.
#[cfg(target_arch = "x86_64")]
fn probe_ia32_emulation_uncached() -> Ia32EmulationProbe {
    // SAFETY: the child runs only raw syscalls, the inline assembly, and
    // `_exit`, all of which are async-signal-safe after a multithreaded fork.
    let child = unsafe { libc::fork() };
    if child < 0 {
        return Ia32EmulationProbe::Unavailable(format!(
            "probe fork failed: {}",
            std::io::Error::last_os_error()
        ));
    }
    if child == 0 {
        unsafe {
            // A missing entry must neither run an inherited handler nor dump core.
            libc::signal(libc::SIGSEGV, libc::SIG_DFL);
            libc::signal(libc::SIGSYS, libc::SIG_DFL);
            libc::prctl(libc::PR_SET_DUMPABLE, 0, 0, 0, 0);
            const IA32_NR_GETPID: u64 = 20;
            let result: u64;
            core::arch::asm!(
                "int 0x80",
                inlateout("rax") IA32_NR_GETPID => result,
                lateout("r8") _,
                lateout("r9") _,
                lateout("r10") _,
                lateout("r11") _,
                options(nostack),
            );
            let expected = libc::syscall(libc::SYS_getpid) as u64;
            libc::_exit(if result == expected { 0 } else { 1 });
        }
    }
    let mut status = 0;
    loop {
        let waited = unsafe { libc::waitpid(child, &mut status, 0) };
        if waited == child {
            break;
        }
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() != Some(libc::EINTR) {
            return Ia32EmulationProbe::Unavailable(format!("probe wait failed: {error}"));
        }
    }
    classify_probe_status(status, &boot_parameter_note())
}

#[cfg(target_arch = "x86_64")]
fn classify_probe_status(status: libc::c_int, boot_note: &str) -> Ia32EmulationProbe {
    if libc::WIFEXITED(status) {
        match libc::WEXITSTATUS(status) {
            0 => Ia32EmulationProbe::Available,
            code => Ia32EmulationProbe::Unavailable(format!(
                "int 0x80 getpid returned a value other than the caller's PID (probe exit {code}){boot_note}"
            )),
        }
    } else if libc::WIFSIGNALED(status) {
        let signal = libc::WTERMSIG(status);
        let name = match signal {
            libc::SIGSEGV => "SIGSEGV",
            libc::SIGSYS => "SIGSYS",
            libc::SIGKILL => "SIGKILL",
            _ => "a signal",
        };
        Ia32EmulationProbe::Unavailable(format!(
            "int 0x80 getpid killed the probe with {name} ({signal}){boot_note}"
        ))
    } else {
        Ia32EmulationProbe::Unavailable(format!(
            "unexpected probe wait status {status:#x}{boot_note}"
        ))
    }
}

/// Names an `ia32_emulation=` boot parameter, when present, for diagnostics.
#[cfg(target_arch = "x86_64")]
fn boot_parameter_note() -> String {
    std::fs::read_to_string("/proc/cmdline")
        .ok()
        .and_then(|cmdline| {
            cmdline
                .split_whitespace()
                .find(|word| word.starts_with("ia32_emulation="))
                .map(|word| format!("; kernel command line has {word}"))
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn site_table_with_patching_off_starts_and_stays_empty() {
        let table = SiteTable::new(SitePatching::Off);
        assert_eq!(table.patching(), SitePatching::Off);
        assert_eq!(table.patched_sites(), 0);
    }

    #[test]
    fn unavailable_probe_is_a_named_refusal() {
        let error = require_ia32_emulation(Ia32EmulationProbe::Unavailable(
            "int 0x80 getpid killed the probe with SIGSEGV (11)".into(),
        ))
        .expect_err("an unavailable IA-32 entry must refuse trap-only launch");
        assert_eq!(
            error.observation,
            "int 0x80 getpid killed the probe with SIGSEGV (11)"
        );
        let message = error.to_string();
        assert!(
            message.contains("LiteInst trap-only launch refused")
                && message.contains("CONFIG_IA32_EMULATION")
                && message.contains("ia32_emulation=")
                && message.contains("SIGSEGV"),
            "{message}"
        );
    }

    #[test]
    fn available_probe_admits_launch() {
        assert_eq!(
            require_ia32_emulation(Ia32EmulationProbe::Available),
            Ok(())
        );
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn probe_status_classification_names_the_fault() {
        // Wait statuses in the layout waitpid(2) reports.
        let exited = |code: libc::c_int| code << 8;
        let signaled = |signal: libc::c_int| signal;
        assert_eq!(
            classify_probe_status(exited(0), ""),
            Ia32EmulationProbe::Available
        );
        let Ia32EmulationProbe::Unavailable(text) = classify_probe_status(
            signaled(libc::SIGSEGV),
            "; kernel command line has ia32_emulation=0",
        ) else {
            panic!("SIGSEGV must classify as unavailable");
        };
        assert!(
            text.contains("SIGSEGV") && text.contains("ia32_emulation=0"),
            "{text}"
        );
        assert!(matches!(
            classify_probe_status(signaled(libc::SIGSYS), ""),
            Ia32EmulationProbe::Unavailable(text) if text.contains("SIGSYS")
        ));
        assert!(matches!(
            classify_probe_status(exited(1), ""),
            Ia32EmulationProbe::Unavailable(text) if text.contains("probe exit 1")
        ));
    }
}
