#!/usr/bin/env python3
"""The pre-merge gate must distinguish marker assertions from prose mentions.

⚠️ WHY THIS EXISTS AS ITS OWN BRACKET. `test_marker_variant_space.py` pins the
shared GRAMMAR against these 72 cells. It says nothing about
`ci-hub/bin/changes-requested-gate`, which carries its own detection so that it
still refuses when the authority cannot be imported. Two implementations of one
rule is a drift risk that agent(hermit-003) named precisely, and the answer is not
to hope: it is to run the SAME enumeration through both.

The old presence rule treated prose explaining a marker as a refusal whenever it
happened to carry a SHA. The invariant here is two-sided: a marker assertion must
block or be flagged, and quoted text must leave the prior approval alone. The
existing lane-heading forms remain assertions, including with trailing prose.
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
QUOTED_LEADS = {"quote", "quote+h2"}


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
        if ln in QUOTED_LEADS:
            if rc != gate.OK:
                silent.append(f"FALSE REFUSAL: prose mention lead={ln} trail={tn} sha={sn}")
        elif rc == gate.OK:
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

    # An old-head refusal remains live until it is answered. A push alone cannot
    # distinguish an author who fixed the objection from one who pushed past it;
    # hermit#2670 merged through exactly that ambiguity.
    stale = "1" * 40
    rc_stale, _ = gate.evaluate([approval, f"CHANGES-REQUESTED-AT: claude {stale}"], SHA)
    if rc_stale != gate.BLOCKED:
        silent.append("SILENTLY CLEARED: an unanswered old-head refusal did not block")
    # And prose discussing the convention must not block, or the gate gets disabled.
    rc_prose, _ = gate.evaluate(
        [approval, "A sweep seeing only APPROVED-AT cannot see a CHANGES-REQUESTED-AT."], SHA
    )
    if rc_prose != gate.OK:
        silent.append("FALSE REFUSAL: prose mentioning the token blocked the head")

    # Same marker vocabulary, opposite syntactic context. The full SHA in the
    # mention must not matter, and the absent SHA in the assertion must not hide it.
    mention = f"The required form is CHANGES-REQUESTED-AT: claude {SHA}, on its own line."
    if gate.evaluate([approval, mention], SHA)[0] != gate.OK:
        silent.append("FALSE REFUSAL: full-SHA prose mention blocked the head")
    if gate.evaluate([approval, "CHANGES-REQUESTED-AT: claude"], SHA)[0] == gate.OK:
        silent.append("SILENTLY CLEARED: standalone refusal with no SHA was ignored")
    lead_in = f"## CODEX-LANE CHANGES-REQUESTED-AT: codex `{SHA}` -- still blocked"
    if gate.evaluate([approval, lead_in], SHA)[0] == gate.OK:
        silent.append("SILENTLY CLEARED: CODEX-LANE refusal with trailing prose was ignored")
    # ⚠️ A WITHDRAWAL CLEARS ITS OWN LANE ONLY -- THE DUAL-LANE GUARANTEE.
    # Until 2026-08-25 `markers()` discarded the lane, so `withdrawn_at_head` was
    # `any(...)` over every marker and ONE lane's withdrawal discharged EVERY
    # lane's refusal. Reproduced live on hermit#2613, by accident, while its
    # claude reviewer withdrew correctly: the gate then reported CLEAR over an
    # unwithdrawn codex refusal. Two lanes mean nothing if the gate cannot tell
    # them apart, so each cell below fails against the pre-fix gate.
    # A disclosure tag, so the issuer is readable and an unquoted withdrawal
    # can be matched to the refusal it retracts.
    SIGNED = "[hermit2, hermit-006, unresolved, devbig014, role=reviewer]\n"
    refuse = lambda lane: f"CHANGES-REQUESTED-AT: {lane} {SHA}"
    drop = lambda lane: f"CHANGES-REQUESTED-WITHDRAWN-AT: {lane} {SHA}"
    for label, bodies, want_ok in [
        ("both lanes refuse, neither withdraws",
         [refuse("claude"), refuse("codex")], False),
        ("both lanes refuse, only claude withdraws",
         [refuse("claude"), refuse("codex"), drop("claude")], False),
        ("both lanes refuse, only codex withdraws",
         [refuse("claude"), refuse("codex"), drop("codex")], False),
        # ⚠️ THESE TWO EXPECTED A CLEAR UNTIL 2026-09-05 AND THE EXPECTATION WAS
        # STALE, NOT THE GATE. They use BARE markers with no disclosure tag, and
        # an UNQUOTED withdrawal now retires only the refusals of the issuer who
        # posted it -- so a refusal with no readable issuer is not matched by one,
        # which is deliberate: hermit#2625 has two same-lane refusals of which the
        # unsigned one must keep binding, and clearing it from an unsigned
        # withdrawal would be a guess about who spoke.
        #
        # A bare refusal is NOT unretirable. Measured on the real hermit#2625
        # pair: a withdrawal quoting its comment id clears it, reported as
        # "issued by <unsigned> and retired by hermit-012". The precise mechanism
        # is the route, and the case below pins it so this file asserts both
        # directions rather than only the refusal.
        ("both lanes refuse, both withdraw, all unsigned",
         [refuse("claude"), refuse("codex"), drop("claude"), drop("codex")], False),
        ("one lane refuses, the OTHER lane withdraws",
         [refuse("claude"), drop("codex")], False),
        ("one lane refuses, its OWN lane withdraws, both unsigned",
         [refuse("claude"), drop("claude")], False),
        ("signed refusal, its own issuer withdraws in the same lane",
         [SIGNED + refuse("claude"), SIGNED + drop("claude")], True),
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
