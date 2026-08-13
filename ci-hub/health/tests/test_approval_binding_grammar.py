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
import os, sys, importlib.util
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

# --- MARKDOWN-STRUCTURAL-PREFIX VERDICTS, BOTH DIRECTIONS ---------------------
# `undecorate` removes markdown that WRAPS a line -- matching opener and closer.
# A structural marker is a prefix with NO closer, so it survives untouched. A
# rejection written that way used to match nothing at all, and the lane then
# reported BOUND_CURRENT off an earlier approval while its newest verdict was a
# rejection at the SAME head: a false PASS that looks healthy. Live on #2176.
#
# The two directions are deliberately asymmetric. Read the comment on
# REJECT_PREFIX in approval_binding.py before making them symmetric.
for marker, label in (("##", "heading"), (">", "blockquote"), ("-", "list dash"),
                      ("*", "list star"), ("+", "list plus"), ("###", "deep heading")):
    case(f"a {label}-prefixed rejection REVOKES an earlier approval",
         C(f"APPROVED-AT: claude {CUR}") + C(f"{marker} CHANGES-REQUESTED-AT: claude {CUR}"),
         "claude", ab.UNDETERMINED)
case("a heading-prefixed LEGACY rejection revokes too",
     C(f"APPROVED-AT: codex {CUR}") + C(f"## REQUEST CHANGES AT {CUR}"),
     "codex", ab.UNDETERMINED)
# Positive half: revocation must not be permanent, or the lane could never
# re-earn authority and the gate becomes unsatisfiable.
case("a masked rejection followed by a fresh approval binds again",
     C(f"APPROVED-AT: claude {CUR}") + C(f"## CHANGES-REQUESTED-AT: claude {CUR}")
     + C(f"APPROVED-AT: claude {CUR}"),
     "claude", ab.BOUND_CURRENT, CUR)
# A masked rejection is lane-scoped exactly like an unmasked one.
case("a masked claude rejection does NOT revoke the codex lane",
     C(f"APPROVED-AT: codex {CUR}") + C(f"## CHANGES-REQUESTED-AT: claude {CUR}"),
     "codex", ab.BOUND_CURRENT, CUR)
# THE ASYMMETRY. A prefixed APPROVAL must never bind -- extending the prefix
# class to `approve` would let `## APPROVED-AT: ...` start authorising, which is
# the direction that loses. It is reported as malformed instead.
mal_pref = []
pref_approve = ab.lane_shas(C(f"## APPROVED-AT: claude {CUR}"), "claude", mal_pref)
cases.append((pref_approve == [] and len(mal_pref) == 1,
              "a heading-prefixed APPROVAL does NOT bind and is reported",
              f"bound={pref_approve} malformed={len(mal_pref)}"))
# A recognised rejection is understood, not malformed -- it has a known shape.
mal_rej = []
ab.lane_shas(C(f"## CHANGES-REQUESTED-AT: claude {CUR}"), "claude", mal_rej)
cases.append((len(mal_rej) == 0,
              "a masked rejection is honoured, not filed as malformed syntax",
              f"malformed={len(mal_rej)}"))
# Prose that merely mentions a marker and a SHA must stay inert.
mal_prose = []
ab.lane_shas(C(f"see the discussion at {CUR} for why"), "claude", mal_prose)
cases.append((len(mal_prose) == 0, "prose naming a SHA is still not a verdict",
              f"malformed={len(mal_prose)}"))

# --- MALFORMED SUPERSESSION ---------------------------------------------------
# A malformed line matters only if this lane has not already spoken past it. A
# blanket "any malformed line counts forever" rule made the gate PERMANENTLY
# UNSATISFIABLE for #2176 and #2172, which already hold four prose verdict
# headlines between them: no amount of correct re-approval would ever clear
# them, and a gate that cannot be satisfied by doing the right thing gets
# routed around.
mal_old = []
old_then_bind = ab.lane_shas(
    C(f"APPROVED AT EXACT HEAD {OLD}") + C(f"APPROVED-AT: claude {CUR}"), "claude", mal_old)
cases.append((old_then_bind == [CUR] and len(mal_old) == 0,
              "a malformed line BEFORE the newest binding is superseded history",
              f"bound={old_then_bind} malformed={len(mal_old)}"))
mal_after = []
bind_then_old = ab.lane_shas(
    C(f"APPROVED-AT: claude {CUR}") + C(f"APPROVED AT EXACT HEAD {CUR}"), "claude", mal_after)
cases.append((bind_then_old == [CUR] and len(mal_after) == 1,
              "a malformed line AFTER the newest binding is still an open question",
              f"bound={bind_then_old} malformed={len(mal_after)}"))
mal_never = []
never_bound = ab.lane_shas(C(f"APPROVED AT EXACT HEAD {OLD}"), "claude", mal_never)
cases.append((never_bound == [] and len(mal_never) == 1,
              "a malformed line still counts when the lane never bound at all",
              f"bound={never_bound} malformed={len(mal_never)}"))
# #1847 again, restated against the supersession rule: same comment means the
# malformed line is NOT before the binding, so it survives. Guards the boundary.
mal_same = []
same_idx = ab.lane_shas(C(f"APPROVE {CUR}\nAPPROVED-AT: claude {CUR}"), "claude", mal_same)
cases.append((same_idx == [CUR] and len(mal_same) == 1,
              "same-comment malformed line is NOT superseded by that comment's binding",
              f"bound={same_idx} malformed={len(mal_same)}"))
# A rejection resets the lane, so it must reset the supersession watermark too:
# an old malformed line is no longer 'spoken past' once the binding is revoked.
mal_reset = []
reset = ab.lane_shas(
    C(f"APPROVED AT EXACT HEAD {OLD}") + C(f"APPROVED-AT: claude {CUR}")
    + C(f"CHANGES-REQUESTED-AT: claude {CUR}"), "claude", mal_reset)
cases.append((reset == [] and len(mal_reset) == 1,
              "revocation resets the watermark; the old malformed line counts again",
              f"bound={reset} malformed={len(mal_reset)}"))


# ---------------------------------------------------------------------------
# THREE-STATE LANE VERDICTS (2026-08-13).
#
# lane_shas returned only APPROVED-AT SHAs, and a CHANGES-REQUESTED-AT cleared
# them, so a REJECTED lane and a SILENT one both surfaced as []. hermit-906
# rebuilt the review inventory by hand because of it. These cases bracket the
# fix in BOTH directions: a rejection must become distinguishable from silence,
# and an approval superseded by a later rejection must STILL be revoked.
# ---------------------------------------------------------------------------

def _lane(comments, head=None, lane="claude"):
    v = ab.lane_verdict(comments, lane)
    return ab.classify_lane(head or CUR, v), v

# DIRECTION 1 -- the defect. Rejection and silence must no longer be the same.
silent, _ = _lane(C("no verdict here at all"))
rejected, rej_v = _lane(C(f"CHANGES-REQUESTED-AT: claude {CUR}"))
cases.append((silent == ab.NEVER and rejected == ab.REJECTED_CURRENT,
              "DIRECTION 1: a rejection at head is distinguishable from silence",
              f"silent={silent} rejected={rejected}"))
cases.append((rej_v.rejected_sha == CUR,
              "the rejection reports WHICH head it named",
              f"rejected_sha={rej_v.rejected_sha}"))

# DIRECTION 2 -- the requirement that must not be broken to get direction 1.
revoked, _ = _lane(C(f"APPROVED-AT: claude {CUR}") + C(f"CHANGES-REQUESTED-AT: claude {CUR}"))
cases.append((revoked == ab.REJECTED_CURRENT,
              "DIRECTION 2: an approval superseded by a later rejection is STILL revoked",
              f"state={revoked}"))
cases.append((ab.lane_shas(C(f"APPROVED-AT: claude {CUR}")
                           + C(f"CHANGES-REQUESTED-AT: claude {CUR}"), "claude") == [],
              "DIRECTION 2: found.clear() still empties the approval list",
              "approved_shas must be []"))

# Revocation is chronological, so a later approval re-establishes authority AND
# clears the recorded rejection -- otherwise a correctly re-approved lane would
# keep reporting a rejection it has already spoken past.
reapproved, reapp_v = _lane(C(f"CHANGES-REQUESTED-AT: claude {OLD}") + C(f"APPROVED-AT: claude {CUR}"))
cases.append((reapproved == ab.BOUND_CURRENT and reapp_v.rejected_sha is None,
              "a later approval re-establishes authority and drops the stale rejection",
              f"state={reapproved} rejected_sha={reapp_v.rejected_sha}"))

# A rejection naming an older commit is its own state, not the current one.
old_rej, _ = _lane(C(f"CHANGES-REQUESTED-AT: claude {OLD}"))
cases.append((old_rej == ab.REJECTED_SUPERSEDED,
              "a rejection at an older head is REJECTED_SUPERSEDED, not REJECTED_CURRENT",
              f"state={old_rej}"))

# The masked rejection that produced the original false pass must reach the new
# state too, not fall back to NEVER.
masked, _ = _lane(C(f"APPROVED-AT: claude {CUR}") + C(f"## CHANGES-REQUESTED-AT: claude {CUR}"))
cases.append((masked == ab.REJECTED_CURRENT,
              "a ##-masked rejection surfaces as REJECTED_CURRENT, not NEVER",
              f"state={masked}"))

# The historical lane-less form names no lane and revokes BOTH.
legacy_cl, _ = _lane(C(f"APPROVED-AT: claude {CUR}") + C(f"REQUEST CHANGES AT {CUR}"))
legacy_cx, _ = _lane(C(f"APPROVED-AT: codex {CUR}") + C(f"REQUEST CHANGES AT {CUR}"), lane="codex")
cases.append((legacy_cl == ab.REJECTED_CURRENT and legacy_cx == ab.REJECTED_CURRENT,
              "the lane-less historical rejection revokes BOTH lanes",
              f"claude={legacy_cl} codex={legacy_cx}"))

# A rejection for the OTHER lane must not revoke this one.
other, _ = _lane(C(f"APPROVED-AT: claude {CUR}") + C(f"CHANGES-REQUESTED-AT: codex {CUR}"))
cases.append((other == ab.BOUND_CURRENT,
              "a rejection for the other lane leaves this lane bound",
              f"state={other}"))

# NON-REGRESSION: the gate vocabulary must be byte-identical, because every
# consumer of classify_approval is a GATE and a gate must refuse on both.
_gate_corpus = [
    C("nothing"),
    C(f"APPROVED-AT: claude {CUR}"),
    C(f"APPROVED-AT: claude {OLD}"),
    C(f"CHANGES-REQUESTED-AT: claude {CUR}"),
    C(f"CHANGES-REQUESTED-AT: claude {OLD}"),
    C(f"APPROVED-AT: claude {CUR}") + C(f"CHANGES-REQUESTED-AT: claude {CUR}"),
    C(f"CHANGES-REQUESTED-AT: claude {OLD}") + C(f"APPROVED-AT: claude {CUR}"),
]
_gate_states = [ab.classify_approval(CUR, ab.lane_shas(c, "claude")) for c in _gate_corpus]
cases.append((_gate_states == [ab.UNDETERMINED, ab.BOUND_CURRENT, ab.SUPERSEDED,
                               ab.UNDETERMINED, ab.UNDETERMINED, ab.UNDETERMINED,
                               ab.BOUND_CURRENT],
              "NON-REGRESSION: classify_approval is unchanged across the corpus",
              f"states={_gate_states}"))

# THE CONSUMER-COLLAPSE GUARD the brief asked for. pr_status.py reduces the
# result to `binding == BOUND_CURRENT`. Both non-approving states must stay
# false there -- a gate that started passing a rejected lane would be the
# regression this fix must not cause.
_has_approval = [ab.classify_approval(CUR, ab.lane_shas(c, "claude")) == ab.BOUND_CURRENT
                 for c in (C("nothing"), C(f"CHANGES-REQUESTED-AT: claude {CUR}"))]
cases.append((_has_approval == [False, False],
              "CONSUMER GUARD: pr_status's boolean stays False for BOTH silence and rejection",
              f"has_approval={_has_approval}"))

# And the third state must be genuinely reachable through the row the CLI
# emits, or the fix is real in the parser and invisible to every reader.
# NO hasattr GUARD: a first draft here wrapped this in `if hasattr(...)` and the
# case SILENTLY SKIPPED because the function is named collect_fixture, not
# collect_from_fixture. A conditionally-skipped assertion reports success while
# testing nothing, which is the exact shape this file exists to catch.
import json as _json, tempfile as _tempfile, os as _os
_fx = _tempfile.NamedTemporaryFile("w", suffix=".json", delete=False)
_json.dump([{
    "number": 1, "head": CUR, "labels": ["passed-review-claude"],
    "comments": [f"CHANGES-REQUESTED-AT: claude {CUR}"],
}], _fx)
_fx.close()
_rows, _ = ab.collect_fixture(_fx.name, [], {})
_os.unlink(_fx.name)
cases.append((len(_rows) == 1
              and _rows[0]["state"] == ab.UNDETERMINED
              and _rows[0]["lane_state"] == ab.REJECTED_CURRENT
              and _rows[0]["rejected_at"] == CUR,
              "the emitted row carries BOTH the gate state and the lane state",
              f"row={ {k: _rows[0][k] for k in ('state','lane_state','rejected_at')} if _rows else None}"))

# The same row for a SILENT lane must differ in lane_state while sharing the
# gate state -- otherwise the row exposes the collapse it was meant to remove.
_fx2 = _tempfile.NamedTemporaryFile("w", suffix=".json", delete=False)
_json.dump([{
    "number": 2, "head": CUR, "labels": ["passed-review-claude"],
    "comments": ["no verdict at all"],
}], _fx2)
_fx2.close()
_rows2, _ = ab.collect_fixture(_fx2.name, [], {})
_os.unlink(_fx2.name)
cases.append((_rows2[0]["state"] == _rows[0]["state"]
              and _rows2[0]["lane_state"] == ab.NEVER
              and _rows[0]["lane_state"] == ab.REJECTED_CURRENT,
              "ROW-LEVEL BRACKET: same gate state, DIFFERENT lane_state for rejected vs silent",
              f"gate={_rows2[0]['state']} silent={_rows2[0]['lane_state']} rejected={_rows[0]['lane_state']}"))

p=sum(1 for ok,_,_ in cases if ok); f=len(cases)-p
for ok,n,d in cases: print(f"  {'PASS' if ok else 'FAIL'}  {n}   [{d}]")
print(f"assertions: {p} passed, {f} failed")
sys.exit(1 if f else 0)
