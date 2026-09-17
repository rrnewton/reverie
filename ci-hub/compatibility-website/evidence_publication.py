#!/usr/bin/env python3
"""Decide whether a cell's evidence may be published, and say WHY when it may not.

A compatibility test-detail page showing stdout for a cell READS AS A RESULT.
That is the whole hazard: a reader cannot tell, from bytes on a page, whether the
run that produced them finished, was killed half-way, or never started at all.
This module makes that distinction before anything is written, and keeps the
negative answers DISTINCT from one another.

⚠️ THE FOUR NEGATIVE ANSWERS ARE NOT INTERCHANGEABLE AND COLLAPSING THEM IS THE
DEFECT THIS EXISTS TO PREVENT. "we captured nothing", "we captured it and it is
too large to publish", "the run never completed" and "we cannot tell what the run
did" are four different statements about the world, and only the first is what a
reader assumes when a page says nothing. The task's own acceptance requires
missing evidence to render explicitly as not recorded; the other three are just
as capable of being mistaken for it.

This is the same defect class as a run record that carries ``admission_result:
null`` -- a field that reads as present, is empty, and makes a downstream
decision impossible. There the consequence was that validate cleanup refused
forever; here it would be a page asserting a result that never happened.

WHY ADMISSION IS CONSULTED AND NOT ONLY STATE
---------------------------------------------
A REFUSED admission proves no validation payload ran. Any bytes lying beside such
a record belong to something else -- a previous attempt in a reused directory, or
setup output -- and are not that cell's result whatever the run state says. A
record with NO admission result at all cannot be reasoned about in either
direction and is refused rather than guessed, for the same reason the cleanup
rule refuses it: a tool must not infer a field it never observed.

WHAT THIS MODULE DOES NOT DO
----------------------------
It decides publication for ONE RUN RECORD. Associating a particular cell with the
run and attempt that produced it is the job of the cell evidence binding, which
is not on main at the time of writing. So this answers "may this run's evidence
be published", not "which run does this cell's evidence belong to"; the second
question needs the binding and is deliberately absent rather than approximated.
"""

from __future__ import annotations

import enum
import sys
from dataclasses import dataclass
from pathlib import Path
from typing import Any, Final, Mapping


sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "validate"))
import run_registry  # noqa: E402


INFO_LOG_PUBLICATION_BOUND_BYTES: Final = 1024 * 1024
"""Per-cell bound on a published INFO log, set against the measured tail.

Measured 2026-09-17 over 394 retained cells: p50 70,752 B, p90 545,908 B, p99
95,442,080 B, max 95,442,120 B. The distribution is heavy-tailed -- the mean is
3.1 MB, 44x the median -- so a bound set against the median would look generous
and fail on the first pathological cell. 1 MiB admits roughly 90% of cells
complete.

⚠️ A CAPPED LOG IS NOT PUBLISHED TRUNCATED. Truncation is on this task's list of
things the negative tests must reject, and a half a log is worse than none: it
reads as the whole thing. An over-bound log is recorded as present, digested at
its TRUE length, and rendered as not published -- which is a different sentence
from not recorded.

stdout and stderr have no bound: the worst case across 710 cells is about 64 MB
even if every cell hit the observed stderr maximum of 90,084 B, and 2.41 MB at
the measured mean. No bound is worth its complexity there.
"""


class Publication(enum.Enum):
    """What a page may say about one piece of cell evidence."""

    PUBLISH = "publish"
    NOT_RECORDED = "not-recorded"
    NOT_PUBLISHED_TOO_LARGE = "not-published-too-large"
    RUN_DID_NOT_COMPLETE = "run-did-not-complete"
    RUN_OUTCOME_UNDECIDABLE = "run-outcome-undecidable"


#: States in which the validation payload ran to an end and its output IS the
#: result. FAILED belongs here deliberately: a failing cell's output is the most
#: useful thing the page can show, and withholding it would be the mistake.
_COMPLETED_STATES: Final = frozenset(
    {run_registry.RunState.COMPLETED, run_registry.RunState.FAILED}
)

#: States in which the payload provably did not run to an end. KILLED is here
#: because its evidence is a PARTIAL capture of an interrupted run, which reads
#: as a complete one.
_INCOMPLETE_STATES: Final = frozenset(
    {
        run_registry.RunState.PREPARING,
        run_registry.RunState.LAUNCHING,
        run_registry.RunState.RUNNING,
        run_registry.RunState.REFUSED,
        run_registry.RunState.KILLED,
    }
)


@dataclass(frozen=True)
class Decision:
    """A publication verdict and the sentence a page should render."""

    publication: Publication
    reason: str

    @property
    def may_publish(self) -> bool:
        return self.publication is Publication.PUBLISH


def run_permits_publication(record: Mapping[str, Any]) -> Decision:
    """May this run's captured evidence be shown as a result?

    Parsed through ``run_registry.parse_current_record`` rather than by reading
    keys, so an unknown or malformed shape is refused by the one parser that owns
    the contract instead of being half-understood here.
    """

    try:
        handle = run_registry.parse_current_record(record)
    except RuntimeError as error:
        return Decision(
            Publication.RUN_OUTCOME_UNDECIDABLE,
            f"the run handle could not be read: {error}",
        )

    admission = handle.admission_result
    if admission is None:
        # The exact shape that blocks validate cleanup forever. Publishing on it
        # would be the same guess in the other direction.
        return Decision(
            Publication.RUN_OUTCOME_UNDECIDABLE,
            "the run handle records no admission result, so whether the "
            "validation payload ever started cannot be determined",
        )
    if admission.state is run_registry.AdmissionState.REFUSED:
        return Decision(
            Publication.RUN_DID_NOT_COMPLETE,
            "validation admission was refused, so no payload ran and these "
            "bytes are not this cell's result",
        )

    if handle.state in _COMPLETED_STATES:
        return Decision(Publication.PUBLISH, f"the run reached {handle.state.value}")
    if handle.state in _INCOMPLETE_STATES:
        return Decision(
            Publication.RUN_DID_NOT_COMPLETE,
            f"the run is recorded as {handle.state.value} rather than completed",
        )
    return Decision(
        Publication.RUN_OUTCOME_UNDECIDABLE,
        f"the run state {handle.state.value!r} does not decide whether the "
        "payload ran to an end",
    )


def classify_evidence(
    *,
    record: Mapping[str, Any],
    recorded_byte_length: int | None,
    bound_bytes: int | None = INFO_LOG_PUBLICATION_BOUND_BYTES,
) -> Decision:
    """Combine run outcome, presence and the size bound into one verdict.

    ``recorded_byte_length`` is ``None`` when nothing was captured, which is the
    only condition that may render as "not recorded".

    ⚠️ THE RUN OUTCOME IS CHECKED FIRST, AND THE ORDER IS LOAD-BEARING. Asking
    about size first would report a killed run's partial 2 MB log as "too large
    to publish" -- a sentence that says the bytes are fine and merely bulky, when
    the truth is that they are not a result at all.
    """

    outcome = run_permits_publication(record)
    if not outcome.may_publish:
        return outcome
    if recorded_byte_length is None:
        return Decision(
            Publication.NOT_RECORDED, "this evidence was not captured for the run"
        )
    if recorded_byte_length < 0:
        return Decision(
            Publication.RUN_OUTCOME_UNDECIDABLE,
            f"recorded byte length {recorded_byte_length} is not a length",
        )
    if bound_bytes is not None and recorded_byte_length > bound_bytes:
        return Decision(
            Publication.NOT_PUBLISHED_TOO_LARGE,
            f"recorded at {recorded_byte_length} bytes, above the "
            f"{bound_bytes}-byte publication bound; the complete log is retained "
            "and is not published truncated",
        )
    return outcome
