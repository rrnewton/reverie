#!/usr/bin/env python3
"""Markdown decoration must not hide a verdict, and quotation must not invent one.

WHY THIS BRACKET EXISTS. `undecorate` stripped paired emphasis but not leading
block-level markdown, so a verdict written as a Markdown heading fell through to
silence. Silence is not neutral here, and that is the whole point:

    a lost APPROVED-AT           reads as ABSENT     -- no authority granted, safe
    a lost CHANGES-REQUESTED-AT  reads as APPROVED   -- the lane's EARLIER approval
                                                       stays the newest binding

Same cause, opposite consequences, and only one of them is safe. Measured on
https://github.com/rrnewton/hermit/pull/2176: a Claude-lane refusal posted as
`## CHANGES-REQUESTED-AT: claude b3ca4e15...` did not bind, and the approval at
that same head stayed authoritative. Replaying that pull request's real comment
history with the reviewer's 87-seconds-later strict re-post removed, the old
parser reports the claude lane BOUND_CURRENT -- approving a head a reviewer had
explicitly refused. That is the mechanism behind
https://github.com/rrnewton/hermit/pull/2591 merging over a valid block.

WHY STRIPPING MORE CANNOT MANUFACTURE A FALSE POSITIVE, which is the only reason
it is safe: removing a prefix decides nothing. After stripping, the anchored
grammar still has to match the entire remaining line -- keyword, colon, lane, and
a 40-hex. No string becomes a verdict by having `##` removed unless it was
already a verdict wearing a hat. The `prose_mentioning_a_sha` case below is the
standing guard on that claim.

A blockquote is different from a heading or list bullet: it says the line is
quoted from somewhere else. Stripping ``>`` turns a mention into this comment's
assertion, so the cases below require quoted approvals and refusals to do nothing.
"""

from __future__ import annotations

import importlib.util
import pathlib
import sys

_HERE = pathlib.Path(__file__).resolve().parent
_TARGET = _HERE.parent / "approval_binding.py"


def _load():
    spec = importlib.util.spec_from_file_location("approval_binding", _TARGET)
    module = importlib.util.module_from_spec(spec)
    try:
        spec.loader.exec_module(module)
    except SystemExit:  # the module guards its own CLI entry point
        pass
    return module


SHA = "b3ca4e155098ad6e6f32f507ed72855da0638010"
OTHER = "e211b9d01e635aac45c4558f12cab57764f6be3c"


def _c(body: str, when: str) -> dict:
    record_id = 1 if when.startswith("2026-08-12") else 2
    return {
        "id": record_id,
        "body": body,
        "createdAt": when,
        "includesCreatedEdit": False,
        "html_url": f"https://example.test/c/{record_id}",
    }


APPROVAL = _c(f"APPROVED-AT: claude {SHA}", "2026-08-12T18:15:20Z")


def _later(body: str) -> dict:
    return _c(body, "2026-08-13T01:27:16Z")


# (name, comments, claude lane should still be granted)
CASES = [
    ("plain_approval_still_grants", [APPROVAL], True),
    ("plain_refusal_blocks", [APPROVAL, _later(f"CHANGES-REQUESTED-AT: claude {SHA}")], False),
    ("heading_refusal_blocks", [APPROVAL, _later(f"## CHANGES-REQUESTED-AT: claude {SHA}")], False),
    ("h1_refusal_blocks", [APPROVAL, _later(f"# CHANGES-REQUESTED-AT: claude {SHA}")], False),
    ("blockquote_refusal_is_a_mention", [APPROVAL, _later(f"> CHANGES-REQUESTED-AT: claude {SHA}")], True),
    ("list_dash_refusal_blocks", [APPROVAL, _later(f"- CHANGES-REQUESTED-AT: claude {SHA}")], False),
    ("list_star_refusal_blocks", [APPROVAL, _later(f"* CHANGES-REQUESTED-AT: claude {SHA}")], False),
    ("bold_refusal_still_blocks", [APPROVAL, _later(f"**CHANGES-REQUESTED-AT: claude {SHA}**")], False),
    ("quoted_heading_refusal_is_a_mention", [APPROVAL, _later(f"> ## CHANGES-REQUESTED-AT: claude {SHA}")], True),
    ("heading_approval_grants", [_c(f"## APPROVED-AT: claude {SHA}", "2026-08-12T18:15:20Z")], True),
    ("blockquote_approval_grants_nothing",
     [_c(f"> APPROVED-AT: claude {SHA}", "2026-08-12T18:15:20Z")], False),
    # The false-positive guard. Stripping markers must never turn prose into a verdict.
    ("prose_mentioning_a_sha_grants_nothing", [_c(f"we considered approving {SHA} today", "2026-08-12T18:15:20Z")], False),
    ("refusal_in_the_other_lane_leaves_claude_alone",
     [APPROVAL, _later(f"## CHANGES-REQUESTED-AT: codex {SHA}")], True),
    # A refusal naming a DIFFERENT head still clears: verdicts are chronological,
    # and the module treats a later refusal as revoking the lane until re-approved.
    ("heading_refusal_at_other_head_still_clears",
     [APPROVAL, _later(f"## CHANGES-REQUESTED-AT: claude {OTHER}")], False),
]


def main() -> int:
    module = _load()
    failures = []
    for name, comments, want_granted in CASES:
        granted = module.lane_shas(comments, "claude", []) != []
        ok = granted == want_granted
        print(f"  {'PASS' if ok else 'FAIL'}  {name}: granted={granted} want={want_granted}")
        if not ok:
            failures.append(name)

    # Mutation control: if the block-level stripping is removed, the heading
    # refusal must go back to failing open. A bracket that passes with and
    # without the behaviour it claims to test is not a bracket.
    plain = module.lane_shas([APPROVAL, _later(f"CHANGES-REQUESTED-AT: claude {SHA}")], "claude", [])
    heading = module.lane_shas([APPROVAL, _later(f"## CHANGES-REQUESTED-AT: claude {SHA}")], "claude", [])
    if plain != heading:
        failures.append("heading and plain refusal must be treated identically")
        print("  FAIL  heading and plain refusal disagree")
    else:
        print("  PASS  heading refusal treated identically to plain refusal")

    print(f"\nassertions: {len(CASES) + 1 - len(failures)} passed, {len(failures)} failed")
    if failures:
        print("RESULT: FAIL")
        return 1
    print("RESULT: PASS")
    return 0



# ⚠️ THIS WRAPPER IS WHY THE FILE RUNS AT ALL. `ci-hub/tests/run_python_suites.py`
# executes this directory under PYTEST, and pytest runs only what it COLLECTS. A
# `main()`-only bracket defines no test function, collects ZERO and executes in no
# CI job -- measured 2026-08-25 at parent main 55438330, when THREE brackets over
# the marker grammar were all inert at once. A file named test_*.py in a directory
# with a declared execution floor then reads as coverage and carries none, which is
# how three separate defects in that grammar survived with every gate green.
# Task: three_marker_grammar_brackets
def test_undecorate_block_assertions_hold(capsys) -> None:
    """Execute the whole bracket; its own printed report is the failure message."""
    rc = main()
    out = capsys.readouterr().out
    assert rc == 0, "\n" + out

if __name__ == "__main__":
    sys.exit(main())
