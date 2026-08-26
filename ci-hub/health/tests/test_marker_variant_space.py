#!/usr/bin/env python3
"""Enumerate marker assertions and prose mentions across the line-shape axes.

WHY THIS EXISTS. Four variants of "a refusal the tooling could not see" were found
one at a time, each fix closing one shape while the next walked through a gap
beside it:

    1  paired emphasis around the whole line          `**...**`
    2  block-level markdown prefix                    `## ...`, `> ...`, `- ...`
    3  a label outliving the sha it was earned at     (not a line shape)
    4  a verdict written with surrounding prose       `## CODEX-LANE CHANGES-...`

Variant four cannot be recovered from line shape without also treating prose that
quotes the same marker as a verdict. The GitHub write boundary now takes the exact
canonical marker through ``--verdict`` and adds it as its own line. That path is
tested in ``test_gh_comment_attribution.py``. This matrix covers only shapes whose
meaning is present in the line itself.

THE AXES. A marker line decomposes as

    [LEAD] KEYWORD [SEP] [LANE] [SEP] SHA [TRAIL]

and only some of those axes are bounded:

    LEAD     bounded    nothing / whitespace / block markdown / paired emphasis.
    KEYWORD  bounded    canonical three, plus historical synonyms.
    SEP      bounded    colon present or absent, whitespace variants.
    LANE     bounded    present / absent / cased.
    SHA      bounded    bare / backticked / bolded / abbreviated.
    TRAIL    UNBOUNDED  nothing / prose / punctuation / emphasis.

The binding grammar and the malformed check use the same boundary. They differ
only after it: a complete marker binds, while an incomplete one is reported. SHA
length does not decide whether a line is an assertion.

WHAT THIS BRACKET ASSERTS, over the generated cross-product:

    every assertion is at least SEEN           -- it binds as a refusal, or it is
                                                  routed to `malformed`.
    every quoted mention is left alone         -- it neither clears nor flags.
    a canonical approval still BINDS           -- the fix must not cost the
                                                  ordinary case.
    decoration never GRANTS what was not said  -- prose naming a sha grants
                                                  nothing, in any wrapping.
"""

from __future__ import annotations

import importlib.util
import itertools
import pathlib
import sys

_HERE = pathlib.Path(__file__).resolve().parent
_TARGET = _HERE.parent / "approval_binding.py"

SHA = "b3ca4e155098ad6e6f32f507ed72855da0638010"


def _load():
    spec = importlib.util.spec_from_file_location("approval_binding", _TARGET)
    module = importlib.util.module_from_spec(spec)
    try:
        spec.loader.exec_module(module)
    except SystemExit:
        pass
    return module


# The unbounded axis, sampled at every KIND of lead rather than exhaustively.
# Leads that open a CommonMark BLOCK rather than decorating a line. These must
# NOT be honoured as verdicts -- see the note in main().
BLOCK_LEADS = {"indent"}
QUOTED_LEADS = {"quote", "quote+h2"}

LEADS = [
    ("none", ""),
    ("indent", "    "),
    ("h2", "## "),
    ("h1", "# "),
    ("quote", "> "),
    ("dash", "- "),
    ("star", "* "),
    ("quote+h2", "> ## "),
]
TRAILS = [
    ("none", ""),
    ("prose", " -- and it binds at the current head"),
    ("punct", "."),
]
SHA_FORMS = [
    ("bare", SHA),
    ("backticked", f"`{SHA}`"),
]


def _comment(body: str, when: str) -> dict:
    return {"body": body, "createdAt": when, "html_url": "https://example/c"}


def main() -> int:
    module = _load()
    failures: list[str] = []

    approval = _comment(f"APPROVED-AT: claude {SHA}", "2026-08-12T18:15:20Z")

    seen = 0
    for (ln, lead), (tn, trail), (sn, sha) in itertools.product(LEADS, TRAILS, SHA_FORMS):
        line = f"{lead}CHANGES-REQUESTED-AT: claude {sha}{trail}"
        malformed: list[tuple[str, str]] = []
        bound = module.lane_shas([approval, _comment(line, "2026-08-13T01:00:00Z")],
                                 "claude", malformed)
        refused = bound == []
        noticed = refused or bool(malformed)
        seen += 1
        # ⚠️ A FOUR-SPACE INDENT IS NOT DECORATION, IT IS A BLOCK TYPE. Every other
        # A four-space indent at the start of a body is a CommonMark INDENTED CODE
        # BLOCK, exactly like a fence, and prose_lines() excludes it deliberately
        # so an illustration cannot bind. A blockquote and a prose prefix are
        # quoted text is a mention; headings, list bullets and paired emphasis
        # are decoration.
        # Demanding that it be honoured contradicts that exclusion, and this file
        # asserted it for six cells that had never once executed. The renderer shows
        # it to a human as a code block; the gate agrees with the reader.
        # Corrected 2026-08-25 after task a_refusal_indented_by was investigated and
        # found to be a defect in THIS EXPECTATION, not in the grammar. The realistic
        # indented forms -- under a bullet, or after prose with no blank line -- do
        # NOT open a code block and are asserted separately below.
        if ln in QUOTED_LEADS:
            ok = bound == [SHA] and not malformed
            if not ok:
                failures.append(
                    f"prose mention changed review state: lead={ln} trail={tn} sha={sn}"
                )
                print(
                    f"  FAIL  prose mention changed review state: "
                    f"lead={ln} trail={tn} sha={sn}"
                )
        else:
            want_seen = ln not in BLOCK_LEADS
            if noticed != want_seen:
                how = "invisible to both grammar and net" if want_seen else (
                    "honoured although it is an indented CODE BLOCK")
                failures.append(f"refusal {how}: lead={ln} trail={tn} sha={sn}")
                print(f"  FAIL  refusal {how}: lead={ln} trail={tn} sha={sn}")
    print(f"  {'PASS' if not failures else 'FAIL'}  "
          f"{seen - len(failures)}/{seen} refusal variants behave as their lead requires")

    # ⚠️ AND THE REALISTIC INDENTED FORMS MUST STILL BE SEEN. An indent only opens a
    # code block when a blank line precedes it. A marker indented under a bullet, or
    # after a sentence, is an ordinary continuation and MUST bind or be netted --
    # otherwise the correction above would be an excuse rather than a fix.
    for label, body in [
        ("indented under a bullet", f"- my verdict:\n    CHANGES-REQUESTED-AT: claude {SHA}"),
        ("indented after prose", f"My verdict is below.\n    CHANGES-REQUESTED-AT: claude {SHA}"),
    ]:
        mal: list[tuple[str, str]] = []
        bound = module.lane_shas([approval, _comment(body, "2026-08-13T01:00:00Z")],
                                 "claude", mal)
        ok = bound == [] or bool(mal)
        print(f"  {'PASS' if ok else 'FAIL'}  {label} is still seen")
        if not ok:
            failures.append(f"{label} was dropped")

    # The ordinary case must still work. A net that eats approvals is not a fix.
    ok_approval = module.lane_shas([approval], "claude", []) == [SHA]
    print(f"  {'PASS' if ok_approval else 'FAIL'}  canonical approval still binds")
    if not ok_approval:
        failures.append("canonical approval no longer binds")

    # And decoration must never GRANT. These are the false-positive guards; if
    # unanchoring the net had made prose bind, this is where it would show.
    for label, body in [
        ("prose naming a sha", f"we thought about approving {SHA} today"),
        ("quoted convention", f"write it as `APPROVED-AT: claude {SHA}` please"),
    ]:
        granted = module.lane_shas([_comment(body, "2026-08-12T18:15:20Z")], "claude", []) != []
        print(f"  {'PASS' if not granted else 'FAIL'}  {label} grants nothing")
        if granted:
            failures.append(f"{label} granted a lane")

    print(f"\nvariants generated: {seen}    failures: {len(failures)}")
    if failures:
        print("RESULT: FAIL")
        return 1
    print("RESULT: PASS")
    return 0




# ⚠️ THIS BRACKET HAD NEVER RUN, AND ITS FIRST EXECUTION WAS RED. It defines its
# cells in main() and, until 2026-08-25, no pytest test -- so it collected ZERO and
# executed in no CI job while reading as coverage. Task three_marker_grammar_brackets.
#
# The first run failed 6 of 72, all lead=indent. Investigated as task
# a_refusal_indented_by and resolved as a defect in the EXPECTATION, not the
# grammar: a four-space indent opens a CommonMark code block, which prose_lines()
# excludes on purpose so an illustration cannot bind, and the renderer shows it to a
# human the same way. The six are now asserted as excluded, and the realistic
# indented forms -- under a bullet, after prose -- are asserted as still seen, so
# the correction cannot be used to wave away a real loss.
def test_marker_variant_space(capsys) -> None:
    """Execute all 48 variants; the bracket's own report is the failure message."""
    rc = main()
    assert rc == 0, "\n" + capsys.readouterr().out

if __name__ == "__main__":
    sys.exit(main())
