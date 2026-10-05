/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! A crate that drives Reverie's precise timers can read the PMU validation
//! result and act on it, rather than finding it only in a log line. This test
//! uses only the public API, as such a crate would.

use reverie_ptrace::PmuValidationError;
use reverie_ptrace::pmu_validation;

#[test]
fn pmu_validation_is_public_and_stable() {
    let first = pmu_validation();
    assert_eq!(pmu_validation(), first);
    if let Err(error) = &first {
        assert!(!error.name().is_empty());
        assert!(!error.to_string().is_empty());
    }
}

/// A caller outside the crate can construct the SpecLockMap result, which is
/// what lets it test its own refusal path on a host without the defect, and can
/// match it by variant.
#[cfg(target_arch = "x86_64")]
#[test]
fn the_speclockmap_result_is_constructible_and_matchable() {
    let error = PmuValidationError::AmdSpecLockMapShouldBeDisabled;
    assert_eq!(error.name(), "AmdSpecLockMapShouldBeDisabled");
    assert!(error.to_string().contains("SpecLockMap"));
    assert!(matches!(
        error,
        PmuValidationError::AmdSpecLockMapShouldBeDisabled
    ));
}
