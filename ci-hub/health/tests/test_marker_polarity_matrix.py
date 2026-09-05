#!/usr/bin/env python3
"""The marker parser reads the SAME line differently depending on its polarity.

⚠️ THIS BRACKET PINS BEHAVIOUR THAT IS PARTLY WRONG, ON PURPOSE. Several cells
below are DEFECTS with open tasks against them. They are asserted as they behave
TODAY so that fixing one side of the parser cannot quietly change the other:
whoever repairs a cell must come here, see the whole table, and update it
deliberately. A green run means "the asymmetry is exactly where we left it", not
"the parser is right".

WHY IT IS ONE TABLE AND NOT THREE BRACKETS. Three tasks were open on this parser
on 2026-08-25 and each looked like its own bug:

    an_inline_code_approved     an ISSUED approval with an ABBREVIATED sha binds
                                nothing and reports nothing -- silent
    quoting_a_refusal_marker    a CITED refusal is honoured as if issued, so
                                EXPLAINING a block to another agent creates one
    changes_requested_gate_gh   ordinary English prose opening a comment is read
                                as an UNCLEARABLE refusal (P0, rc=2 forever)

Run the same inputs through both polarities. The polarity asymmetry itself is
deliberate and correct -- a malformed REFUSAL must still refuse while a malformed
APPROVAL must still fail to bind. The assertion boundary is shared: a marker at
the start of its normalized line is a verdict; marker text inside prose is a
mention in either polarity. Existing lane headings and the existing ``VERDICT:``
prefix make a lead-in explicit without relying on the marker's value or its tail.

The sharpest rows are CITED_FULL and CITED_SHORT. The identical sentence --

    I posted `<TOKEN>: claude 6457ba0acfb4` earlier.

-- must be a mention in both polarities. The full and abbreviated SHA rows prove
that a value does not turn prose into a verdict. The standalone short marker rows
prove the converse: an assertion is reported or refused even though its SHA is
not full length.

Needs no network and no build: every cell is a synthetic comment body.
"""

from __future__ import annotations

import importlib.machinery
import importlib.util
import pathlib
import sys

HERE = pathlib.Path(__file__).resolve()
HEALTH = HERE.parent.parent
BIN = HEALTH.parent / "bin"

FULL = "6457ba0acfb4eda22cf5fd617412c6b734b92801"
SHORT = "6457ba0acfb4"
LANE = "claude"


def _load(name: str, path: pathlib.Path):
    loader = importlib.machinery.SourceFileLoader(name, str(path))
    spec = importlib.util.spec_from_loader(name, loader)
    module = importlib.util.module_from_spec(spec)
    loader.exec_module(module)
    return module


ab = _load("approval_binding_matrix", HEALTH / "approval_binding.py")
gate = _load("changes_requested_gate_matrix", BIN / "changes-requested-gate")


# Each form is a template; `{T}` is the polarity token. The two polarities are
# therefore GUARANTEED to differ in nothing but the token, which is the whole
# point of the comparison.
FORMS = [
    ("CANONICAL_FULL", "{T}: claude " + FULL),
    ("CANONICAL_SHORT", "{T}: claude " + SHORT),
    ("INLINE_CODE_WHOLE_LINE", "`{T}: claude " + FULL + "`"),
    ("CITED_FULL", "I posted `{T}: claude " + FULL + "` earlier."),
    ("CITED_SHORT", "I posted `{T}: claude " + SHORT + "` earlier."),
    ("FENCED", "```\n{T}: claude " + FULL + "\n```"),
]

# Prose carries no token at all, so it cannot share a template with the others.
PROSE = {
    "APPROVED-AT": "Approved this earlier tonight, it looks right.",
    "CHANGES-REQUESTED-AT": "Changes requested, on the value pin alone.",
}

# (form, polarity) -> outcome observed on origin/main 2026-08-25.
# A cell tagged DEFECT is wrong and has an owning task; it is pinned, not endorsed.
EXPECTED = {
    ("CANONICAL_FULL", "APPROVED-AT"): "BINDS",
    ("CANONICAL_FULL", "CHANGES-REQUESTED-AT"): "REFUSES(claude," + FULL + ")",
    # FIXED: the loudness net once demanded a FULL 40-hex, so a correctly written
    # approval that merely ABBREVIATED its sha bound nothing and reported nothing --
    # a real review lost with nobody told. An asserted marker is now loud regardless
    # of value length. It still does not BIND, which is right: granting authority
    # demands the exact form. Task an_inline_code_approved.
    ("CANONICAL_SHORT", "APPROVED-AT"): "MALFORMED",
    ("CANONICAL_SHORT", "CHANGES-REQUESTED-AT"): "REFUSES(claude," + SHORT + ")",
    # A whole-line quotation is undecorated into a verdict, because a leading
    # backtick is stripped as decoration. In a SINGLE comment that is correct --
    # nothing earlier to cite, so it is an issuance. In a STREAM the issuer rule now
    # suppresses the author's own re-quotation; see the issuance/citation cells
    # below. RESIDUAL, and it needs the issuer FIELD rather than this rule: a
    # DIFFERENT agent quoting someone else's verdict still binds, because a repeat
    # by another author is indistinguishable from an independent second verdict
    # without a `BY <agent-id>` on the marker. Task make_the_review_marker.
    ("INLINE_CODE_WHOLE_LINE", "APPROVED-AT"): "BINDS",
    ("INLINE_CODE_WHOLE_LINE", "CHANGES-REQUESTED-AT"): "REFUSES(claude," + FULL + ")",
    ("CITED_FULL", "APPROVED-AT"): "SILENT",
    ("CITED_FULL", "CHANGES-REQUESTED-AT"): "NOTHING",
    # The abbreviated form follows the same assertion/mention boundary as the
    # full SHA; length changes neither classification.
    ("CITED_SHORT", "APPROVED-AT"): "SILENT",
    ("CITED_SHORT", "CHANGES-REQUESTED-AT"): "NOTHING",
    # Correct in both polarities: a fenced illustration is nothing.
    ("FENCED", "APPROVED-AT"): "SILENT",
    ("FENCED", "CHANGES-REQUESTED-AT"): "NOTHING",
    ("PROSE", "APPROVED-AT"): "SILENT",
    # FIXED 2026-08-25 at parent main 84bdd9e3 by agent(hermit-013), "Prose is not
    # a verdict" (task changes_requested_gate_gh). It previously returned
    # REFUSES(None,None): invented from prose and UNCLEARABLE, since no sha means
    # no head supersedes it and no lane means no withdrawal reaches it. This cell
    # is now correct and is pinned so it cannot silently come back.
    ("PROSE", "CHANGES-REQUESTED-AT"): "NOTHING",
}

DEFECT_CELLS = {

    ("INLINE_CODE_WHOLE_LINE", "APPROVED-AT"): "quoting_a_refusal_marker",
    ("INLINE_CODE_WHOLE_LINE", "CHANGES-REQUESTED-AT"): "quoting_a_refusal_marker",
    # A marker inside prose is a mention in both polarities. Whole-line inline
    # code remains separately pinned above because it is existing decoration.
    # ("PROSE", "CHANGES-REQUESTED-AT") was here and is FIXED -- see EXPECTED.
}

# ⚠️ THE FIRST THING THIS TABLE CAUGHT WAS A FIX, NOT A REGRESSION, AND THAT IS
# THE POINT. Written against parent main 55438330 and rebased onto 84bdd9e3
# minutes later, the PROSE/CHANGES-REQUESTED-AT cell flipped from
# REFUSES(None,None) to NOTHING because agent(hermit-013) landed the P0 repair in
# between. Fifteen of the sixteen cells were untouched, which is the measurement
# that actually mattered: the refusal-side fix did NOT disturb the approval side,
# so the collision predicted below did not occur for that change. A one-sided fix
# is not forbidden here -- it is required to announce itself.


def approval_outcome(body: str) -> str:
    """BINDS / MALFORMED / SILENT, via the authority that grants review lanes."""
    malformed: list[tuple[str, str]] = []
    shas = ab.lane_shas([{"body": body, "html_url": "u"}], LANE, malformed, 1, {})
    if shas:
        return "BINDS"
    return "MALFORMED" if malformed else "SILENT"


def refusal_outcome(body: str) -> str:
    """REFUSES(lane,sha) / NOTHING, via the gate that actually blocks merges."""
    found = gate.markers([body])
    if not found:
        return "NOTHING"
    _kind, lane, sha = found[0]
    return f"REFUSES({lane},{sha})"


def _cells() -> list[tuple[str, str, str, str]]:
    """(form, polarity, body, expected) for every cell of the matrix."""
    rows = [(name, tpl) for name, tpl in FORMS] + [("PROSE", None)]
    out = []
    for form, tpl in rows:
        for polarity in ("APPROVED-AT", "CHANGES-REQUESTED-AT"):
            body = PROSE[polarity] if tpl is None else tpl.format(T=polarity)
            out.append((form, polarity, body, EXPECTED[(form, polarity)]))
    return out


def observe(polarity: str, body: str) -> str:
    return approval_outcome(body) if polarity == "APPROVED-AT" else refusal_outcome(body)


# ⚠️ ONE MODULE-LEVEL `test_*` FUNCTION PER CELL, AND THAT IS NOT A STYLE CHOICE.
# `ci-hub/tests/run_python_suites.py` runs this directory under PYTEST, and pytest
# executes nothing but collected tests. Measured 2026-08-25 in this same tree:
# `test_gate_variant_space.py`, `test_marker_variant_space.py` and
# `test_undecorate_block.py` define NO pytest tests, collect ZERO, and therefore
# run in no CI job at all -- three brackets over the marker grammar, all of them
# inert, which is how a parser wrong in three directions stayed green. A
# `main()`-only bracket is a file that reads as coverage and is not. These are
# generated rather than imported from pytest so the module needs no pytest at all
# to be collectable BY pytest, and `main()` below still works by hand.
def _install_cell_tests() -> None:
    for form, polarity, body, want in _cells():
        slug = f"{form}__{polarity.replace('-', '_').lower()}"

        def check(body: str = body, polarity: str = polarity, want: str = want,
                  form: str = form) -> None:
            actual = observe(polarity, body)
            owner = DEFECT_CELLS.get((form, polarity))
            hint = (
                f" This cell is a KNOWN DEFECT owned by task {owner}. If you are "
                "fixing it, update EXPECTED here and check the OTHER polarity in "
                "the same change -- the two nets are one grammar."
                if owner
                else " This cell was CORRECT; a change elsewhere has regressed it."
            )
            assert actual == want, (
                f"{form}/{polarity}: expected {want!r}, got {actual!r}.{hint}"
            )

        check.__name__ = f"test_cell_{slug}"
        check.__doc__ = f"{form} / {polarity} is pinned at {want!r}."
        globals()[check.__name__] = check


_install_cell_tests()


def test_control_approval_binds() -> None:
    """A canonical approval MUST bind, or every SILENT cell above is void."""
    assert approval_outcome(f"APPROVED-AT: claude {FULL}") == "BINDS", (
        "the harness is not reaching the parser, so every other cell is meaningless"
    )


def test_control_refusal_refuses() -> None:
    """A canonical refusal MUST refuse, or every NOTHING cell above is void."""
    assert refusal_outcome(f"CHANGES-REQUESTED-AT: claude {FULL}").startswith(
        "REFUSES("
    ), "the gate is not reaching its own markers(), so every cell is meaningless"



# ---------------------------------------------------------------------------
# ISSUANCE vs MENTION. Marker syntax, not a transient author name, decides whether
# text is authoritative. Inline prose, blockquotes, and code blocks are mentions;
# a whole-line canonical marker remains a verdict even when repeated.
#
# Measured on hermit#2615: the gate counted FIVE refusal markers where TWO
# verdicts existed. All three phantoms came from good practice -- sentences
# naming which marker still stood. A reviewer writing "my earlier refusal is
# answered by the fix below" and then approving BLOCKED THE HEAD THEY APPROVED.
# Task: quoting_a_refusal_marker
# Bodies retain disclosure tags as metadata so changing or omitting them can be
# checked without changing the marker result.
_TAG = "[hermit2, hermit-004, unresolved, devbig014, role=reviewer]"


def _by(agent: str, text: str) -> str:
    return f"[hermit2, {agent}, unresolved, devbig014, role=reviewer]\n\n{text}"


_ISS = f"CHANGES-REQUESTED-AT: claude {FULL}"


def _refusals(bodies, agent="hermit-004"):
    return gate.markers([_by(agent, b) for b in bodies])


def test_a_lone_mention_is_not_an_issuance() -> None:
    """Prose around the marker makes it a mention even without an earlier copy."""
    assert _refusals([f"see `{_ISS}` above"]) == []


def test_citing_a_refusal_does_not_create_a_second_one() -> None:
    assert len(_refusals([_ISS, f"my `{_ISS}` above still stands"])) == 1


def test_an_abbreviated_citation_matches_its_issuance() -> None:
    """Citations abbreviate; 06c3f3735fbf naming 06c3f3735fbf... is one verdict."""
    assert len(_refusals([_ISS, f"`CHANGES-REQUESTED-AT: claude {FULL[:12]}` is superseded"])) == 1


def test_the_issuance_itself_is_the_one_that_survives() -> None:
    got = _refusals([_ISS, f"quoting `{_ISS}`"])
    assert got and got[0][2] == FULL, got


def test_markdown_context_distinguishes_mentions_from_verdicts() -> None:
    for form in (f"> {_ISS}", f"    {_ISS}", f"I posted {_ISS} earlier."):
        assert len(_refusals([_ISS, form])) == 1, form
    for form in (f"`{_ISS}`", f"## {_ISS}"):
        assert len(_refusals([_ISS, form])) == 2, form


def test_a_different_lane_is_not_a_citation() -> None:
    """Two lanes refusing the same head are two verdicts, not a repeat."""
    assert len(_refusals([_ISS, f"CHANGES-REQUESTED-AT: codex {FULL}"])) == 2


def test_a_different_head_is_not_a_citation() -> None:
    other = "9" * 40
    assert len(_refusals([_ISS, f"CHANGES-REQUESTED-AT: claude {other}"])) == 2


def test_a_marker_with_no_sha_is_never_suppressed_by_identity() -> None:
    body = "CHANGES-REQUESTED-AT: claude"
    got = _refusals([body, body])
    assert len(got) == 2
    assert all(sha is None for _kind, _lane, sha in got)


def test_missing_or_different_author_metadata_does_not_change_issuance() -> None:
    named = gate.markers([_by("hermit-004", _ISS), _by("false-name", _ISS)])
    unnamed = gate.markers([_ISS, _ISS])
    assert named == unnamed
    assert len(named) == 2


def test_two_reviewers_refusing_one_head_are_two_verdicts() -> None:
    """⚠️ THE REGRESSION THAT CAUGHT AN AUTHOR-FREE VERSION OF THIS RULE.
    hermit-012 and hermit-015 both refused hermit#2625 at one head in one lane.
    Collapsing them let ONE withdrawal clear BOTH -- the dangerous direction."""
    got = gate.markers([_by("hermit-012", _ISS), _by("hermit-015", _ISS)])
    assert len(got) == 2, got


def test_inline_prose_mention_is_not_a_second_verdict() -> None:
    got = gate.markers([_by("hermit-004", _ISS), _by("hermit-004", f"`{_ISS}` still stands")])
    assert len(got) == 1, got


def test_hermit_2615_five_markers_were_two_verdicts() -> None:
    """The measured case, as a named regression rather than only a count."""
    codex = f"CHANGES-REQUESTED-AT: codex {FULL}"
    stream = [
        _ISS,                                    # hermit-004, genuine claude
        f"my `{_ISS}` above still stands",       # citation
        codex,                                   # hermit-001, genuine codex
        f"`{_ISS}` is superseded",               # citation
        f"see `CHANGES-REQUESTED-AT: claude {FULL[:8]}`",  # citation, abbreviated
    ]
    assert len(_refusals(stream)) == 2, _refusals(stream)


# ---------------------------------------------------------------------------
# THE APPROVAL SIDE: NEVER SILENT. Three outcomes are allowed and they must be
# DISTINGUISHABLE -- it BINDS, it is reported MALFORMED, or it counts as nothing
# and says so. Silence was a fourth, and it lost real reviews.
# Task: an_inline_code_approved
def _approval(bodies, agent="hermit-007"):
    """(bound shas, malformed count, citation count) for a comment stream."""
    mal: list = []
    cit: list = []
    cs = [{"body": _by(agent, b) if isinstance(b, str) else _by(b[0], b[1]),
           "html_url": "u"} for b in bodies]
    shas = ab.lane_shas(cs, "claude", mal, 1, {}, cit)
    return shas, len(mal), len(cit)


_APPR = f"APPROVED-AT: claude {FULL}"


def test_an_abbreviated_approval_is_loud_not_silent() -> None:
    """⚠️ THE DEFECT. Canonical form, own line, correct lane, merely abbreviated:
    it bound nothing and reported nothing, so the reviewer believed their lane was
    bound, the lane read unreviewed, and nothing said the review was dropped."""
    shas, mal, _ = _approval([f"APPROVED-AT: claude {FULL[:12]}"])
    assert shas == [] and mal == 1, (shas, mal)


def test_an_abbreviated_approval_still_does_not_bind() -> None:
    """Being loud is not the same as being accepted. Granting authority keeps
    demanding the exact form; only the SILENCE was the defect."""
    shas, _, _ = _approval([f"APPROVED-AT: claude {FULL[:12]}"])
    assert shas == []


def test_a_second_whole_line_approval_is_a_second_assertion() -> None:
    shas, _, cit = _approval([_APPR, f"`{_APPR}`"])
    assert shas == [FULL, FULL] and cit == 0, (shas, cit)


def test_a_suppressed_citation_is_counted_rather_than_dropped() -> None:
    """The third outcome has to be VISIBLE, or it is just a quieter silence."""
    _, _, cit = _approval([_APPR, f"as I said, `{_APPR}`"])
    assert cit == 1


def test_a_lone_quoted_approval_is_an_issuance() -> None:
    """Nothing earlier to cite. Same safety property as the refusal side."""
    shas, _, _ = _approval([f"`{_APPR}`"])
    assert shas == [FULL]


def test_a_citation_is_not_reported_as_malformed() -> None:
    """A citation is not a failed ATTEMPT at a verdict. Without this, widening the
    net to abbreviated shas would report every sentence quoting a marker -- and a
    malformed REFUSAL-shaped line BLOCKS, so it would manufacture blocks out of
    people explaining a refusal."""
    _, mal, cit = _approval([_APPR, f"my `APPROVED-AT: claude {FULL[:12]}` above"])
    assert mal == 0 and cit == 1, (mal, cit)

def main() -> int:
    failures: list[str] = []
    checked = 0

    for form, polarity, body, want in _cells():
        actual = observe(polarity, body)
        checked += 1
        if actual != want:
            owner = DEFECT_CELLS.get((form, polarity))
            note = (
                f" -- this cell is a KNOWN DEFECT owned by task {owner}; if you "
                "are fixing it, update this table and check the OTHER polarity "
                "in the same change"
                if owner
                else " -- this cell was CORRECT; a fix elsewhere has regressed it"
            )
            failures.append(
                f"{form}/{polarity}: expected {want!r}, got {actual!r}{note}"
            )

    # ⚠️ CONTROL THAT MUST BIND. Without it every "SILENT" above could be an
    # import that half-failed, and the bracket would pass by measuring nothing --
    # which is the exact failure mode the cells below describe.
    control = approval_outcome(f"APPROVED-AT: claude {FULL}")
    if control != "BINDS":
        failures.append(
            f"CONTROL FAILED: a canonical approval returned {control!r}, not 'BINDS'. "
            "The harness is not reaching the parser, so every other cell is void."
        )
    control_r = refusal_outcome(f"CHANGES-REQUESTED-AT: claude {FULL}")
    if not control_r.startswith("REFUSES("):
        failures.append(
            f"CONTROL FAILED: a canonical refusal returned {control_r!r}. "
            "The gate is not reaching its own markers(), so every cell is void."
        )

    print(f"polarity cells: {checked}    controls: 2    mismatches: {len(failures)}")
    print(f"known-defect cells pinned: {len(DEFECT_CELLS)}")
    for f in failures:
        print(f"  {f}")
    print("RESULT: " + ("PASS" if not failures else "FAIL"))
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
