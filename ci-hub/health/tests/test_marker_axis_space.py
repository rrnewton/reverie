#!/usr/bin/env python3
"""Enumerate the marker AXIS SPACE, including comment composition.

WHY A SECOND MATRIX. `test_marker_variant_space.py` enumerates LINE SHAPE --
lead, trail, sha decoration -- and that closed six forms. It could not, by
construction, find the seventh: a comment carrying BOTH a refusal and a fresh
approval binds NOTHING, silently, because the rejection scan is per COMMENT and
`continue` skips the rest of it. Split across two comments the identical lines
bind correctly, which is why it stayed invisible.

The lesson is that "enumerate the variants" is not one matrix. A marker line has
line-level axes AND the comment has composition axes, and a matrix over one
family proves nothing about the other. Seven axes are enumerated here:

    LEAD         nothing / heading / quote / lane heading / nested
    TRAIL        nothing / trailing prose
    SHA FORM     bare / backticked
    SEPARATOR    colon present / absent
    LANE TOKEN   present / absent
    WHICH SHA    current head / a stale head
    COMPOSITION  single marker / two in ONE comment / two across comments

THE INVARIANT, which is weaker than "parses correctly" on purpose: no assertion
may be silently wrong, and no quoted mention may change review state. A refusal
must clear the lane or be flagged; a retract-then-rebind must bind at the head or
be flagged. The existing lane-heading form remains an assertion with or without
trailing prose.

Measured when this was written: 472/480 safe, and all 8 failures were the single
composition class above.
"""

import importlib.util
import itertools
SHA="b3ca4e155098ad6e6f32f507ed72855da0638010"
OLD="94d9a86545d7"+"0"*28
def load(p):
    s=importlib.util.spec_from_file_location("m",p); m=importlib.util.module_from_spec(s)
    try: s.loader.exec_module(m)
    except SystemExit: pass
    return m
def c(b,t):
    record_id = {"T1": 1, "T2": 2, "T3": 3}[t]
    return {
        "id": record_id,
        "body": b,
        "createdAt": f"2026-09-04T00:00:0{record_id}Z",
        "includesCreatedEdit": False,
        "html_url": f"https://example.test/c/{record_id}",
    }

# ---- THE AXES -------------------------------------------------------------
LEAD   = {"none":"", "h2":"## ", "quote":"> ", "word":"CODEX-LANE ", "q+h2":"> ## "}
TRAIL  = {"none":"", "prose":" -- see below"}
SHAF   = {"bare":lambda s:s, "tick":lambda s:f"`{s}`"}
SEP    = {"colon":":", "nocolon":""}          # axis 4: missing colon
LANE   = {"lane":" claude", "nolane":""}      # axis 5: missing lane token
WHICH  = {"head":SHA, "stale":OLD}            # axis 6: stale head
COMPOSE= {"single":1, "one-comment":2, "two-comments":3}   # axis 7

def line(kw, lead, trail, shaf, sep, lane, sha):
    return f"{LEAD[lead]}{kw}{SEP[sep]}{LANE[lane]} {SHAF[shaf](WHICH[sha])}{TRAIL[trail]}".replace("  "," ")

def outcome(m, comments, lane_name="claude"):
    mal=[]; got=m.lane_shas(comments, lane_name, mal)
    return (got[-1] if got else None), len(mal)>0

def main(path):
    m=load(path); rows=[]; silent_wrong=[]
    for lead,trail,shaf,sep,lane,sha,comp in itertools.product(
            LEAD,TRAIL,SHAF,SEP,LANE,WHICH,COMPOSE):
        ref = line("CHANGES-REQUESTED-AT", lead,trail,shaf,sep,lane,sha)
        # A malformed marker under CODEX-LANE belongs to the codex row, not
        # the claude row. A canonical marker's explicit lane remains decisive.
        row_lane = "codex" if lead == "word" and m.parse_marker(ref) is None else "claude"
        app = f"APPROVED-AT: {row_lane} {SHA}"
        if COMPOSE[comp]==1:      cs=[c(f"APPROVED-AT: {row_lane} {SHA}","T1"), c(ref,"T2")]
        elif COMPOSE[comp]==2:    cs=[c(f"APPROVED-AT: {row_lane} {SHA}","T1"), c(ref+"\n"+app,"T2")]
        else:                     cs=[c(f"APPROVED-AT: {row_lane} {SHA}","T1"), c(ref,"T2"), c(app,"T3")]
        bound, flagged = outcome(m, cs, row_lane)
        mention = lead in {"quote", "q+h2"}
        if mention:
            ok = bound == SHA and not flagged
            intent="prose mention must not change review state"
        elif COMPOSE[comp]==1:
            # a refusal is present. Safe outcomes: cleared, or flagged.
            ok = (bound is None) or flagged
            intent="refusal must clear or be flagged"
        else:
            # refusal THEN approval at head. Safe: bound at head, or flagged.
            ok = (bound==SHA) or flagged
            intent="retract-then-rebind must bind at head or be flagged"
        rows.append(ok)
        if not ok:
            silent_wrong.append((comp,lead,sep,lane,sha,shaf,trail,intent,bound))
    return rows, silent_wrong

import pathlib, sys
TARGET = pathlib.Path(__file__).resolve().parent.parent / "approval_binding.py"


def report(bad, rows):
    """Human-readable breakdown, shared by the test and the script entry point."""
    from collections import Counter
    lines = [f"{len(rows)-len(bad)}/{len(rows)} cells safe, {len(bad)} SILENTLY WRONG"]
    if bad:
        lines.append(f"  by composition: {dict(Counter(b[0] for b in bad))}")
        lines.append(f"  by separator  : {dict(Counter(b[2] for b in bad))}")
        lines.append(f"  by lane token : {dict(Counter(b[3] for b in bad))}")
        for b in bad[:6]:
            lines.append(f"   SILENTLY WRONG: {b[:7]} bound= {(b[8] or 'NOTHING')[:12]}")
    return "\n".join(lines)


# ⚠️ THIS MATRIX USED TO RUN AT IMPORT AND CALL sys.exit, WHICH COST THE WHOLE
# SUITE. There was no `__main__` guard, so `pytest` importing this file during
# COLLECTION raised SystemExit, pytest reported INTERNALERROR, and the run aborted
# for the ENTIRE directory: `ci-hub/health/tests` reported discovered=218
# executed=0 -- 218 assertions that read as coverage and had never run, including
# the APPROVED-AT grammar brackets that decide what may bind a review lane.
#
# ⚠️ AND GUARDING IT ALONE WOULD HAVE BEEN THE WRONG FIX. A guard stops the abort
# but leaves this file contributing ZERO tests to the suite it is named for, so
# its 480 cells would still never run under the runner -- trading a loud abort for
# a silent gap, which is the same defect wearing the opposite face. So the matrix
# is ALSO exposed as a real test below. The script entry point is preserved
# because the per-axis breakdown is genuinely useful when it does fail.
def test_marker_axis_space():
    rows, bad = main(str(TARGET))
    assert rows, "the axis matrix generated no cells; the enumeration broke"
    assert not bad, "approval_binding marker axis space:\n" + report(bad, rows)


if __name__ == "__main__":
    rows, bad = main(str(TARGET))
    print("approval_binding: " + report(bad, rows))
    print("RESULT: " + ("PASS" if not bad else "FAIL"))
    sys.exit(1 if bad else 0)
