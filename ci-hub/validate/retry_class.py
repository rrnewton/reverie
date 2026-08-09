#!/usr/bin/env python3
"""Typed, fail-closed classification of a validate run's outcome.

WHY THIS EXISTS — a measured conflation, not a hypothetical one.

`ci-hub/validate/aggregate.py:340-360` decides a run's verdict with a
most-severe-first chain::

    fail > timeout > incomplete > no_result(zero tests) > pass-partial > pass

`interruption_signal` does not appear in that chain. It is WRITTEN by
the former shell producer (:1422, :1525, :1574), asserted by
`hermit/scripts/test_validate_stop_paths.py`, and — measured 2026-08-07 by
grepping the whole parent — **consumed by nothing**. A typed fact is computed,
serialised, and thrown away, which is the same failure mode already recorded on
this task for `StepOutcome`/`step_failure_reason`.

The consequence, counted on the live 654-row ledger: of the 8 runs interrupted
by SIGTERM, **7 are stored `no_result` and 1 is stored `fail`** — an
interruption wearing a product-red badge, with `reclassified_reason` null. The
classifier fails OPEN into product-red, the most damaging direction: a
no-result that reads as a genuine red is what puts an automated revert of a
healthy main one step away.

THE SHAPE OF THE FIX. The offending row is not simply mislabelled — it is a
COMPOUND state. That run really did observe a product failure AND really was
interrupted before finishing its gates. One scalar `result` cannot hold both,
so one fact always dies; today it is the interruption. So the cure is not a
better `if`/`elif` ordering, it is **separate typed axes**:

* :class:`Completion`    — did the run finish under its own power?
* :class:`ProductSignal` — what did it observe about the product?

Those are independent. Every combination is representable, so nothing is
destroyed, and the four classes the drain needs kept apart — NO-RESULT,
CANCELLED, CONTENTION, PRODUCT-RED — are distinguishable by construction rather
than by a precedence accident.

FAIL-CLOSED means one specific thing here: an interrupted run NEVER certifies,
whatever it observed, because it did not finish its gates. It does NOT mean
rewriting its red into a no-result — that would destroy the product observation
in the other direction and could hide a real failure.

This module is a pure function over already-recorded typed fields. It reads no
logs, runs no subprocess, and greps no text — deliberately, since the
toolchain-dependent text-grep classifier is the thing this task exists to stop
relying on.
"""

from __future__ import annotations

import sys
from dataclasses import dataclass
from enum import Enum
from pathlib import Path
from typing import Any, Mapping

# COMPLETENESS IS COVERAGE, NOT A COUNT.
#
# The first version of this module decided GREEN from a positive executed-test
# COUNT. That is the REFUTED completeness signal: a count cannot distinguish
# "ran fewer tests" from "covered fewer nodes". Commit ee303899 carries 8 PASS
# rows at executed=427 that are NOT full greens -- 4 executed test nodes of 19
# planned, 15 absent.
#
# Reading that field here was caught by the executed-test-count consumer
# registry test under ci-hub/tests/, whose classification rule is explicit: a
# completeness or green/qualified key IS the bug, and must be rewritten against
# coverage.* rather than added to the allowlist. So this module no longer names
# that field anywhere -- deliberately, since the registry is a literal grep and
# a file that merely mentions the field still reads as a consumer.
#
# The completeness axis DELEGATES to the shared predicate one directory up
# rather than restating it. A local copy is exactly the drift that predicate
# exists to remove, and the tree already carries one such copy
# (anchor_select.py::_coverage_satisfied); this module does not add a second.
#
# Two coverage axes, different questions, both checked below:
#   coverage.*     per-node: did every planned test node actually execute?
#   full_coverage  profile scope: was this the FULL profile, not a partial
#                  `*-only` profile whose pass reads like a full green?
sys.path.insert(0, str(Path(__file__).resolve().parent.parent))
from qualifying_receipt import coverage_satisfied  # noqa: E402


class Completion(Enum):
    """Did the run reach its own end, and if not, why not?"""

    COMPLETED = "completed"
    """Ran to its own conclusion. Only this value can ever certify."""

    INTERRUPTED = "interrupted"
    """A signal stopped it (the driver's `interruption_signal`). CANCELLED."""

    KILLED_BY_BOUND = "killed-by-bound"
    """A wall/CPU/memory bound killed it. CONTENTION — a resource story."""

    KILLED_BY_SIGNAL = "killed-by-signal"
    """Killed by a signal recorded as such (`killed_by_signal`). CONTENTION."""


class ProductSignal(Enum):
    """What the run managed to observe about the product itself."""

    RED = "red"
    """At least one genuine product failure was counted."""

    GREEN = "green"
    """Zero failures AND per-node coverage satisfied: every planned test node
    actually executed. Not "some tests ran" — that is the refuted count."""

    NONE = "none"
    """Established nothing: no failures counted, and coverage does not show a
    complete run. An uncounted or partially-covered receipt is UNVERIFIED, which
    is deliberately NOT the same as green."""


class RetryClass(Enum):
    """Whether re-running could plausibly produce a different answer."""

    PERMANENT = "permanent"
    """A completed run saw a real red. Retrying re-observes the same red."""

    TRANSIENT = "transient"
    """Stopped by interruption/bound/contention. A retry may well answer."""

    NO_RESULT = "no-result"
    """Completed but established nothing. Retry is the only way to learn."""


@dataclass(frozen=True)
class Outcome:
    """The typed verdict. Both axes are kept; neither overwrites the other."""

    completion: Completion
    product: ProductSignal
    retry: RetryClass
    certifies: bool
    reason: str

    def as_row(self) -> dict[str, Any]:
        """Flat, ledger-friendly projection. Every axis stays its own column."""
        return {
            "completion": self.completion.value,
            "product_signal": self.product.value,
            "retry_class": self.retry.value,
            "certifies": self.certifies,
            "classification_reason": self.reason,
        }


def _int(row: Mapping[str, Any], key: str) -> int:
    value = row.get(key)
    return value if isinstance(value, int) and not isinstance(value, bool) else 0


def classify(row: Mapping[str, Any]) -> Outcome:
    """Classify one validate-ledger row from its TYPED fields only.

    Never inspects log text. Unknown/absent fields are treated as the least
    favourable interpretation, so a row that omits evidence cannot certify.
    """
    # --- axis 1: completion. Most specific cause first; these are disjoint. ---
    if _int(row, "killed_by_bound"):
        completion = Completion.KILLED_BY_BOUND
    elif _int(row, "killed_by_signal"):
        completion = Completion.KILLED_BY_SIGNAL
    elif row.get("interruption_signal"):
        completion = Completion.INTERRUPTED
    else:
        completion = Completion.COMPLETED

    # --- axis 2: what it saw. Independent of whether it finished. ---
    failures = _int(row, "failures")
    if failures > 0:
        product = ProductSignal.RED
    elif coverage_satisfied(row.get("coverage")):
        product = ProductSignal.GREEN
    else:
        # No failures, but coverage does not demonstrate a complete run: absent
        # or zero-executed nodes, no planned test node, or no coverage at all.
        # `coverage_satisfied` is fail-closed on a missing/partial object, which
        # is what we want -- absence of evidence is not a green.
        product = ProductSignal.NONE

    # --- retry class ---
    if completion is not Completion.COMPLETED:
        retry = RetryClass.TRANSIENT
    elif product is ProductSignal.RED:
        retry = RetryClass.PERMANENT
    elif product is ProductSignal.NONE:
        retry = RetryClass.NO_RESULT
    else:
        retry = RetryClass.PERMANENT  # a completed green needs no retry

    # --- certification: FAIL-CLOSED. Every clause must hold. ---
    certifies = (
        completion is Completion.COMPLETED
        and product is ProductSignal.GREEN
        and bool(row.get("full_coverage"))
        and row.get("result") not in (None, "")
    )

    if completion is not Completion.COMPLETED and product is ProductSignal.RED:
        reason = (
            f"{completion.value} AND a product red was observed; the red is "
            "retained but the run cannot certify (it did not finish its gates)"
        )
    elif completion is not Completion.COMPLETED:
        reason = f"{completion.value}; no product verdict was established"
    elif product is ProductSignal.RED:
        reason = "completed run counted a product failure"
    elif product is ProductSignal.NONE:
        reason = (
            "completed with no failures, but per-node coverage does not show a "
            "complete run (absent/zero-executed nodes, or no coverage reported); "
            "establishes nothing"
        )
    elif not certifies:
        reason = "coverage satisfied but not the full profile; cannot certify"
    else:
        reason = "completed, full profile, per-node coverage satisfied, zero failures"

    return Outcome(completion, product, retry, certifies, reason)
