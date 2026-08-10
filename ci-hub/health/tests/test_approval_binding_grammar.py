#!/usr/bin/env python3
"""Bracket the APPROVED-AT binding GRAMMAR, including the rejection trap.

Four behaviours the convention has to have, and one it must NOT have:

  APPROVED-AT <sha>                         binds
  no binding line                           stays UNDETERMINED
  REQUEST CHANGES AT <sha>                  does NOT bind as a positive
  bound approval + head moved               SUPERSEDED, not green
  a codex approval                          does not satisfy the claude lane

THE REJECTION CASE IS THE SUBTLE ONE and it is why this file exists separately.
A rejection that names a SHA is otherwise trivially parsed as a verdict FOR that
SHA -- the binding defect inverted, with the detector then confidently reporting
a bound approval that is actually a rejection. Worse, a comment DOCUMENTING the
convention necessarily contains the literal APPROVED-AT token; twice on
2026-08-10 a check of mine matched prose about a defect as the defect. So
rejection wins over approval within a comment, and the last case here plants
exactly that mixed body.

The ambiguous-tag case is the real #1727 shape: a Codex-family reviewer tagged
`gpt-5.6-sol`, which says neither "codex" nor "claude". It must fail CLOSED
rather than be guessed -- which is the argument for standardising on the
lane-explicit spelling.
"""
import sys, importlib.util
spec = importlib.util.spec_from_file_location("ab", "ci-hub/health/approval_binding.py")
ab = importlib.util.module_from_spec(spec); spec.loader.exec_module(ab)
CUR="ef1392494c11911748dbfe0a42bf3635d1ccb3e0"; OLD="87f62930d3e24ab75b516d01cba106161e0cca2e"
def C(b): return [{"body": b}]
cases = []
def case(name, comments, lane, expect_state, expect_bound=None):
    shas = ab.lane_shas(comments, lane)
    st = ab.classify_approval(CUR, shas)
    ok = st == expect_state and (expect_bound is None or (shas[-1] if shas else None) == expect_bound)
    cases.append((ok, name, f"state={st} bound={shas[-1] if shas else None}"))

# 1. APPROVED-AT binds (dispatch's canonical no-lane form, lane from tag)
case("APPROVED-AT <sha> with claude tag BINDS",
     C(f"[adversarial-reviewer agent, CLAUDE] [x, y]\nAPPROVED-AT {CUR}\n"), "claude", ab.BOUND_CURRENT, CUR)
# 1b. lane-explicit form binds
case("APPROVED-AT: claude <sha> BINDS",
     C(f"[adversarial-reviewer agent, claude-opus-5] [x]\n\nAPPROVED-AT: claude {CUR}\n"), "claude", ab.BOUND_CURRENT, CUR)
# 2. no line stays UNDETERMINED
case("no binding line stays UNDETERMINED",
     C("[adversarial-reviewer agent, CLAUDE]\nLooks good to me, shipping.\n"), "claude", ab.UNDETERMINED)
# 3. REQUEST CHANGES naming a SHA does NOT bind as positive
case("REQUEST CHANGES AT <sha> does NOT bind",
     C(f"[adversarial-reviewer agent, CLAUDE] [x]\nREQUEST CHANGES AT {CUR}\n"), "claude", ab.UNDETERMINED)
# 3b. rejection wins even if the body also contains an APPROVED-AT token (prose trap)
# THE CASE THAT MAKES reject-wins LOAD-BEARING. The first version of this test
# put the quoted APPROVED-AT mid-line ("Reviewers must emit APPROVED-AT ..."),
# which the line-anchored approve regex never matches -- so disabling reject-wins
# turned NOTHING red and the guard was untested. Here the quoted line starts at
# column 0, exactly as it would in a rejection that quotes the convention or
# quotes a prior approval it is superseding. Now the guard is the only thing
# preventing a rejection from binding as a positive.
case("rejection QUOTING a line-anchored APPROVED-AT still does NOT bind",
     C(f"[adversarial-reviewer agent, CLAUDE]\nREQUEST CHANGES AT {CUR}\n\nThe convention is:\nAPPROVED-AT {CUR}\n"),
     "claude", ab.UNDETERMINED)
# 4. a push after a bound approval => SUPERSEDED, not green
case("bound approval + head moved => SUPERSEDED",
     C(f"[adversarial-reviewer agent, CLAUDE]\nAPPROVED-AT {OLD}\n"), "claude", ab.SUPERSEDED, OLD)
# 5. lane isolation: a codex approval must not satisfy the claude lane
case("codex-tagged approval does NOT satisfy the claude lane",
     C(f"[adversarial-reviewer agent, GPT-5 Codex] [codex-reviewer]\nAPPROVED-AT {CUR}\n"), "claude", ab.UNDETERMINED)
case("codex-tagged approval DOES satisfy the codex lane",
     C(f"[adversarial-reviewer agent, GPT-5 Codex] [codex-reviewer]\nAPPROVED-AT {CUR}\n"), "codex", ab.BOUND_CURRENT, CUR)
# 6. ambiguous tag (the real #1727 case) fails closed
case("ambiguous 'gpt-5.6-sol' tag with no explicit lane fails CLOSED",
     C(f"[adversarial-reviewer agent, gpt-5.6-sol]\nAPPROVED-AT {CUR}\n"), "codex", ab.UNDETERMINED)
p=sum(1 for ok,_,_ in cases if ok); f=len(cases)-p
for ok,n,d in cases: print(f"  {'PASS' if ok else 'FAIL'}  {n}   [{d}]")
print(f"assertions: {p} passed, {f} failed")
sys.exit(1 if f else 0)
