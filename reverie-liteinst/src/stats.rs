/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! Typed end-of-run statistics owned by the LiteInst backend.

use std::collections::BTreeSet;
use std::fmt;
use std::io;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::OnceLock;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;

use reverie::BackendStatsSnapshot;
use reverie::BackendStatsSource;
use reverie::CounterSnapshot;
use reverie::GlobalTool;
use reverie::InstructionPatchShape;
use reverie::PatchShapeCollector;
use reverie::PatchShapeStats;
use reverie::Tid;
use reverie_rpc_transport::BlockingRpcClient;
use serde::Deserialize;
use serde::Serialize;

/// A distinct outcome for one candidate LiteInst patch site.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum LiteinstPatchDecision {
    /// A direct pun patch was installed.
    DirectPun,
    /// A replace-first relocation patch was installed.
    Relocated,
    /// The original syscall site was retained because its patch crossed a cache line.
    StraddlerFallback,
    /// The original syscall site was retained for another unpatchable-site reason.
    OtherFallback,
}

/// A dispatch or installation path taken by a LiteInst runtime.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum LiteinstDispatchPath {
    /// A ptrace `Event::Seccomp` at a previously unseen site.
    FirstSiteSeccomp,
    /// A successful stopped-tracee patch installation performed through ptrace.
    PtraceInstallation,
    /// An actual in-guest `SIGSYS` entered the patch dispatcher.
    InGuestSigsys,
    /// An actual in-guest `SIGSYS` was forwarded while a Tool callback was active.
    InGuestNestedSigsys,
    /// A cache-line-straddling site that retained the ptrace fallback.
    CachelineStraddlerFallback,
    /// An unpatchable or otherwise rejected site that retained the ptrace fallback.
    UnpatchableOrOtherFallback,
    /// A patched-site callback that returned to the ptrace-host Tool through SIGTRAP.
    DirectHook,
}

/// Stable LiteInst statistics captured after one backend run.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LiteinstBackendStatsSnapshot {
    patch_shapes: PatchShapeStats,
    patch_decisions: CounterSnapshot<LiteinstPatchDecision>,
    dispatch_paths: CounterSnapshot<LiteinstDispatchPath>,
}

impl LiteinstBackendStatsSnapshot {
    /// Aggregate shape distribution over distinct patch-site identities.
    ///
    /// Collection deduplicates by process, exec generation, and virtual RIP.
    /// Those raw identities are discarded before this aggregate is constructed.
    pub const fn patch_shapes(&self) -> &PatchShapeStats {
        &self.patch_shapes
    }

    /// Patch decisions in deterministic enum order.
    pub const fn patch_decisions(&self) -> &CounterSnapshot<LiteinstPatchDecision> {
        &self.patch_decisions
    }

    /// Dispatch-path counts in deterministic enum order.
    pub const fn dispatch_paths(&self) -> &CounterSnapshot<LiteinstDispatchPath> {
        &self.dispatch_paths
    }

    fn decision_count(&self, decision: LiteinstPatchDecision) -> u64 {
        count(&self.patch_decisions, decision)
    }

    fn path_count(&self, path: LiteinstDispatchPath) -> u64 {
        count(&self.dispatch_paths, path)
    }
}

impl fmt::Display for LiteinstBackendStatsSnapshot {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "LiteInst instrumentation stats: distinct_rips_patched={} patch_candidates={} decisions[direct_pun={},relocated={},straddler_fallback={},other_fallback={}] paths[first_site_seccomp={},ptrace_installation={},in_guest_sigsys={},in_guest_nested_sigsys={},cacheline_straddler={},unpatchable_or_other={},direct_hook={}] classified_candidates={} cacheline_straddlers={} non_straddling={} instruction_lengths[",
            self.patch_shapes.patched_rips(),
            self.patch_shapes.candidate_rips(),
            self.decision_count(LiteinstPatchDecision::DirectPun),
            self.decision_count(LiteinstPatchDecision::Relocated),
            self.decision_count(LiteinstPatchDecision::StraddlerFallback),
            self.decision_count(LiteinstPatchDecision::OtherFallback),
            self.path_count(LiteinstDispatchPath::FirstSiteSeccomp),
            self.path_count(LiteinstDispatchPath::PtraceInstallation),
            self.path_count(LiteinstDispatchPath::InGuestSigsys),
            self.path_count(LiteinstDispatchPath::InGuestNestedSigsys),
            self.path_count(LiteinstDispatchPath::CachelineStraddlerFallback),
            self.path_count(LiteinstDispatchPath::UnpatchableOrOtherFallback),
            self.path_count(LiteinstDispatchPath::DirectHook),
            self.patch_shapes.classified_candidates(),
            self.patch_shapes.cacheline_straddlers(),
            self.patch_shapes.non_straddling(),
        )?;
        write_buckets(formatter, self.patch_shapes.instruction_lengths())?;
        formatter.write_str("] straddle_prefix[")?;
        write_buckets(formatter, self.patch_shapes.straddle_after())?;
        formatter.write_str("]")
    }
}

impl BackendStatsSnapshot for LiteinstBackendStatsSnapshot {
    const BACKEND_NAME: &'static str = "liteinst";
}

/// Backend-owned source for a typed LiteInst end-of-run snapshot.
#[derive(Clone, Debug)]
pub struct LiteinstBackendStatsSource {
    snapshot: LiteinstBackendStatsSnapshot,
}

impl LiteinstBackendStatsSource {
    pub(crate) fn from_ptrace_host_hybrid(
        stats: reverie_ptrace::LiteinstInstrumentationStats,
    ) -> Self {
        let decisions = stats.decision_counts();
        let paths = stats.dispatch_path_counts();
        Self {
            snapshot: LiteinstBackendStatsSnapshot {
                patch_shapes: stats.patch_shape_stats(),
                patch_decisions: CounterSnapshot::new([
                    (LiteinstPatchDecision::DirectPun, decisions[0]),
                    (LiteinstPatchDecision::Relocated, decisions[1]),
                    (LiteinstPatchDecision::StraddlerFallback, decisions[2]),
                    (LiteinstPatchDecision::OtherFallback, decisions[3]),
                ]),
                dispatch_paths: CounterSnapshot::new([
                    (LiteinstDispatchPath::FirstSiteSeccomp, paths[0]),
                    (LiteinstDispatchPath::PtraceInstallation, paths[1]),
                    (LiteinstDispatchPath::CachelineStraddlerFallback, paths[2]),
                    (LiteinstDispatchPath::UnpatchableOrOtherFallback, paths[3]),
                    (LiteinstDispatchPath::DirectHook, paths[4]),
                ]),
            },
        }
    }

    /// Returns the captured snapshot without performing another collection pass.
    pub const fn snapshot(&self) -> &LiteinstBackendStatsSnapshot {
        &self.snapshot
    }

    /// Returns the number of distinct instruction pointers successfully patched.
    pub fn distinct_rips(&self) -> usize {
        self.snapshot.patch_shapes.patched_rips() as usize
    }

    /// Returns distinct patch candidates, including fallback sites.
    pub fn patch_candidates(&self) -> usize {
        self.snapshot.patch_shapes.candidate_rips() as usize
    }

    /// Returns direct, relocated, straddler-fallback, and other-fallback counts.
    pub fn decision_counts(&self) -> [usize; 4] {
        [
            self.snapshot
                .decision_count(LiteinstPatchDecision::DirectPun) as usize,
            self.snapshot
                .decision_count(LiteinstPatchDecision::Relocated) as usize,
            self.snapshot
                .decision_count(LiteinstPatchDecision::StraddlerFallback) as usize,
            self.snapshot
                .decision_count(LiteinstPatchDecision::OtherFallback) as usize,
        ]
    }

    /// Returns dispatch-path counts in [`LiteinstDispatchPath`] order.
    pub fn dispatch_path_counts(&self) -> [u64; 7] {
        [
            self.snapshot
                .path_count(LiteinstDispatchPath::FirstSiteSeccomp),
            self.snapshot
                .path_count(LiteinstDispatchPath::PtraceInstallation),
            self.snapshot
                .path_count(LiteinstDispatchPath::InGuestSigsys),
            self.snapshot
                .path_count(LiteinstDispatchPath::InGuestNestedSigsys),
            self.snapshot
                .path_count(LiteinstDispatchPath::CachelineStraddlerFallback),
            self.snapshot
                .path_count(LiteinstDispatchPath::UnpatchableOrOtherFallback),
            self.snapshot.path_count(LiteinstDispatchPath::DirectHook),
        ]
    }

    /// Returns candidates with a decoded instruction shape.
    pub fn classified_candidates(&self) -> usize {
        self.snapshot.patch_shapes.classified_candidates() as usize
    }

    /// Returns decoded candidates whose patch prefix crosses a cache line.
    pub fn cacheline_straddlers(&self) -> usize {
        self.snapshot.patch_shapes.cacheline_straddlers() as usize
    }

    /// Returns decoded candidates whose patch prefix stays within a cache line.
    pub fn non_straddling(&self) -> usize {
        self.snapshot.patch_shapes.non_straddling() as usize
    }

    /// Returns instruction-length counts ordered as 5+, 4, 3, 2, and 1 byte.
    pub fn instruction_length_counts(&self) -> [usize; 5] {
        let lengths = self.snapshot.patch_shapes.instruction_lengths();
        [
            lengths[4..].iter().sum::<u64>() as usize,
            lengths[3] as usize,
            lengths[2] as usize,
            lengths[1] as usize,
            lengths[0] as usize,
        ]
    }

    /// Returns straddler counts for boundaries after 1, 2, 3, and 4 bytes.
    pub fn straddle_prefix_counts(&self) -> [usize; 4] {
        let prefixes = self.snapshot.patch_shapes.straddle_after();
        [
            prefixes[0] as usize,
            prefixes[1] as usize,
            prefixes[2] as usize,
            prefixes[3] as usize,
        ]
    }
}

pub(crate) const IN_GUEST_PATH_COUNT: usize = 5;
pub(crate) const IN_GUEST_SIGSYS: usize = 0;
pub(crate) const IN_GUEST_NESTED_SIGSYS: usize = 1;
pub(crate) const IN_GUEST_STRADDLER_FALLBACK: usize = 2;
pub(crate) const IN_GUEST_OTHER_FALLBACK: usize = 3;
pub(crate) const IN_GUEST_DIRECT_HOOK: usize = 4;

struct GuestStatsCollector {
    coordinator: PathBuf,
    paths: [AtomicU64; IN_GUEST_PATH_COUNT],
}

static GUEST_STATS: OnceLock<GuestStatsCollector> = OnceLock::new();

pub(crate) fn initialize_guest_stats(coordinator: &Path) -> io::Result<()> {
    GUEST_STATS
        .set(GuestStatsCollector {
            coordinator: coordinator.to_path_buf(),
            paths: std::array::from_fn(|_| AtomicU64::new(0)),
        })
        .map_err(|_| {
            io::Error::new(
                io::ErrorKind::AlreadyExists,
                "LiteInst statistics initialized twice",
            )
        })
}

pub(crate) fn guest_stats_enabled() -> bool {
    GUEST_STATS.get().is_some()
}

pub(crate) fn record_guest_path(path: usize) {
    if let Some(stats) = GUEST_STATS.get() {
        stats.paths[path].fetch_add(1, Ordering::Relaxed);
    }
}

pub(crate) fn reset_guest_stats_after_fork() {
    if let Some(stats) = GUEST_STATS.get() {
        for count in &stats.paths {
            count.store(0, Ordering::Relaxed);
        }
    }
}

pub(crate) fn submit_guest_stats(
    tid: Tid,
    process_identity: u64,
    sites: Vec<LiteinstProcessSiteStats>,
) -> io::Result<()> {
    let Some(stats) = GUEST_STATS.get() else {
        return Ok(());
    };
    let paths = std::array::from_fn(|index| stats.paths[index].load(Ordering::Relaxed));
    let client = BlockingRpcClient::<LiteinstStatsGlobal>::connect(&stats.coordinator, tid)
        .map_err(|error| io::Error::other(error.to_string()))?;
    client
        .try_send_rpc(LiteinstProcessStats {
            process_identity,
            execution_generation: 0,
            paths,
            sites,
        })
        .map_err(|error| io::Error::other(error.to_string()))
}

/// One process-local patch-site observation sent only after an enabled run.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct LiteinstProcessSiteStats {
    pub(crate) rip: u64,
    pub(crate) patched: bool,
    pub(crate) instruction_length: u8,
    pub(crate) straddle_after: u8,
}

/// One process-local, post-Tool-exit statistics message.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct LiteinstProcessStats {
    pub(crate) process_identity: u64,
    pub(crate) execution_generation: u64,
    pub(crate) paths: [u64; IN_GUEST_PATH_COUNT],
    pub(crate) sites: Vec<LiteinstProcessSiteStats>,
}

/// Coordinator-side typed RPC target for per-process LiteInst snapshots.
#[derive(Debug, Default)]
pub(crate) struct LiteinstStatsGlobal {
    processes: Mutex<Vec<LiteinstProcessStats>>,
}

#[reverie::global_tool]
impl GlobalTool for LiteinstStatsGlobal {
    type Request = LiteinstProcessStats;
    type Response = ();
    type Config = ();

    async fn receive_rpc(&self, from: Tid, mut message: Self::Request) {
        // The supported lifecycle is single-threaded plain fork, so the
        // connection's stamped TID is also the process identity. Do not trust a
        // guest-supplied identity when the transport already authenticated one.
        message.process_identity = from.as_raw() as u64;
        self.processes.lock().unwrap().push(message);
    }
}

impl LiteinstStatsGlobal {
    pub(crate) fn into_source(self) -> LiteinstBackendStatsSource {
        let processes = self.processes.into_inner().unwrap();
        let mut shapes = PatchShapeCollector::default();
        let mut seen_sites = BTreeSet::new();
        let mut decisions = [0_u64; 4];
        let mut paths = [0_u64; 7];

        for process in processes {
            paths[2] += process.paths[IN_GUEST_SIGSYS];
            paths[3] += process.paths[IN_GUEST_NESTED_SIGSYS];
            paths[4] += process.paths[IN_GUEST_STRADDLER_FALLBACK];
            paths[5] += process.paths[IN_GUEST_OTHER_FALLBACK];
            paths[6] += process.paths[IN_GUEST_DIRECT_HOOK];
            for site in process.sites {
                if !seen_sites.insert((
                    process.process_identity,
                    process.execution_generation,
                    site.rip,
                )) {
                    continue;
                }
                let shape = (site.instruction_length != 0).then(|| {
                    InstructionPatchShape::new(
                        site.instruction_length,
                        (site.straddle_after != 0).then_some(site.straddle_after),
                    )
                });
                if site.patched {
                    decisions[1] += 1;
                } else if site.straddle_after != 0 {
                    decisions[2] += 1;
                } else {
                    decisions[3] += 1;
                }
                shapes.record_process_site(
                    process.process_identity,
                    process.execution_generation,
                    site.rip,
                    site.patched,
                    shape,
                );
            }
        }

        LiteinstBackendStatsSource {
            snapshot: LiteinstBackendStatsSnapshot {
                patch_shapes: shapes.snapshot(),
                patch_decisions: CounterSnapshot::new([
                    (LiteinstPatchDecision::DirectPun, decisions[0]),
                    (LiteinstPatchDecision::Relocated, decisions[1]),
                    (LiteinstPatchDecision::StraddlerFallback, decisions[2]),
                    (LiteinstPatchDecision::OtherFallback, decisions[3]),
                ]),
                dispatch_paths: CounterSnapshot::new([
                    (LiteinstDispatchPath::FirstSiteSeccomp, paths[0]),
                    (LiteinstDispatchPath::PtraceInstallation, paths[1]),
                    (LiteinstDispatchPath::InGuestSigsys, paths[2]),
                    (LiteinstDispatchPath::InGuestNestedSigsys, paths[3]),
                    (LiteinstDispatchPath::CachelineStraddlerFallback, paths[4]),
                    (LiteinstDispatchPath::UnpatchableOrOtherFallback, paths[5]),
                    (LiteinstDispatchPath::DirectHook, paths[6]),
                ]),
            },
        }
    }
}

impl fmt::Display for LiteinstBackendStatsSource {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.snapshot.fmt(formatter)
    }
}

impl BackendStatsSource for LiteinstBackendStatsSource {
    type Snapshot = LiteinstBackendStatsSnapshot;

    fn backend_stats(&self) -> Self::Snapshot {
        self.snapshot.clone()
    }
}

fn count<K: Copy + Eq + Ord>(snapshot: &CounterSnapshot<K>, key: K) -> u64 {
    snapshot
        .counts()
        .iter()
        .find_map(|(candidate, count)| (*candidate == key).then_some(*count))
        .unwrap_or(0)
}

fn write_buckets(formatter: &mut fmt::Formatter<'_>, buckets: &[u64]) -> fmt::Result {
    for (index, count) in buckets.iter().enumerate() {
        if index != 0 {
            formatter.write_str(",")?;
        }
        write!(formatter, "{}={count}", index + 1)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use reverie::InstructionPatchShape;
    use reverie::PatchShapeCollector;

    use super::*;

    #[test]
    fn display_is_deterministic_and_contains_no_raw_identity() {
        let mut shapes = PatchShapeCollector::default();
        shapes.record_site(
            0x7fff_1234_5678,
            true,
            Some(InstructionPatchShape::new(2, None)),
        );
        let source = LiteinstBackendStatsSource {
            snapshot: LiteinstBackendStatsSnapshot {
                patch_shapes: shapes.snapshot(),
                patch_decisions: CounterSnapshot::new([(LiteinstPatchDecision::Relocated, 1)]),
                dispatch_paths: CounterSnapshot::new([
                    (LiteinstDispatchPath::FirstSiteSeccomp, 1),
                    (LiteinstDispatchPath::PtraceInstallation, 1),
                    (LiteinstDispatchPath::DirectHook, 9),
                ]),
            },
        };

        let rendered = source.snapshot().to_string();
        assert_eq!(rendered, source.snapshot().to_string());
        assert!(rendered.contains("distinct_rips_patched=1"));
        assert!(rendered.contains("paths[first_site_seccomp=1"));
        assert!(rendered.contains("direct_hook=9"));
        assert!(!rendered.contains("0x7fff"));
        assert!(!rendered.contains("pid="));
        assert!(!rendered.contains("time="));
    }

    #[tokio::test]
    async fn aggregates_equal_rips_from_distinct_fork_processes_and_exact_hits() {
        let global = LiteinstStatsGlobal::default();
        for (pid, direct_hooks) in [(101, 7), (202, 11)] {
            global
                .receive_rpc(
                    Tid::from_raw(pid),
                    LiteinstProcessStats {
                        process_identity: 999,
                        execution_generation: 0,
                        paths: [1, 2, 3, 4, direct_hooks],
                        sites: vec![LiteinstProcessSiteStats {
                            rip: 0x4000,
                            patched: true,
                            instruction_length: 2,
                            straddle_after: 0,
                        }],
                    },
                )
                .await;
        }

        let source = global.into_source();
        assert_eq!(source.patch_candidates(), 2);
        assert_eq!(source.distinct_rips(), 2);
        assert_eq!(source.decision_counts(), [0, 2, 0, 0]);
        assert_eq!(source.dispatch_path_counts(), [0, 0, 2, 4, 6, 8, 18]);
        let rendered = source.to_string();
        assert!(rendered.contains("first_site_seccomp=0"), "{rendered}");
        assert!(rendered.contains("in_guest_sigsys=2"), "{rendered}");
        assert!(rendered.contains("in_guest_nested_sigsys=4"), "{rendered}");
        assert!(!rendered.contains("pid="), "{rendered}");
        assert!(!rendered.contains("0x4000"), "{rendered}");
    }
}
