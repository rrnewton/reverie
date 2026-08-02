/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! End-of-run statistics for the KVM backend.
//!
//! The KVM backend advances the guest with `KVM_RUN`, which returns control to
//! the supervisor on each vCPU exit (a syscall-transport hypercall, a halt, an
//! I/O or MMIO access, and so on). Counting those exits by reason is the natural
//! KVM analogue of the patch/trap counters other Reverie backends expose, and it
//! is the lowest-overhead signal available: the supervisor already inspects every
//! `VcpuExit`, so classification adds one enum map and one increment per exit and
//! nothing on the guest fast path.
//!
//! Collection is opt-in via [`reverie::backend_stats::BackendStatsRequest`]; when
//! the caller disables it, [`KvmExitCollector::record`] is never called and no
//! counters move. The counting is observationally pure — it reads the exit reason
//! the supervisor is about to dispatch and never changes control flow, guest
//! memory, or register state — so it cannot perturb deterministic replay.

use std::collections::BTreeMap;
use std::fmt;

use kvm_ioctls::VcpuExit;
use reverie::backend_stats::BackendStatsSnapshot;
use reverie::backend_stats::CounterSnapshot;

/// A stable, coarse category for a KVM `VcpuExit`.
///
/// The variants are ordered most-informative-first so the deterministic
/// [`CounterSnapshot`] rendering leads with the categories a reader cares about
/// (syscall-transport hypercalls and halts). Every unmodelled exit collapses into
/// [`KvmExitReason::Other`] so the mapping is total and forward-compatible with
/// new `kvm_ioctls` variants.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum KvmExitReason {
    /// A `VMCALL`/`VMMCALL` hypercall — the syscall transport in this backend.
    Hypercall,
    /// A `HLT` — process/thread park or the VMware backdoor probe.
    Hlt,
    /// A port I/O access (`IN`/`OUT`).
    Io,
    /// A memory-mapped I/O access.
    Mmio,
    /// A guest debug event.
    Debug,
    /// An interrupt window opened for pending interrupt delivery.
    IrqWindowOpen,
    /// The guest requested shutdown (triple fault / reset).
    Shutdown,
    /// `KVM_RUN` failed to enter the guest.
    FailEntry,
    /// The kernel reported an internal error.
    InternalError,
    /// A system event (shutdown/reset/crash) surfaced to userspace.
    SystemEvent,
    /// The run was interrupted by a signal delivered to the vCPU thread.
    Intr,
    /// Any other exit reason not modelled above.
    Other,
}

impl fmt::Display for KvmExitReason {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            Self::Hypercall => "hypercall",
            Self::Hlt => "hlt",
            Self::Io => "io",
            Self::Mmio => "mmio",
            Self::Debug => "debug",
            Self::IrqWindowOpen => "irq_window_open",
            Self::Shutdown => "shutdown",
            Self::FailEntry => "fail_entry",
            Self::InternalError => "internal_error",
            Self::SystemEvent => "system_event",
            Self::Intr => "intr",
            Self::Other => "other",
        };
        formatter.write_str(name)
    }
}

impl From<&VcpuExit<'_>> for KvmExitReason {
    fn from(exit: &VcpuExit<'_>) -> Self {
        match exit {
            VcpuExit::Hypercall(_) => Self::Hypercall,
            VcpuExit::Hlt => Self::Hlt,
            VcpuExit::IoIn(..) | VcpuExit::IoOut(..) => Self::Io,
            VcpuExit::MmioRead(..) | VcpuExit::MmioWrite(..) => Self::Mmio,
            VcpuExit::Debug(_) => Self::Debug,
            VcpuExit::IrqWindowOpen => Self::IrqWindowOpen,
            VcpuExit::Shutdown => Self::Shutdown,
            VcpuExit::FailEntry(..) => Self::FailEntry,
            VcpuExit::InternalError => Self::InternalError,
            VcpuExit::SystemEvent(..) => Self::SystemEvent,
            VcpuExit::Intr => Self::Intr,
            _ => Self::Other,
        }
    }
}

/// Accumulates KVM vCPU-exit counts for one backend instance.
///
/// The collector is cheap and always present; it only moves counters while the
/// owning backend's [`reverie::backend_stats::BackendStatsRequest`] is enabled,
/// so a disabled run performs no work here.
#[derive(Clone, Debug, Default)]
pub struct KvmExitCollector {
    counts: BTreeMap<KvmExitReason, u64>,
}

impl KvmExitCollector {
    /// Records one classified vCPU exit.
    pub fn record(&mut self, exit: &VcpuExit<'_>) {
        *self.counts.entry(KvmExitReason::from(exit)).or_insert(0) += 1;
    }

    /// Captures a deterministically ordered snapshot of the counts so far.
    pub fn snapshot(&self) -> KvmBackendStats {
        KvmBackendStats {
            exits: CounterSnapshot::new(
                self.counts.iter().map(|(&reason, &count)| (reason, count)),
            ),
        }
    }
}

/// A stable, displayable end-of-run snapshot of KVM vCPU exits by reason.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct KvmBackendStats {
    exits: CounterSnapshot<KvmExitReason>,
}

impl KvmBackendStats {
    /// Returns the deterministically ordered per-reason exit counts.
    pub fn exits(&self) -> &CounterSnapshot<KvmExitReason> {
        &self.exits
    }

    /// Returns the total number of recorded vCPU exits.
    pub fn total_exits(&self) -> u64 {
        self.exits.total()
    }

    /// Returns the recorded count for a single exit reason.
    pub fn count(&self, reason: KvmExitReason) -> u64 {
        self.exits
            .counts()
            .iter()
            .find(|(candidate, _)| *candidate == reason)
            .map_or(0, |(_, count)| *count)
    }
}

impl fmt::Display for KvmBackendStats {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "kvm backend: {} vCPU exit(s)",
            self.exits.total()
        )?;
        for (reason, count) in self.exits.counts() {
            write!(formatter, "\n  {reason}: {count}")?;
        }
        Ok(())
    }
}

impl BackendStatsSnapshot for KvmBackendStats {
    const BACKEND_NAME: &'static str = "kvm";
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exit_reasons_map_from_vcpu_exits() {
        assert_eq!(KvmExitReason::from(&VcpuExit::Hlt), KvmExitReason::Hlt);
        assert_eq!(
            KvmExitReason::from(&VcpuExit::IrqWindowOpen),
            KvmExitReason::IrqWindowOpen
        );
        assert_eq!(
            KvmExitReason::from(&VcpuExit::Shutdown),
            KvmExitReason::Shutdown
        );
        assert_eq!(KvmExitReason::from(&VcpuExit::Intr), KvmExitReason::Intr);
    }

    #[test]
    fn collector_counts_and_orders_by_reason() {
        let mut collector = KvmExitCollector::default();
        collector.record(&VcpuExit::Hlt);
        collector.record(&VcpuExit::IrqWindowOpen);
        collector.record(&VcpuExit::Hlt);
        collector.record(&VcpuExit::Intr);

        let snapshot = collector.snapshot();
        assert_eq!(snapshot.total_exits(), 4);
        assert_eq!(snapshot.count(KvmExitReason::Hlt), 2);
        assert_eq!(snapshot.count(KvmExitReason::IrqWindowOpen), 1);
        assert_eq!(snapshot.count(KvmExitReason::Intr), 1);
        assert_eq!(snapshot.count(KvmExitReason::Hypercall), 0);

        // Declaration order (most-informative-first) drives the stable rendering:
        // Hlt precedes IrqWindowOpen precedes Intr.
        let reasons: Vec<_> = snapshot
            .exits()
            .counts()
            .iter()
            .map(|(reason, _)| *reason)
            .collect();
        assert_eq!(
            reasons,
            vec![
                KvmExitReason::Hlt,
                KvmExitReason::IrqWindowOpen,
                KvmExitReason::Intr
            ]
        );
    }

    #[test]
    fn snapshot_backend_name_is_kvm() {
        assert_eq!(KvmBackendStats::BACKEND_NAME, "kvm");
    }

    #[test]
    fn display_leads_with_total() {
        let mut collector = KvmExitCollector::default();
        collector.record(&VcpuExit::Hlt);
        let rendered = collector.snapshot().to_string();
        assert!(rendered.starts_with("kvm backend: 1 vCPU exit(s)"));
        assert!(rendered.contains("hlt: 1"));
    }
}
