#!/usr/bin/env python3
"""ENUMERATE the marker-line variant space instead of chasing variants one at a time.

WHY THIS EXISTS. Four variants of "a refusal the tooling could not see" were found
one at a time, each fix closing one shape while the next walked through a gap
beside it:

    1  paired emphasis around the whole line          `**...**`
    2  block-level markdown prefix                    `## ...`, `> ...`, `- ...`
    3  a label outliving the sha it was earned at     (not a line shape)
    4  an arbitrary WORD prefix                       `## CODEX-LANE CHANGES-...`

Variant four is the one that made enumeration necessary rather than optional: it
escaped the binding grammar AND the "loud on the unknown" net, because BOTH were
anchored with ``^``. The net had the same blind spot as the thing it was netting,
and its own docstring had promised "the next one must announce itself instead".

THE AXES. A marker line decomposes as

    [LEAD] KEYWORD [SEP] [LANE] [SEP] SHA [TRAIL]

and only some of those axes are bounded:

    LEAD     UNBOUNDED  nothing / whitespace / block markdown / paired emphasis /
                        ARBITRARY WORDS. Any words may precede a marker.
    KEYWORD  bounded    canonical three, plus historical synonyms.
    SEP      bounded    colon present or absent, whitespace variants.
    LANE     bounded    present / absent / cased.
    SHA      bounded    bare / backticked / bolded / abbreviated.
    TRAIL    UNBOUNDED  nothing / prose / punctuation / emphasis.

LEAD and TRAIL are unbounded, so ANY anchored matcher has infinitely many holes
and each new phrasing is a new variant. That is the actual defect behind all four,
and it is why the fix is a property rather than a case list:

    the BINDING grammar stays anchored -- granting authority should demand an
    exact form -- while the NET is an unanchored SEARCH, so no lead-in or
    trailing commentary can hide a verdict-shaped line from it.

WHAT THIS BRACKET ASSERTS, over the generated cross-product:

    every refusal variant is at least SEEN     -- it binds as a refusal, or it is
                                                  routed to `malformed`, which the
                                                  gate refuses. Never silence.
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
LEADS = [
    ("none", ""),
    ("indent", "    "),
    ("h2", "## "),
    ("h1", "# "),
    ("quote", "> "),
    ("dash", "- "),
    ("star", "* "),
    ("quote+h2", "> ## "),
    ("word", "CODEX-LANE "),
    ("words+colon", "Codex lane: "),
    ("h2+word", "## CODEX-LANE "),
    ("emphasis+word", "**Codex lane** "),
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
        if not noticed:
            failures.append(f"refusal INVISIBLE: lead={ln} trail={tn} sha={sn}")
            print(f"  FAIL  refusal invisible to both grammar and net: "
                  f"lead={ln} trail={tn} sha={sn}")
    print(f"  {'PASS' if not failures else 'FAIL'}  "
          f"{seen - len(failures)}/{seen} refusal variants are seen (bound or netted)")

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

    # Variant four exactly as found, kept as a named regression alongside the
    # generated matrix so the report names it if it ever returns.
    v4 = f"## CODEX-LANE CHANGES-REQUESTED-AT: codex `{SHA}`"
    mal: list[tuple[str, str]] = []
    codex_appr = _comment(f"APPROVED-AT: codex {SHA}", "2026-08-12T18:15:20Z")
    bound = module.lane_shas([codex_appr, _comment(v4, "2026-08-13T01:00:00Z")], "codex", mal)
    v4_seen = bound == [] or bool(mal)
    print(f"  {'PASS' if v4_seen else 'FAIL'}  variant four is seen")
    if not v4_seen:
        failures.append("variant four still invisible")

    total = 4
    print(f"\nvariants generated: {seen}    failures: {len(failures)}")
    if failures:
        print("RESULT: FAIL")
        return 1
    print("RESULT: PASS")
    return 0


if __name__ == "__main__":
    sys.exit(main())
