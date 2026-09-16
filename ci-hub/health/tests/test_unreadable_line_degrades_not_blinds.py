#!/usr/bin/env python3
"""An unreadable marker-shaped line must be REPORTED, never fatal, never ignored.

⚠️ THIS BRACKET HAS TO FAIL IN BOTH DIRECTIONS OR IT IS NOT A CHECK. A parser
made permissive enough to never abort is a parser that has stopped checking, so
every case below is paired:

    a real objection must still BLOCK         (permissiveness would break this)
    an unreadable line must still be REPORTED (silent tolerance would break this)

WHAT WENT WRONG, measured 2026-09-16. `unresolved_review_objections` ran both
lanes, fully populated the objection list, and then raised if any malformed line
existed -- it computed the answer and discarded it. One such line made
`changes-requested-gate` return rc=2 INDETERMINATE at EVERY head of that pull
request. rc=2 reads as a tooling problem, so the pull request read as unattended:

    hermit#2694  hid 6 objections, 2 binding the exact head; sat 20 days
    hermit#1689  hid 1 outrun refusal, codex@5b6ce1fe8095
    hermit#2907  hid CLEAR -- there was nothing wrong with it

#2907 is the case that settles the design question. The failure is SYMMETRIC: it
conceals admission as readily as objection. A fail-closed argument only holds
when the failure is conservative in one direction, and this one is not.

THE TRIGGER IS TEXT WRAPPING. Two of the three were ordinary English sentences
in which a line wrap put a verdict word at column 0, mid-sentence. No author
controls where prose wraps, so there is no discipline that prevents it.

Needs no network and no build: every case is a synthetic comment body.
"""

from __future__ import annotations

import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

import approval_binding as grammar  # noqa: E402

HEAD = "a" * 40
OTHER = "b" * 40

# A canonical refusal binding the head under test.
def _record(record_id: int, body: str, minute: int) -> dict:
    """A synthetic issue comment carrying the immutable times the reader needs."""
    stamp = f"2026-09-16T00:{minute:02d}:00Z"
    return {
        "id": record_id,
        "html_url": f"https://example.invalid/#issuecomment-{record_id}",
        "body": body,
        "created_at": stamp,
        "updated_at": stamp,
        "_review_surface": "issue-comment",
    }


REFUSAL = _record(1, f"CHANGES-REQUESTED-AT: codex {HEAD}\n", 1)

# Ordinary prose that a line wrap has broken so a verdict word starts the line.
# This is the real shape from hermit#1689 and hermit#2907, not an invention.
WRAPPED_PROSE = _record(
    2,
    "The earlier review of this branch is\n"
    "CHANGES-REQUESTED and the rebase answered none of it.\n",
    2,
)


def _objections(records):
    return grammar.review_objections_and_unreadable(records, HEAD)


def test_a_real_objection_still_blocks_on_its_own() -> None:
    """The permissiveness control: nothing here may stop an objection binding."""
    unresolved, unreadable = _objections([REFUSAL])
    assert unreadable == [], unreadable
    assert len(unresolved) == 1, unresolved
    assert unresolved[0]["sha"].casefold() == HEAD


def test_an_unreadable_line_is_reported_rather_than_ignored() -> None:
    """The silent-tolerance control: the line must not simply be dropped."""
    _unresolved, unreadable = _objections([WRAPPED_PROSE])
    assert len(unreadable) == 1, unreadable
    where, text = unreadable[0]
    assert "issuecomment-2" in where
    assert "rebase answered none of it" in text


def test_an_unreadable_line_no_longer_discards_a_real_objection() -> None:
    """The regression this bracket exists for.

    Before the repair this raised, and the refusal at the exact head -- which
    parsed perfectly well -- was never returned to any caller.
    """
    unresolved, unreadable = _objections([REFUSAL, WRAPPED_PROSE])
    assert len(unreadable) == 1, unreadable
    assert len(unresolved) == 1, unresolved
    assert unresolved[0]["sha"].casefold() == HEAD


def test_a_clean_record_reports_neither() -> None:
    """A pull request with nothing wrong must still say so.

    hermit#2907 was this case and read as unreadable for weeks.
    """
    clean = _record(3, "Looks good to me, no objection.\n", 3)
    unresolved, unreadable = _objections([clean])
    assert unresolved == []
    assert unreadable == []


def test_the_raising_contract_is_preserved_for_untouched_callers() -> None:
    """`unresolved_review_objections` still raises, deliberately.

    Callers not yet moved across keep the behaviour they were written against;
    only the gate and the lander opt into the reporting form.
    """
    try:
        grammar.unresolved_review_objections([REFUSAL, WRAPPED_PROSE], HEAD)
    except grammar.ReviewRecordError as error:
        assert "rebase answered none of it" in str(error)
    else:  # pragma: no cover - the assertion below is the failure message
        raise AssertionError(
            "unresolved_review_objections must still raise on an unreadable line"
        )
    # And it must NOT raise when there is nothing unreadable.
    assert len(grammar.unresolved_review_objections([REFUSAL], HEAD)) == 1


def test_one_source_line_is_reported_once_not_once_per_lane() -> None:
    """`lane_shas` runs per lane; one defect must not count as two."""
    _unresolved, unreadable = _objections([WRAPPED_PROSE])
    assert len(unreadable) == 1, unreadable
    assert len(set(unreadable)) == len(unreadable)


def test_backticking_the_word_is_a_sufficient_repair() -> None:
    """The minimal repair used on all three pull requests: +2 bytes.

    `strip_decoration` only unwraps a code span enclosing the WHOLE line, so a
    mid-sentence span stays prose and stops asserting a verdict.
    """
    repaired = {
        **WRAPPED_PROSE,
        "body": (
            "The earlier review of this branch is\n"
            "`CHANGES-REQUESTED` and the rebase answered none of it.\n"
        ),
    }
    _unresolved, unreadable = _objections([repaired])
    assert unreadable == [], unreadable


def main() -> int:
    failures = 0
    for name, case in sorted(globals().items()):
        if not name.startswith("test_") or not callable(case):
            continue
        try:
            case()
        except AssertionError as error:
            failures += 1
            print(f"  FAIL  {name}: {error}")
        else:
            print(f"  ok    {name}")
    if failures:
        print(f"unreadable-line degradation: {failures} FAILED")
        return 1
    print("unreadable-line degradation: all cases pass")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
