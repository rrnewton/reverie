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
        line = f"{lead}CHANGES-REQUESTED-AT: claude {sha}{trail}"
        rc, _why = gate.evaluate([approval, line], SHA)
        seen += 1
        if rc == gate.OK:
            silent.append(f"refusal SILENTLY CLEARED: lead={ln} trail={tn} sha={sn}")

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

    print(f"gate cells: {seen}    silent-or-false: {len(silent)}")
    for s in silent:
        print(f"  {s}")
    print("RESULT: " + ("PASS" if not silent else "FAIL"))
    return 1 if silent else 0


if __name__ == "__main__":
    raise SystemExit(main())
