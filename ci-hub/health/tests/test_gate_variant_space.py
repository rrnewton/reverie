#!/usr/bin/env python3
"""The pre-merge gate must see every cell of the marker variant space.

⚠️ WHY THIS EXISTS AS ITS OWN BRACKET. `test_marker_variant_space.py` pins the
shared GRAMMAR against these 72 cells. It says nothing about
`ci-hub/bin/changes-requested-gate`, which carries its own detection so that it
still refuses when the authority cannot be imported. Two implementations of one
rule is a drift risk that agent(hermit-003) named precisely, and the answer is not
to hope: it is to run the SAME enumeration through both.

Measured 2026-08-25, and this is the whole reason the bracket exists:

    gate at 35505ca1 (line-initial rule)   caught 48/72, missed 24
    gate after the presence fix            caught 72/72, missed  0

The 24 were exactly the four word-bearing leads -- `CODEX-LANE `, `Codex lane: `,
`## CODEX-LANE `, `**Codex lane** ` -- crossed with 3 trails and 2 sha forms. The
gate RAN and REFUSED throughout; it was simply blind to a third of the space, and
its "clear" verdict was printed into the lander log as if verified. A gate that is
blind is worse than one that is absent, because its zero means two things and only
one of them is good news.

The invariant asserted is agent(hermit-003)'s, deliberately weaker than "parses
correctly": NO CELL MAY BE SILENTLY WRONG. A refusal must block or be flagged --
both are non-zero and both stop the merge. Only a silent OK is a failure here.
"""
from __future__ import annotations

import importlib.machinery
import importlib.util
import itertools
import pathlib
import sys

SHA = "b3ca4e155098ad6e6f32f507ed72855da0638010"

# Mirrors ci-hub/health/tests/test_marker_variant_space.py. Kept literal rather
# than imported: if that file's axes change, this bracket should FAIL and be
# updated deliberately, not silently follow.
LEADS = [
    ("none", ""), ("indent", "    "), ("h2", "## "), ("h1", "# "),
    ("quote", "> "), ("dash", "- "), ("star", "* "), ("quote+h2", "> ## "),
    ("word", "CODEX-LANE "), ("words+colon", "Codex lane: "),
    ("h2+word", "## CODEX-LANE "), ("emphasis+word", "**Codex lane** "),
]
TRAILS = [("none", ""), ("prose", " -- and it binds at the current head"), ("punct", ".")]
SHA_FORMS = [("bare", SHA), ("backticked", f"`{SHA}`")]


def _load_gate():
    # SourceFileLoader, not spec_from_file_location: the gate is an extensionless
    # executable and the usual loader refuses it.
    path = pathlib.Path(__file__).resolve().parents[2] / "bin" / "changes-requested-gate"
    loader = importlib.machinery.SourceFileLoader("changes_requested_gate", str(path))
    spec = importlib.util.spec_from_loader("changes_requested_gate", loader)
    module = importlib.util.module_from_spec(spec)
    loader.exec_module(module)
    return module


def main() -> int:
    gate = _load_gate()
    approval = f"APPROVED-AT: claude {SHA}"
    silent: list[str] = []
    seen = 0
    for (ln, lead), (tn, trail), (sn, sha) in itertools.product(LEADS, TRAILS, SHA_FORMS):
        # ⚠️ A CONTEXT LINE FIRST, BECAUSE A BARE INDENTED LINE IS A CODE BLOCK.
        # Without it the `indent` lead makes the marker the whole body, which
        # CommonMark -- and `approval_binding.prose_lines` -- read as an indented
        # code block, so the gate correctly declines to honour it. Asserting that
        # such a cell must REFUSE would pin the opposite of the deliberate rule
        # that the gate agrees with what the reader sees. Measured 2026-08-25:
        # this bracket began failing `indent/punct/backticked` the moment the
        # fence rule landed, and the fixture was wrong, not the rule.
        # A real marker appears inside a comment that says something, so model that.
        line = f"{lead}CHANGES-REQUESTED-AT: claude {sha}{trail}"
        body = f"My verdict on this head:\n{line}"
        rc, _why = gate.evaluate([approval, body], SHA)
        seen += 1
        if rc == gate.OK:
            silent.append(f"refusal SILENTLY CLEARED: lead={ln} trail={tn} sha={sn}")

    # ⚠️ AND PIN THE FENCE RULE ITSELF, in both directions. A marker that renders
    # as code must NOT refuse -- otherwise every worked example in a review blocks
    # its own head -- and a marker indented as a CONTINUATION must still refuse.
    fenced = f"```\nCHANGES-REQUESTED-AT: claude {SHA}\n```"
    if gate.evaluate([approval, fenced], SHA)[0] != gate.OK:
        silent.append("FALSE REFUSAL: a marker inside a fenced code block refused")
    blocked_indent = f"Example:\n\n    CHANGES-REQUESTED-AT: claude {SHA}"
    if gate.evaluate([approval, blocked_indent], SHA)[0] != gate.OK:
        silent.append("FALSE REFUSAL: a marker in an indented code block refused")
    continuation = f"- my verdict:\n    CHANGES-REQUESTED-AT: claude {SHA}"
    if gate.evaluate([approval, continuation], SHA)[0] == gate.OK:
        silent.append("SILENTLY CLEARED: an indented CONTINUATION marker was not honoured")

    # The other direction, so the bracket cannot pass by blocking everything:
    # a refusal naming a SUPERSEDED sha must NOT block -- that is the author
    # pushing a fix, and 5 of the 7 markers in the 184-PR census were exactly it.
    stale = "1" * 40
    rc_stale, _ = gate.evaluate([approval, f"CHANGES-REQUESTED-AT: claude {stale}"], SHA)
    if rc_stale != gate.OK:
        silent.append("FALSE REFUSAL: a superseded-sha refusal blocked the head")
    # And prose discussing the convention must not block, or the gate gets disabled.
    rc_prose, _ = gate.evaluate(
        [approval, "A sweep seeing only APPROVED-AT cannot see a CHANGES-REQUESTED-AT."], SHA
    )
    if rc_prose != gate.OK:
        silent.append("FALSE REFUSAL: prose mentioning the token blocked the head")

    # ⚠️ A WITHDRAWAL CLEARS ITS OWN LANE ONLY -- THE DUAL-LANE GUARANTEE.
    # Until 2026-08-25 `markers()` discarded the lane, so `withdrawn_at_head` was
    # `any(...)` over every marker and ONE lane's withdrawal discharged EVERY
    # lane's refusal. Reproduced live on hermit#2613, by accident, while its
    # claude reviewer withdrew correctly: the gate then reported CLEAR over an
    # unwithdrawn codex refusal. Two lanes mean nothing if the gate cannot tell
    # them apart, so each cell below fails against the pre-fix gate.
    refuse = lambda lane: f"CHANGES-REQUESTED-AT: {lane} {SHA}"
    drop = lambda lane: f"CHANGES-REQUESTED-WITHDRAWN-AT: {lane} {SHA}"
    for label, bodies, want_ok in [
        ("both lanes refuse, neither withdraws",
         [refuse("claude"), refuse("codex")], False),
        ("both lanes refuse, only claude withdraws",
         [refuse("claude"), refuse("codex"), drop("claude")], False),
        ("both lanes refuse, only codex withdraws",
         [refuse("claude"), refuse("codex"), drop("codex")], False),
        ("both lanes refuse, both withdraw",
         [refuse("claude"), refuse("codex"), drop("claude"), drop("codex")], True),
        ("one lane refuses, the OTHER lane withdraws",
         [refuse("claude"), drop("codex")], False),
        ("one lane refuses, its OWN lane withdraws",
         [refuse("claude"), drop("claude")], True),
        # Both laneless shapes fail closed, or the unilateral clear returns
        # through the bare spelling: a withdrawal with no lane cannot say whose
        # refusal it discharges, and a refusal with no lane has no owner who
        # could withdraw it.
        ("unlaned withdrawal clears nothing",
         [refuse("claude"), f"CHANGES-REQUESTED-WITHDRAWN-AT {SHA}"], False),
    ]:
        rc, _why = gate.evaluate(bodies, SHA)
        if want_ok and rc != gate.OK:
            silent.append(f"FALSE REFUSAL: {label}")
        if not want_ok and rc == gate.OK:
            silent.append(f"SILENTLY CLEARED: {label}")

    print(f"gate cells: {seen}    silent-or-false: {len(silent)}")
    for s in silent:
        print(f"  {s}")
    print("RESULT: " + ("PASS" if not silent else "FAIL"))
    return 1 if silent else 0



# ⚠️ THIS WRAPPER IS WHY THE FILE RUNS AT ALL. `ci-hub/tests/run_python_suites.py`
# executes this directory under PYTEST, and pytest runs only what it COLLECTS. A
# `main()`-only bracket defines no test function, collects ZERO and executes in no
# CI job -- measured 2026-08-25 at parent main 55438330, when THREE brackets over
# the marker grammar were all inert at once. A file named test_*.py in a directory
# with a declared execution floor then reads as coverage and carries none, which is
# how three separate defects in that grammar survived with every gate green.
# Task: three_marker_grammar_brackets
def test_gate_variant_space_has_no_silent_cell(capsys) -> None:
    """Execute the whole bracket; its own printed report is the failure message."""
    rc = main()
    out = capsys.readouterr().out
    assert rc == 0, "\n" + out

if __name__ == "__main__":
    raise SystemExit(main())
