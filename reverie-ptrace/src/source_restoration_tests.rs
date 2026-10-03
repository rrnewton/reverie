/* Copyright (c) Meta Platforms, Inc. and affiliates.
 * Licensed under the BSD-style license in the root LICENSE file. */

// Metadata refusal controls only. Actual register writes and restoration after
// real private ENTRY/EXIT require the separately admitted native fixture.
use super::*;

#[test]
fn checked_restoration_requires_the_only_exact_stopped_native_invocation() {
    assert!(checked_return_phase(
        Life::Stopped,
        1,
        Some(7),
        7,
        Effect::Native,
        Outcome::Stopped
    ));
    for life in [
        Life::Initializing,
        Life::Executing,
        Life::Exiting,
        Life::Terminal,
    ] {
        assert!(!checked_return_phase(
            life,
            1,
            Some(7),
            7,
            Effect::Native,
            Outcome::Stopped
        ));
    }
    for count in [0, 2, usize::MAX] {
        assert!(!checked_return_phase(
            Life::Stopped,
            count,
            Some(7),
            7,
            Effect::Native,
            Outcome::Stopped
        ));
    }
    for invocation in [None, Some(6), Some(8)] {
        assert!(!checked_return_phase(
            Life::Stopped,
            1,
            invocation,
            7,
            Effect::Native,
            Outcome::Stopped
        ));
    }
    for effect in [
        Effect::Execution,
        Effect::Birth,
        Effect::Exec,
        Effect::Terminal,
        Effect::Unknown,
    ] {
        assert!(!checked_return_phase(
            Life::Stopped,
            1,
            Some(7),
            7,
            effect,
            Outcome::Stopped
        ));
    }
    for outcome in [Outcome::Waiting, Outcome::Unknown] {
        assert!(!checked_return_phase(
            Life::Stopped,
            1,
            Some(7),
            7,
            Effect::Native,
            outcome
        ));
    }
}
