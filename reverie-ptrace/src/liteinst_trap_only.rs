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
/// process runs, so the first definitive answer is cached. A probe that could
/// not run (for example, because installing its signal handler failed) is
/// reported but not cached, so a later launch probes again.
///
/// The probe runs on the calling thread and creates no task. That matters
/// because an embedder may call this from inside the guest's PID namespace:
/// a forked probe child would take a PID there and shift every guest PID by
/// one relative to a plain-ptrace run.
pub fn probe_ia32_emulation() -> Ia32EmulationProbe {
    static PROBE: Mutex<Option<Ia32EmulationProbe>> = Mutex::new(None);
    let mut cached = PROBE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(probe) = cached.as_ref() {
        return probe.clone();
    }
    let (probe, definitive) = probe_ia32_emulation_uncached();
    if definitive {
        *cached = Some(probe.clone());
    }
    probe
}

#[cfg(not(target_arch = "x86_64"))]
fn probe_ia32_emulation_uncached() -> (Ia32EmulationProbe, bool) {
    (
        Ia32EmulationProbe::Unavailable("int 0x80 exists only on x86_64 hosts".into()),
        true,
    )
}

/// Executes an IA-32 `getpid` through `int 0x80` on the calling thread.
///
/// A missing IA-32 entry raises a general-protection fault (`SIGSEGV`; a
/// not-present gate would raise `SIGBUS`), and a seccomp filter can answer
/// with `SIGSYS`. A temporary handler for these signals catches the fault on
/// this thread only, steps over the instruction,
/// and records the signal. Returns the probe result and whether it is
/// definitive (cacheable).
#[cfg(target_arch = "x86_64")]
fn probe_ia32_emulation_uncached() -> (Ia32EmulationProbe, bool) {
    // SAFETY: `int80_getpid` executes only `int 0x80`, which the guard
    // expects.
    match unsafe { guarded_fault::run(guarded_fault::int80_getpid) } {
        Ok((fault, result)) => {
            let expected = unsafe { libc::syscall(libc::SYS_getpid) } as u64;
            (
                classify_probe_outcome(fault, result, expected, &boot_parameter_note()),
                true,
            )
        }
        Err(reason) => (
            Ia32EmulationProbe::Unavailable(format!("the probe could not run: {reason}")),
            false,
        ),
    }
}

#[cfg(target_arch = "x86_64")]
fn classify_probe_outcome(
    fault: Option<libc::c_int>,
    result: u64,
    expected: u64,
    boot_note: &str,
) -> Ia32EmulationProbe {
    match fault {
        None if result == expected => Ia32EmulationProbe::Available,
        None => Ia32EmulationProbe::Unavailable(format!(
            "int 0x80 getpid returned {result:#x}, not the caller's PID {expected}{boot_note}"
        )),
        Some(signal) => {
            let name = match signal {
                libc::SIGSEGV => "SIGSEGV",
                libc::SIGBUS => "SIGBUS",
                libc::SIGSYS => "SIGSYS",
                _ => "a signal",
            };
            Ia32EmulationProbe::Unavailable(format!(
                "int 0x80 getpid raised {name} ({signal}){boot_note}"
            ))
        }
    }
}

/// Runs one `int` instruction on the calling thread with its fault caught.
#[cfg(target_arch = "x86_64")]
mod guarded_fault {
    use std::cell::UnsafeCell;
    use std::sync::Mutex;
    use std::sync::atomic::AtomicI32;
    use std::sync::atomic::AtomicI64;
    use std::sync::atomic::Ordering;

    const SIGNALS: [libc::c_int; 3] = [libc::SIGSEGV, libc::SIGBUS, libc::SIGSYS];

    /// Value placed in `rax` when the instruction faults.
    const FAULTED: u64 = u64::MAX;

    /// Thread id of the thread inside `run`'s guarded instruction, or 0.
    static ARMED_TID: AtomicI64 = AtomicI64::new(0);
    /// Signal that interrupted the guarded instruction, or 0.
    static FAULT_SIGNAL: AtomicI32 = AtomicI32::new(0);
    /// Serializes `run`, which swaps process-wide signal dispositions.
    static RUN: Mutex<()> = Mutex::new(());

    struct Previous(UnsafeCell<[libc::sigaction; SIGNALS.len()]>);
    // SAFETY: written only while `RUN` is held and before the handler that
    // reads it is installed; read only by that handler.
    unsafe impl Sync for Previous {}
    static PREVIOUS: Previous = Previous(UnsafeCell::new(unsafe { std::mem::zeroed() }));

    /// The production probe: IA-32 `getpid` (number 20) through `int 0x80`.
    pub(super) unsafe fn int80_getpid() -> u64 {
        const IA32_NR_GETPID: u64 = 20;
        let result: u64;
        // SAFETY: `int 0x80` either runs the IA-32 getpid, which touches no
        // memory, or faults into `on_fault`, which steps over it.
        unsafe {
            core::arch::asm!(
                "int 0x80",
                inlateout("rax") IA32_NR_GETPID => result,
                lateout("r8") _,
                lateout("r9") _,
                lateout("r10") _,
                lateout("r11") _,
                options(nostack),
            );
        }
        result
    }

    /// Runs `instruction`, which must execute exactly one two-byte `int imm8`
    /// whose result is `rax`, with `SIGSEGV`, `SIGBUS` and `SIGSYS` caught on
    /// this thread. Returns the signal that interrupted it, if any, and `rax`.
    ///
    /// A fault on any other thread in the meantime is passed to the
    /// disposition that was installed before, so this never swallows a real
    /// crash elsewhere in the process.
    pub(super) unsafe fn run(
        instruction: unsafe fn() -> u64,
    ) -> Result<(Option<libc::c_int>, u64), String> {
        let _serial = RUN
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let previous = PREVIOUS.0.get();
        let mut handler: libc::sigaction = unsafe { std::mem::zeroed() };
        handler.sa_sigaction = on_fault as *const () as usize;
        handler.sa_flags = libc::SA_SIGINFO | libc::SA_ONSTACK;
        unsafe { libc::sigemptyset(&mut handler.sa_mask) };

        let mut installed = 0;
        let mut failure = None;
        for (index, signal) in SIGNALS.into_iter().enumerate() {
            // SAFETY: `previous` is only read by `on_fault`, which cannot run
            // for `signal` until this call installs it.
            let slot = unsafe { &mut (*previous)[index] };
            if unsafe { libc::sigaction(signal, &handler, slot) } != 0 {
                failure = Some(format!(
                    "sigaction({signal}) failed: {}",
                    std::io::Error::last_os_error()
                ));
                break;
            }
            installed += 1;
        }

        let mut outcome = Err(String::new());
        if failure.is_none() {
            // A blocked synchronous fault would kill the process instead of
            // reaching the handler.
            let mut unblock: libc::sigset_t = unsafe { std::mem::zeroed() };
            let mut saved_mask: libc::sigset_t = unsafe { std::mem::zeroed() };
            unsafe {
                libc::sigemptyset(&mut unblock);
                for signal in SIGNALS {
                    libc::sigaddset(&mut unblock, signal);
                }
            }
            let masked =
                unsafe { libc::pthread_sigmask(libc::SIG_UNBLOCK, &unblock, &mut saved_mask) };
            if masked != 0 {
                failure = Some(format!(
                    "pthread_sigmask failed: {}",
                    std::io::Error::from_raw_os_error(masked)
                ));
            } else {
                FAULT_SIGNAL.store(0, Ordering::SeqCst);
                ARMED_TID.store(unsafe { libc::syscall(libc::SYS_gettid) }, Ordering::SeqCst);
                let result = unsafe { instruction() };
                ARMED_TID.store(0, Ordering::SeqCst);
                let signal = FAULT_SIGNAL.swap(0, Ordering::SeqCst);
                outcome = Ok(((signal != 0).then_some(signal), result));
                unsafe {
                    libc::pthread_sigmask(libc::SIG_SETMASK, &saved_mask, std::ptr::null_mut())
                };
            }
        }

        for (index, signal) in SIGNALS.into_iter().enumerate().take(installed) {
            let slot = unsafe { &(*previous)[index] };
            unsafe { libc::sigaction(signal, slot, std::ptr::null_mut()) };
        }
        match failure {
            Some(reason) => Err(reason),
            None => outcome,
        }
    }

    extern "C" fn on_fault(
        signal: libc::c_int,
        info: *mut libc::siginfo_t,
        context: *mut libc::c_void,
    ) {
        let tid = unsafe { libc::syscall(libc::SYS_gettid) };
        if tid == ARMED_TID.load(Ordering::SeqCst) {
            // SAFETY: the kernel passes a valid ucontext for SA_SIGINFO, and
            // on this thread the interrupted instruction is the guarded
            // `int imm8` in our own text, so reading its first byte is safe.
            unsafe {
                let context = &mut *context.cast::<libc::ucontext_t>();
                let gregs = &mut context.uc_mcontext.gregs;
                let rip = gregs[libc::REG_RIP as usize];
                // A fault reports the address of the `int`; a seccomp trap
                // (SIGSYS) reports the address after it.
                if signal != libc::SIGSYS && *(rip as *const u8) == 0xcd {
                    gregs[libc::REG_RIP as usize] = rip + 2;
                }
                gregs[libc::REG_RAX as usize] = FAULTED as i64;
            }
            FAULT_SIGNAL.store(signal, Ordering::SeqCst);
            return;
        }
        // SAFETY: the saved disposition was written before this handler
        // was installed.
        unsafe { forward(signal, info, context) };
    }

    /// Passes a fault that is not ours to the disposition saved by `run`.
    unsafe fn forward(signal: libc::c_int, info: *mut libc::siginfo_t, context: *mut libc::c_void) {
        let Some(index) = SIGNALS.iter().position(|&candidate| candidate == signal) else {
            return;
        };
        let previous = unsafe { &(*PREVIOUS.0.get())[index] };
        let action = previous.sa_sigaction;
        if action == libc::SIG_DFL || action == libc::SIG_IGN {
            // Reinstall the old disposition. A synchronous fault re-executes
            // and meets it; a sent signal is re-raised (it stays blocked
            // until this handler returns) so that it is not lost.
            unsafe {
                libc::sigaction(signal, previous, std::ptr::null_mut());
                if !info.is_null() && (*info).si_code <= 0 {
                    libc::raise(signal);
                }
            }
        } else if previous.sa_flags & libc::SA_SIGINFO != 0 {
            let handler: extern "C" fn(libc::c_int, *mut libc::siginfo_t, *mut libc::c_void) =
                unsafe { std::mem::transmute(action) };
            handler(signal, info, context);
        } else {
            let handler: extern "C" fn(libc::c_int) = unsafe { std::mem::transmute(action) };
            handler(signal);
        }
    }

    /// Test-only instruction: `int 0x81` has no user-accessible gate, so it
    /// always raises a general-protection fault (`SIGSEGV`).
    #[cfg(test)]
    pub(super) unsafe fn int81() -> u64 {
        let result: u64;
        unsafe {
            core::arch::asm!(
                "int 0x81",
                inlateout("rax") 0u64 => result,
                lateout("r8") _,
                lateout("r9") _,
                lateout("r10") _,
                lateout("r11") _,
                options(nostack),
            );
        }
        result
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
    fn probe_outcome_classification_names_the_fault() {
        assert_eq!(
            classify_probe_outcome(None, 42, 42, ""),
            Ia32EmulationProbe::Available
        );
        let Ia32EmulationProbe::Unavailable(text) = classify_probe_outcome(
            Some(libc::SIGSEGV),
            u64::MAX,
            42,
            "; kernel command line has ia32_emulation=0",
        ) else {
            panic!("SIGSEGV must classify as unavailable");
        };
        assert!(
            text.contains("SIGSEGV") && text.contains("ia32_emulation=0"),
            "{text}"
        );
        assert!(matches!(
            classify_probe_outcome(Some(libc::SIGSYS), u64::MAX, 42, ""),
            Ia32EmulationProbe::Unavailable(text) if text.contains("SIGSYS")
        ));
        // A serviced entry that answers wrongly (for example -ENOSYS) is not
        // usable either.
        assert!(matches!(
            classify_probe_outcome(None, -38i64 as u64, 42, ""),
            Ia32EmulationProbe::Unavailable(text)
                if text.contains("0xffffffffffffffda") && text.contains("PID 42")
        ));
    }

    #[cfg(target_arch = "x86_64")]
    fn current_disposition(signal: libc::c_int) -> (usize, libc::c_int) {
        let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
        assert_eq!(
            unsafe { libc::sigaction(signal, std::ptr::null(), &mut action) },
            0
        );
        // glibc's sigaction(3) adds SA_RESTORER to every action it installs
        // and points it at its own trampoline; the kernel reports it back.
        // It is not part of the disposition a caller chose.
        const SA_RESTORER: libc::c_int = 0x0400_0000;
        (action.sa_sigaction, action.sa_flags & !SA_RESTORER)
    }

    /// The fault path a host without the IA-32 entry takes: a general
    /// protection fault at the `int` instruction, caught on this thread, with
    /// the process surviving and the old dispositions restored.
    #[cfg(target_arch = "x86_64")]
    #[test]
    fn guarded_fault_catches_a_general_protection_fault_and_restores_handlers() {
        let before = [
            current_disposition(libc::SIGSEGV),
            current_disposition(libc::SIGBUS),
            current_disposition(libc::SIGSYS),
        ];
        for _ in 0..2 {
            let (fault, result) =
                unsafe { guarded_fault::run(guarded_fault::int81) }.expect("guard installs");
            assert_eq!(fault, Some(libc::SIGSEGV));
            assert_eq!(result, u64::MAX, "the faulting instruction was not skipped");
        }
        let after = [
            current_disposition(libc::SIGSEGV),
            current_disposition(libc::SIGBUS),
            current_disposition(libc::SIGSYS),
        ];
        assert_eq!(before, after, "the probe leaked its signal handlers");
    }

    /// The fault path a seccomp filter takes: `SIGSYS` reported after the
    /// instruction. The filter is installed on a scratch thread only.
    #[cfg(target_arch = "x86_64")]
    #[test]
    fn probe_reports_sigsys_from_a_seccomp_trap_on_ia32_syscalls() {
        let (probe, definitive) = std::thread::spawn(|| {
            const AUDIT_ARCH_I386: u32 = 0x4000_0003;
            let filter = [
                // A = seccomp_data.arch
                libc::sock_filter {
                    code: (libc::BPF_LD | libc::BPF_W | libc::BPF_ABS) as u16,
                    jt: 0,
                    jf: 0,
                    k: 4,
                },
                libc::sock_filter {
                    code: (libc::BPF_JMP | libc::BPF_JEQ | libc::BPF_K) as u16,
                    jt: 0,
                    jf: 1,
                    k: AUDIT_ARCH_I386,
                },
                libc::sock_filter {
                    code: (libc::BPF_RET | libc::BPF_K) as u16,
                    jt: 0,
                    jf: 0,
                    k: libc::SECCOMP_RET_TRAP,
                },
                libc::sock_filter {
                    code: (libc::BPF_RET | libc::BPF_K) as u16,
                    jt: 0,
                    jf: 0,
                    k: libc::SECCOMP_RET_ALLOW,
                },
            ];
            let program = libc::sock_fprog {
                len: filter.len() as u16,
                filter: filter.as_ptr().cast_mut(),
            };
            unsafe {
                assert_eq!(libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0), 0);
                assert_eq!(
                    libc::prctl(
                        libc::PR_SET_SECCOMP,
                        libc::SECCOMP_MODE_FILTER,
                        &program as *const libc::sock_fprog,
                    ),
                    0,
                    "install seccomp filter: {}",
                    std::io::Error::last_os_error()
                );
            }
            probe_ia32_emulation_uncached()
        })
        .join()
        .expect("probe thread");
        assert!(definitive, "a fault is a definitive answer");
        assert!(
            matches!(&probe, Ia32EmulationProbe::Unavailable(text) if text.contains("SIGSYS")),
            "{probe:?}"
        );
    }
}
