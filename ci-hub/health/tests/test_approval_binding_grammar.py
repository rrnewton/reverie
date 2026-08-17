#!/usr/bin/env python3
"""Bracket the APPROVED-AT binding GRAMMAR, including the rejection trap.

Four behaviours the convention has to have, and one it must NOT have:

  APPROVED-AT: <lane> <sha>                 binds
  APPROVED-AT <sha>                         stays UNDETERMINED
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
`gpt-5.6-sol`, which says neither "codex" nor "claude". The explicit lane makes
the binding deterministic without guessing from that model-name prose.
"""
import contextlib, importlib.util, io, os, sys
tool = os.environ.get("APPROVAL_BINDING_TOOL", "ci-hub/health/approval_binding.py")
spec = importlib.util.spec_from_file_location("ab", tool)
ab = importlib.util.module_from_spec(spec); spec.loader.exec_module(ab)
CUR="ef1392494c11911748dbfe0a42bf3635d1ccb3e0"; OLD="87f62930d3e24ab75b516d01cba106161e0cca2e"
def C(b): return [{"body": b}]
cases = []
def case(name, comments, lane, expect_state, expect_bound=None):
    shas = ab.lane_shas(comments, lane)
    st = ab.classify_approval(CUR, shas)
    ok = st == expect_state and (expect_bound is None or (shas[-1] if shas else None) == expect_bound)
    cases.append((ok, name, f"state={st} bound={shas[-1] if shas else None}"))

# 1. Only the lane-explicit form binds.
malformed_lane_less = []
lane_less = C(f"[adversarial-reviewer agent, CLAUDE] [x, y]\nAPPROVED-AT {CUR}\n")
lane_less_shas = ab.lane_shas(lane_less, "claude", malformed_lane_less)
cases.append((lane_less_shas == [] and len(malformed_lane_less) == 1,
              "APPROVED-AT <sha> without a lane is ABSENT authority",
              f"bound={lane_less_shas} malformed={len(malformed_lane_less)}"))
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
     C(f"[adversarial-reviewer agent, CLAUDE]\nREQUEST CHANGES AT {CUR}\n\nThe convention is:\nAPPROVED-AT: claude {CUR}\n"),
     "claude", ab.UNDETERMINED)
# 4. a push after a bound approval => SUPERSEDED, not green
case("bound approval + head moved => SUPERSEDED",
     C(f"[adversarial-reviewer agent, CLAUDE]\nAPPROVED-AT: claude {OLD}\n"), "claude", ab.SUPERSEDED, OLD)
# 5. lane isolation: a codex approval must not satisfy the claude lane
case("codex-lane approval does NOT satisfy the claude lane",
     C(f"[adversarial-reviewer agent, GPT-5 Codex] [codex-reviewer]\nAPPROVED-AT: codex {CUR}\n"), "claude", ab.UNDETERMINED)
case("codex-lane approval DOES satisfy the codex lane",
     C(f"[adversarial-reviewer agent, GPT-5 Codex] [codex-reviewer]\nAPPROVED-AT: codex {CUR}\n"), "codex", ab.BOUND_CURRENT, CUR)
# 6. The explicit lane, not a model-name guess, supplies lane identity.
case("ambiguous 'gpt-5.6-sol' tag with explicit lane still binds",
     C(f"[adversarial-reviewer agent, gpt-5.6-sol]\nAPPROVED-AT: codex {CUR}\n"), "codex", ab.BOUND_CURRENT, CUR)

# --- MARKDOWN EMPHASIS: decoration may wrap the canonical line. --------------
# Bolding or code-formatting does not change the mandatory colon, lane, or SHA.
case("**APPROVED-AT: claude <sha>** (bold) BINDS",
     C(f"[adversarial-reviewer agent, CLAUDE] [x]\n**APPROVED-AT: claude {CUR}**\n"), "claude", ab.BOUND_CURRENT, CUR)
case("`APPROVED-AT: claude <sha>` (backticked) BINDS",
     C(f"[adversarial-reviewer agent, claude-opus-5]\n`APPROVED-AT: claude {CUR}`\n"), "claude", ab.BOUND_CURRENT, CUR)
quoted = []
quoted_shas = ab.lane_shas(
    C(f"[adversarial-reviewer agent, claude-opus-5]\n"
      f"`APPROVED-AT: claude {CUR}` all stand exactly as posted -- this is not a re-approval.\n"),
    "claude", quoted)
cases.append((quoted_shas == [] and quoted == [],
              "inline-code binding quoted in prose neither BINDS nor reports malformed",
              f"bound={quoted_shas} malformed={len(quoted)}"))
case("bold REJECTION still does NOT bind",
     C(f"[adversarial-reviewer agent, CLAUDE]\n**REQUEST CHANGES AT {CUR}**\n"), "claude", ab.UNDETERMINED)
case("current CHANGES-REQUESTED-AT revokes an earlier exact-head approval",
     C(f"APPROVED-AT: codex {CUR}") + C(f"CHANGES-REQUESTED-AT: codex {CUR}"),
     "codex", ab.UNDETERMINED)
case("lane-specific CHANGES-REQUESTED-AT does not revoke the other lane",
     C(f"APPROVED-AT: claude {CUR}") + C(f"CHANGES-REQUESTED-AT: codex {CUR}"),
     "claude", ab.BOUND_CURRENT, CUR)
case("a later exact-head approval resolves an earlier request for changes",
     C(f"CHANGES-REQUESTED-AT: codex {CUR}") + C(f"APPROVED-AT: codex {CUR}"),
     "codex", ab.BOUND_CURRENT, CUR)

# --- LOUD ON THE UNKNOWN ------------------------------------------------------
# A line plainly trying to be a binding, matching neither grammar, must be
# REPORTED. Silently skipping an unrecognised variant is how variant three went
# unnoticed: an approval that exists but is not seen reads as no approval, and a
# consumer acting on that retires real authorisation.
mal = []
ab.lane_shas(C(f"[adversarial-reviewer agent, CLAUDE]\nAPPROVED @ {CUR}\n"), "claude", mal)
cases.append((len(mal) == 1, "an unrecognised binding-shaped line is REPORTED, not ignored",
              f"malformed={len(mal)}"))
# The live collector scans one comment for each approval lane.  The same raw
# malformed line is still one defect and must not inflate the headline count.
ab.lane_shas(C(f"[adversarial-reviewer agent, CLAUDE]\nAPPROVED @ {CUR}\n"), "codex", mal)
cases.append((len(mal) == 1, "one malformed source line is counted once across lanes",
              f"malformed={len(mal)}"))
mal2 = []
ab.lane_shas(C(f"[adversarial-reviewer agent, CLAUDE]\nAPPROVED-AT: claude {CUR}\n"), "claude", mal2)
cases.append((len(mal2) == 0, "a WELL-FORMED binding is not reported as malformed",
              f"malformed={len(mal2)}"))
mal3 = []
ab.lane_shas(C("[adversarial-reviewer agent, CLAUDE]\nThis looks good, nice work.\n"), "claude", mal3)
cases.append((len(mal3) == 0, "ordinary prose with no SHA is not reported as malformed",
              f"malformed={len(mal3)}"))

# The real #1847 shape: syntax debt and canonical authority can coexist in one
# comment.  Parsing must preserve BOTH facts so the report layer can assign
# them different severity and remedies instead of making one erase the other.
mal4 = []
mal4_prs = {}
same_comment = C(
    f"[adversarial-reviewer agent, claude-opus-5]\n"
    f"APPROVE {CUR}\n"
    f"APPROVED-AT: claude {CUR}\n"
)
same_comment_shas = ab.lane_shas(same_comment, "claude", mal4, 1847, mal4_prs)
cases.append((same_comment_shas == [CUR] and len(mal4) == 1 and mal4_prs.get(mal4[0]) == 1847,
              "same comment preserves canonical authority AND malformed syntax debt",
              f"bound={same_comment_shas} malformed={len(mal4)} pr={mal4_prs.get(mal4[0]) if mal4 else None}"))

receipt_comment = C(
    "<!-- locally-validated-receipt "
    f"commit={OLD} path=validation-receipts/rrnewton/hermit/{CUR}/"
    f"{'a' * 64}.json sha256={'b' * 64} -->"
)
cases.append((ab.local_receipt_heads(receipt_comment, "rrnewton/hermit") == [CUR],
              "locally-validated marker binds to the head in its receipt path",
              str(ab.local_receipt_heads(receipt_comment, "rrnewton/hermit"))))
wrong_repo = ab.local_receipt_heads(receipt_comment, "rrnewton/reverie")
cases.append((wrong_repo == [],
              "a receipt marker for another repository does not bind",
              str(wrong_repo)))

# Collection can fail before the ordinary verdict summary is constructed. That
# is a NO RESULT, and tick-hub still needs concrete capture fields to render it.
old_collect, old_argv = ab.collect_live, sys.argv
try:
    ab.collect_live = lambda *args, **kwargs: (_ for _ in ()).throw(
        ab.GitHubUnavailable("HTTP 503 {request}")
    )
    sys.argv = ["approval_binding.py", "--gate", "--gate-on", "superseded"]
    stdout, stderr = io.StringIO(), io.StringIO()
    with contextlib.redirect_stdout(stdout), contextlib.redirect_stderr(stderr):
        unavailable_rc = ab.main()
finally:
    ab.collect_live, sys.argv = old_collect, old_argv
unavailable_out = stdout.getvalue()
cases.append((unavailable_rc == 75
              and "state=unavailable" in unavailable_out
              and "summary=" in unavailable_out
              and "{summary}" not in unavailable_out
              and "{request}" not in unavailable_out,
              "live collection failure emits renderable NO RESULT fields",
              f"rc={unavailable_rc} out={unavailable_out.strip()}"))

p=sum(1 for ok,_,_ in cases if ok); f=len(cases)-p
for ok,n,d in cases: print(f"  {'PASS' if ok else 'FAIL'}  {n}   [{d}]")
print(f"assertions: {p} passed, {f} failed")
sys.exit(1 if f else 0)
