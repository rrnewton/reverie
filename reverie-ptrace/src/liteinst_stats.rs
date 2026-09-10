/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! Aggregate statistics for dynamically installed LiteInst patch sites.

use std::sync::Arc;
use std::sync::Mutex;

pub use reverie::liteinst_stats::LiteinstInstrumentationStats;
pub(crate) use reverie::liteinst_stats::LiteinstPatchOutcome;

/// Cloneable observer for instrumentation statistics owned by a running tracer.
#[derive(Clone, Debug)]
pub struct LiteinstInstrumentationStatsHandle {
    inner: Arc<Mutex<LiteinstInstrumentationStats>>,
}

impl LiteinstInstrumentationStatsHandle {
    pub(crate) fn from_shared(inner: Arc<Mutex<LiteinstInstrumentationStats>>) -> Self {
        Self { inner }
    }

    /// Returns a consistent snapshot of all sites installed so far.
    pub fn snapshot(&self) -> LiteinstInstrumentationStats {
        self.inner
            .lock()
            .expect("LiteInst instrumentation statistics lock poisoned")
            .clone()
    }
}

pub(crate) fn with_liteinst_stats<R>(
    stats: Option<&Arc<Mutex<LiteinstInstrumentationStats>>>,
    record: impl FnOnce(&mut LiteinstInstrumentationStats) -> R,
) -> Option<R> {
    let stats = stats?;
    Some(record(
        &mut stats
            .lock()
            .expect("LiteInst instrumentation statistics lock poisoned"),
    ))
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::sync::Arc;
    use std::sync::Mutex;

    use super::LiteinstInstrumentationStats;
    use super::LiteinstInstrumentationStatsHandle;
    use super::LiteinstPatchOutcome;
    use super::with_liteinst_stats;

    #[test]
    fn disabled_collection_does_not_run_stats_only_classification() {
        let classified = Cell::new(false);
        let result = with_liteinst_stats(None, |_| classified.set(true));

        assert!(result.is_none());
        assert!(!classified.get());
    }

    #[test]
    fn old_stats_path_and_host_handle_keep_shared_type_and_counts() {
        let original = reverie::liteinst_stats::LiteinstInstrumentationStats::default();
        let old: crate::LiteinstInstrumentationStats = original;
        let shared: reverie::liteinst_stats::LiteinstInstrumentationStats = old;
        let state = Arc::new(Mutex::new(shared));
        let handle = LiteinstInstrumentationStatsHandle::from_shared(Arc::clone(&state));
        with_liteinst_stats(Some(&state), |stats| {
            stats.record_process_site(
                11,
                0,
                0x1000,
                LiteinstPatchOutcome::RelocatedPatched,
                Some((2, None)),
            );
            stats.record_direct_hook();
        });
        let snapshot: LiteinstInstrumentationStats = handle.snapshot();
        assert_eq!(snapshot.patch_candidates(), 1);
        assert_eq!(snapshot.decision_counts(), [0, 1, 0, 0]);
        assert_eq!(snapshot.dispatch_path_counts(), [0, 0, 0, 0, 1]);
        assert_eq!(snapshot.instruction_length_counts(), [0, 0, 0, 1, 0]);
    }
}
