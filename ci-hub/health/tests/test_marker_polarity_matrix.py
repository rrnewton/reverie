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

Run the same inputs through both polarities and they collapse into one sentence:
THE PARSER DISTINGUISHES POLARITY BUT NOT ISSUANCE FROM CITATION. The polarity
asymmetry itself is deliberate and correct -- a malformed REFUSAL must still
refuse while a malformed APPROVAL must still fail to bind (approval_binding.py,
"a malformed REFUSAL must still refuse"). The defect is that the same asymmetry
is applied to text that was never a verdict at all.

The sharpest row is CITED_SHORT. The identical sentence --

    I posted `<TOKEN>: claude 6457ba0acfb4` earlier.

-- is SILENTLY IGNORED when <TOKEN> is APPROVED-AT and is a BINDING REFUSAL when
it is CHANGES-REQUESTED-AT. Nobody designed that; it is two independently
hardened code paths meeting.

MECHANISM, so a reader need not re-derive it:
  - the APPROVAL side under-reads. The binder is anchored, and the loudness net
    `suspect` demands a verdict keyword AND a FULL 40-hex on the same line, so a
    line carrying an ABBREVIATED sha is invisible to the binder AND to the net.
  - the REFUSAL side over-reads. It scans for the token anywhere in the line and
    accepts a lane within a short window, and its near-miss branch accepts a hit
    with NO lane and NO sha whenever the line is line-initial.

⚠️ DO NOT "FIX" A CELL HERE ALONE. Widening the approval net to accept short shas
also routes more lines to `malformed`, and a malformed REFUSAL-shaped line fails
the gate -- so the obvious approval-side repair can manufacture blocks, which is
the P0 above made worse. The durable fix needs ISSUANCE to be distinguishable
from CITATION, which is what the issuer field in task make_the_review_marker
provides; both nets should move together afterwards, against this table.

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
    # DEFECT an_inline_code_approved: a correctly written approval, merely
    # abbreviated, is lost with nobody told. Operationally the worst of the three.
    ("CANONICAL_SHORT", "APPROVED-AT"): "SILENT",
    ("CANONICAL_SHORT", "CHANGES-REQUESTED-AT"): "REFUSES(claude," + SHORT + ")",
    # DEFECT quoting_a_refusal_marker (both polarities): a whole-line quotation is
    # undecorated into a verdict, because a leading backtick is stripped as
    # decoration. Quoting an approval GRANTS one; quoting a refusal CREATES one.
    ("INLINE_CODE_WHOLE_LINE", "APPROVED-AT"): "BINDS",
    ("INLINE_CODE_WHOLE_LINE", "CHANGES-REQUESTED-AT"): "REFUSES(claude," + FULL + ")",
    ("CITED_FULL", "APPROVED-AT"): "MALFORMED",
    # DEFECT quoting_a_refusal_marker: citing a block manufactures a block.
    ("CITED_FULL", "CHANGES-REQUESTED-AT"): "REFUSES(claude," + FULL + ")",
    # DEFECT, the sharpest row: identical sentence, opposite treatment.
    ("CITED_SHORT", "APPROVED-AT"): "SILENT",
    ("CITED_SHORT", "CHANGES-REQUESTED-AT"): "REFUSES(claude," + SHORT + ")",
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
    ("CANONICAL_SHORT", "APPROVED-AT"): "an_inline_code_approved",
    ("INLINE_CODE_WHOLE_LINE", "APPROVED-AT"): "quoting_a_refusal_marker",
    ("INLINE_CODE_WHOLE_LINE", "CHANGES-REQUESTED-AT"): "quoting_a_refusal_marker",
    ("CITED_FULL", "CHANGES-REQUESTED-AT"): "quoting_a_refusal_marker",
    ("CITED_SHORT", "APPROVED-AT"): "an_inline_code_approved",
    ("CITED_SHORT", "CHANGES-REQUESTED-AT"): "quoting_a_refusal_marker",
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
