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
from pathlib import Path

ROOT = Path(__file__).resolve().parents[3]
sys.path.insert(0, str(ROOT / "agent-utils" / "py"))
from tick_hub.engine import (  # noqa: E402
    UnresolvedPlaceholderError,
    parse_kv_lines,
    render_emit,
)
from tick_hub.io import config_from_yaml  # noqa: E402

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
legacy = []
ab.lane_shas(C(f"APPROVE {CUR}"), "claude", legacy)
cases.append((len(legacy) == 1,
              "the historical one-argument APPROVE assertion is still REPORTED",
              f"malformed={len(legacy)}"))
# Assertion versus mention is decided by line syntax, not by whether a SHA-like
# value happens to be present. These two directions pin the distinction: the
# standalone marker is an attempted verdict even with its SHA missing, while the
# same token and a full SHA inside a prose sentence are only a mention.
missing_sha = []
ab.lane_shas(C("APPROVED-AT: claude"), "claude", missing_sha)
cases.append((len(missing_sha) == 1,
              "a standalone APPROVED-AT with no SHA is REPORTED",
              f"malformed={len(missing_sha)}"))
prose_mention = []
ab.lane_shas(
    C(f"The required form is APPROVED-AT: claude {CUR}, written on its own line."),
    "claude", prose_mention,
)
cases.append((prose_mention == [],
              "prose mentioning a complete APPROVED-AT is not a malformed verdict",
              f"malformed={len(prose_mention)}"))
lead_in_assertion = []
lead_in_shas = ab.lane_shas(
    C(f"VERDICT: APPROVED-AT: claude {CUR} because this head is ready"),
    "claude", lead_in_assertion,
)
cases.append((lead_in_shas == [CUR] and lead_in_assertion == [],
              "an explicit VERDICT lead-in binds instead of being ignored",
              f"shas={lead_in_shas} malformed={len(lead_in_assertion)}"))
lead_in_refusal = []
lead_in_refused_shas = []
lead_in_remaining = ab.lane_shas(
    C(f"APPROVED-AT: codex {CUR}")
    + C(f"VERDICT: For this head, CHANGES-REQUESTED-AT: codex {CUR} because it is blocked"),
    "codex", lead_in_refusal, refusals=lead_in_refused_shas,
)
cases.append((
    lead_in_remaining == []
    and lead_in_refused_shas == [CUR]
    and lead_in_refusal == [],
    "an explicit VERDICT lead-in records a refusal instead of being ignored",
    f"bound={lead_in_remaining} refused={lead_in_refused_shas} "
    f"malformed={len(lead_in_refusal)}",
))
two_marker_line = (
    f"VERDICT: The previous APPROVED-AT: codex {CUR} is no longer my verdict; "
    f"CHANGES-REQUESTED-AT: codex {CUR} because this head is blocked"
)
two_marker_malformed = []
two_marker_shas = ab.lane_shas(C(two_marker_line), "codex", two_marker_malformed)
cases.append((
    two_marker_shas == [] and len(two_marker_malformed) == 1,
    "a VERDICT line with two markers grants neither occurrence",
    f"shas={two_marker_shas} malformed={len(two_marker_malformed)}",
))

# Inline code has the same split. A whole-line code span is existing marker
# decoration, so a missing SHA is still reported. Prose around the span makes it
# a mention and must not be flagged.
quoted_assertion = []
ab.lane_shas(C("`APPROVED-AT: claude`"), "claude", quoted_assertion)
cases.append((len(quoted_assertion) == 1,
              "a whole-line code span with a missing SHA is REPORTED",
              f"malformed={len(quoted_assertion)}"))
quoted_mention = []
ab.lane_shas(C("The incomplete form is `APPROVED-AT: claude`."), "claude", quoted_mention)
cases.append((quoted_mention == [],
              "an inline-code mention with a missing SHA is not a malformed verdict",
              f"malformed={len(quoted_mention)}"))
assertion_with_mention = []
ab.lane_shas(
    C("APPROVED-AT: claude -- unlike `APPROVED-AT: codex`"),
    "claude", assertion_with_mention,
)
cases.append((len(assertion_with_mention) == 1,
              "a later inline-code mention cannot hide a leading assertion",
              f"malformed={len(assertion_with_mention)}"))
# The live collector scans one comment for each approval lane.  The same raw
# malformed line is still one defect and must not inflate the headline count.
ab.lane_shas(C(f"[adversarial-reviewer agent, CLAUDE]\nAPPROVED @ {CUR}\n"), "codex", mal)
cases.append((len(mal) == 1, "one malformed source line is counted once across lanes",
              f"malformed={len(mal)}"))
# --- A WELL-FORMED WITHDRAWAL IS NOT A MALFORMED LINE -------------------------
# ⚠️ THE GATE PENALISED THE ONE CORRECT BEHAVIOUR. `approved_binding()` returns
# None for a withdrawal (it is not an approval), so the line fell into the
# near-miss branch, whose `_kind` had no withdrawn case and read any line
# containing "CHANGES-REQUESTED" as a REFUSAL. A withdrawal contains that
# substring by construction, so it failed the citation test and was counted
# malformed.
#
# MEASURED on hermit#2672: malformed_source_lines=1, and the single detail was a
# deliberate, correctly-spelled withdrawal used to discharge a refusal after
# reading the delta. Letting a refusal lapse silently scored clean; withdrawing
# it explicitly scored as a defect -- and withdrawal is the remedy for nearly
# every way a verdict decays, so the grammar was making its own fix unusable.
mal_wd = []
ab.lane_shas(C(f"CHANGES-REQUESTED-WITHDRAWN-AT: claude {CUR}"), "claude", mal_wd)
cases.append((len(mal_wd) == 0,
              "a well-formed WITHDRAWAL is not reported as malformed",
              f"malformed={len(mal_wd)}"))
# ...and the same line for the OTHER lane, since the near-miss branch runs per
# lane and the original defect counted once per scan.
ab.lane_shas(C(f"CHANGES-REQUESTED-WITHDRAWN-AT: claude {CUR}"), "codex", mal_wd)
cases.append((len(mal_wd) == 0,
              "a well-formed WITHDRAWAL is not malformed on the other lane either",
              f"malformed={len(mal_wd)}"))
# ⚠️ THE CONTROL THAT MUST STILL FIRE. Without it, exempting withdrawals could be
# implemented as "any line containing CHANGES-REQUESTED-WITHDRAWN is fine", which
# would wave through a genuinely broken attempt at one.
mal_bad_wd = []
ab.lane_shas(C(f"CHANGES-REQUESTED-WITHDRAWN @ {CUR}"), "claude", mal_bad_wd)
cases.append((len(mal_bad_wd) == 1,
              "a MALFORMED withdrawal-shaped line is still REPORTED",
              f"malformed={len(mal_bad_wd)}"))

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
    "APPROVED-AT: claude\n"
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

# --- landing observability: surfacing only, and it must not flag everything ----
# The reporter earns its place only if it stays quiet about the path that IS
# observed. A reporter that flags every landing is ignored within a week, so
# both directions are bracketed here.

direct = [{"sha": "a"*40, "prs": []}]
r = ab.landing_observability(direct)
cases.append((r["landed_without_pr"] == 1 and r["landed_via_pr"] == 0,
              "a direct-to-main landing is reported as unobserved",
              f"without_pr={r['landed_without_pr']} via_pr={r['landed_via_pr']}"))

# The live collector currently returns only PR numbers. Missing approved_at is
# therefore a missing measurement, not evidence that zero PRs carried bindings.
unpopulated_pr = [{"sha": "f"*40, "prs": [{"number": 6}]}]
r = ab.landing_observability(unpopulated_pr)
cases.append((r["pr_landings_carrying_a_binding"] == "NOT MEASURED"
              and "APPROVED-AT binding coverage is NOT MEASURED" in r["summary"]
              and "of which 0 came from" not in r["summary"],
              "an unpopulated approved_at field renders NOT MEASURED, not zero",
              r["summary"]))

bound_pr = [{"sha": "b"*40, "prs": [{"number": 7, "approved_at": ["c"*40]}]}]
r = ab.landing_observability(bound_pr)
cases.append((r["landed_without_pr"] == 0 and r["pr_landings_carrying_a_binding"] == 1,
              "a properly bound PR landing is NOT flagged",
              f"without_pr={r['landed_without_pr']} bound={r['pr_landings_carrying_a_binding']}"))

populated_unbound_pr = [
    {"sha": "0"*40, "prs": [{"number": 8, "approved_at": []}]},
]
r = ab.landing_observability(populated_unbound_pr)
cases.append((r["pr_landings_carrying_a_binding"] == 0
              and "of which 0 came from a PR carrying" in r["summary"]
              and "NOT MEASURED" not in r["summary"],
              "a populated approved_at field still renders its real zero count",
              r["summary"]))

# A squash merge gives main a different sha from the PR head. Requiring equality
# would report every squash as unbound; the binding is counted per-PR, not per-sha.
squashed = [{"sha": "d"*40, "prs": [{"number": 9, "approved_at": ["e"*40]}]}]
r = ab.landing_observability(squashed)
cases.append((r["pr_landings_carrying_a_binding"] == 1,
              "a squash merge whose sha differs from the bound head is not a false alarm",
              f"bound={r['pr_landings_carrying_a_binding']}"))

mixed = ab.landing_observability(direct + bound_pr)
cases.append((mixed["window_commits"] == 2 and mixed["landed_without_pr"] == 1
              and mixed["landed_via_pr"] == 1,
              "a mixed window separates the two populations",
              mixed["summary"][:70]))

cases.append(("unobserved" in mixed["summary"] and "unreviewed" in mixed["summary"]
              and "not a review verdict" in mixed["summary"],
              "the wording says unobserved and explicitly disclaims unreviewed",
              "summary carries the disclaimer"))

# A zero workflow-run count only establishes "unevaluated" while Actions are
# enabled.  Bracket both directions so accounting for the precondition cannot
# silently turn into disabling the escalation.
zero_run_row = {"number": 7, "runs_at_head": 0}
cases.append((ab.unevaluated_binding_rows([zero_run_row], False) == [],
              "zero runs do not mean unevaluated while Actions are disabled",
              "disabled precondition produces no unevaluated row"))
cases.append((ab.unevaluated_binding_rows([zero_run_row], True) == [zero_run_row],
              "zero runs still mean unevaluated while Actions are enabled",
              "enabled precondition preserves the escalation"))

# The production tick path is capture -> interpolate. The reporter used to
# print `landing-observability: ...`, which computed the right value but gave
# parse_kv_lines no `summary` field, so the renderer correctly refused the
# unresolved placeholder. Exercise the actual configured emit in both
# directions: computed output renders, missing output still refuses.
stdout = io.StringIO()
with contextlib.redirect_stdout(stdout):
    ab.emit_landing_observability(mixed, False)
captured = parse_kv_lines(stdout.getvalue())
config = config_from_yaml((ROOT / "ci-hub" / "health" / "tick-hub.yaml").read_text())
landing_reminder = config.by_name()["landing_observability"]
rendered = render_emit(landing_reminder.emit, captured)
cases.append((captured.get("summary") == mixed["summary"]
              and mixed["summary"] in rendered
              and "{summary}" not in rendered,
              "computed landing summary survives capture and renders in the live template",
              rendered[:100]))

stdout = io.StringIO()
with contextlib.redirect_stdout(stdout):
    ab.emit_landing_observability(ab.landing_observability(unpopulated_pr), False)
captured = parse_kv_lines(stdout.getvalue())
rendered = render_emit(landing_reminder.emit, captured)
cases.append(("APPROVED-AT binding coverage is NOT MEASURED" in rendered
              and "{summary}" not in rendered,
              "the live template emits the NOT MEASURED result rather than going quiet",
              rendered[:140]))

missing_summary_refused = False
try:
    render_emit(landing_reminder.emit, {})
except UnresolvedPlaceholderError:
    missing_summary_refused = True
cases.append((missing_summary_refused,
              "the live template still refuses when the producer omits summary",
              "UnresolvedPlaceholderError raised" if missing_summary_refused
              else "missing summary rendered"))

empty_refused = False
try:
    ab.landing_observability([])
except ab.EmptyLandingWindow:
    empty_refused = True
cases.append((empty_refused,
              "an empty window REFUSES instead of reporting a clean 0 of 0",
              "EmptyLandingWindow raised" if empty_refused else "returned a count over nothing"))

# ---------------------------------------------------------------------------
# THE FENCE RULE. An illustration is not an instruction -- and the rule is
# SYMMETRIC, which is the half that is easy to get wrong.
#
# Measured 2026-08-25 across all 28 open heads through both landed lander paths:
# gh-merge-verified refused 12 of which SIX carried no refusal naming that head,
# and changes-requested-gate refused 7 of which one did. hermit#2611 was refused
# by BOTH while approved by BOTH lanes; what refused it was a fenced example in a
# comment teaching a reviewer the grammar.
#
# ⚠️ AND THESE REFUSALS ARE PERMANENT. A refusal naming a sha stops naming it when
# the author pushes. An example carrying no sha never named this head, so no
# author action clears it.

FENCE = "`" * 3

# A fenced APPROVAL must not bind -- otherwise quoting the approval grammar
# GRANTS a lane, which is a fail-open and strictly worse than the refusal side.
# The landed parser at 32faa7e2 did bind this.
case("a FENCED approval does not bind", C(f"see the form:\n{FENCE}\nAPPROVED-AT: claude {CUR}\n{FENCE}\n"),
     "claude", "UNDETERMINED")
case("a TILDE-fenced approval does not bind", C(f"~~~\nAPPROVED-AT: claude {CUR}\n~~~\n"),
     "claude", "UNDETERMINED")
case("an INDENTED-block approval does not bind", C(f"Example:\n\n    APPROVED-AT: claude {CUR}\n\nend\n"),
     "claude", "UNDETERMINED")
case("a fenced block with an info string still hides its contents",
     C(f"{FENCE}text\nAPPROVED-AT: claude {CUR}\n{FENCE}\n"), "claude", "UNDETERMINED")

# ...while an ordinary approval is untouched. Without these the fence rule could
# pass by making the parser blind to everything.
case("a plain approval still binds", C(f"APPROVED-AT: claude {CUR}\n"), "claude", "BOUND_CURRENT", CUR)
case("a heading approval still binds", C(f"## APPROVED-AT: claude {CUR}\n"), "claude", "BOUND_CURRENT", CUR)
case("an approval AFTER a closed fence still binds",
     C(f"{FENCE}\nsome transcript\n{FENCE}\n\nAPPROVED-AT: claude {CUR}\n"), "claude", "BOUND_CURRENT", CUR)

# An UNCLOSED fence runs to the end of the comment, matching what the renderer
# shows a human. The gate must agree with the reader.
case("an unclosed fence hides what follows it",
     C(f"{FENCE}\nAPPROVED-AT: claude {CUR}\n"), "claude", "UNDETERMINED")

# ⚠️ THE INDENT RULE NEEDS A BLANK LINE BEFORE IT, or it would eat real verdicts
# indented under a bullet. This is the case that keeps the rule honest.
case("an indented continuation under a bullet is NOT a code block",
     C(f"- context here\n    APPROVED-AT: claude {CUR}\n"), "claude", "BOUND_CURRENT", CUR)

_fenced_refusal = ab.marker_lines(f"{FENCE}\nCHANGES-REQUESTED-AT: claude {CUR}\n{FENCE}\n")
cases.append((all("CHANGES-REQUESTED-AT" not in l for l in _fenced_refusal),
              "a FENCED refusal is not an eligible marker line",
              f"eligible={_fenced_refusal}"))

_real_refusal = ab.marker_lines(f"CHANGES-REQUESTED-AT: claude {CUR}\n")
cases.append((any("CHANGES-REQUESTED-AT" in l for l in _real_refusal),
              "a REAL refusal is still an eligible marker line",
              f"eligible={_real_refusal}"))

cases.append((ab.is_template("<lane> <sha>") and not ab.is_template(f" claude {CUR}"),
              "the template rule lives in the shared grammar, not in one lander",
              "is_template"))


# ---------------------------------------------------------------------------
# THE NEAR-MISS NET. A refusal that omits `-AT` is reported MALFORMED and does not
# parse -- so if the landers' fail-closed branch does not catch it either, a real
# block is invisible to both. Measured live on hermit#2611:
# `CHANGES-REQUESTED: claude 7a75909c92ae` at the exact head, flagged, not honoured.
#
# ⚠️ THESE EXIST BECAUSE I NARROWED THIS NET MYSELF IN `ff2a3d83` and dropped three
# spellings while fixing a fourth. The rows below are the ones that regressed.

for _form in (
    f"CHANGES-REQUESTED: claude {CUR}",
    f"CHANGES REQUESTED: claude {CUR}",
    f"REJECTED: claude {CUR}",
    "CHANGES-REQUESTED: claude -- see above",
):
    cases.append((ab.refusal_shaped(_form),
                  f"a near-miss refusal is still refusal-SHAPED: {_form[:34]}",
                  f"refusal_shaped={ab.refusal_shaped(_form)}"))

# ...and the narrowing that WAS wanted stays: prose mentioning a marker mid-line is
# not a verdict, which is what stopped a sentence adjudicating another pull request
# from refusing a merge.
for _prose in (
    f"I adjudicated hermit#2591: CHANGES-REQUESTED-AT: claude {CUR} was posted",
    "this PR has no CHANGES-REQUESTED-AT at its head",
    f"post CHANGES-REQUESTED-AT: <lane> {CUR} to block it",
):
    cases.append((not ab.refusal_shaped(_prose),
                  f"prose mentioning a marker is NOT refusal-shaped: {_prose[:34]}",
                  f"refusal_shaped={ab.refusal_shaped(_prose)}"))

# A decorated near-miss must still be caught -- decoration is stripped first.
cases.append((ab.refusal_shaped(f"## CHANGES-REQUESTED: claude {CUR}"),
              "a heading-decorated near-miss refusal is refusal-shaped",
              "heading + missing -AT"))

# And a fenced one must not be, because eligibility is decided before this runs.
_fenced = ab.marker_lines("```\n" + f"CHANGES-REQUESTED: claude {CUR}" + "\n```")
cases.append((not any(ab.refusal_shaped(l) for l in _fenced),
              "a FENCED near-miss refusal is not an eligible marker line",
              f"eligible={_fenced}"))


# The two landers must not diverge on the near-miss family a second time.
# `ff2a3d83` narrowed it, `7db64676` restored it in `refusal_shaped()` only, and
# `changes-requested-gate`'s own extractor still could not see a missing-`-AT`
# line -- the permissive side being the path `land-pr.sh` uses.
_nm = ab.near_miss_refusal(f"CHANGES-REQUESTED: claude {CUR}")
cases.append((_nm is not None and _nm.start() == 0,
              "near_miss_refusal locates a missing-AT refusal for BOTH landers",
              f"match={_nm}"))
cases.append((ab.near_miss_refusal("nothing to see here") is None,
              "near_miss_refusal does not fire on ordinary prose",
              "no match"))


# ⚠️ THE NEAR-MISS NET MUST NOT FIRE ON THE GATE'S OWN NAME. `changes-requested-gate`
# contains the token and `\b` was satisfied by the hyphen, so every sentence naming
# the tool became an unbindable refusal. Measured on hermit#2624 -- a head with no
# refusal on it -- which went INDETERMINATE and could not merge.
for _benign in (
    "`changes-requested-gate` **CLEAR**; no markers of any shape on this PR",
    "the changes-requested-gate reports CLEAR",
    "see ci-hub/bin/changes-requested-gate for the rule",
):
    cases.append((not ab.refusal_shaped(_benign) and ab.near_miss_refusal(_benign) is None,
                  f"naming the gate is not a refusal: {_benign[:38]}",
                  f"shaped={ab.refusal_shaped(_benign)}"))

# A WITHDRAWAL IS NOT A REFUSAL EITHER -- the same trailing guard gives this for free.
cases.append((not ab.refusal_shaped(f"CHANGES-REQUESTED-WITHDRAWN-AT: claude {CUR}"),
              "a withdrawal is not refusal-shaped",
              "withdrawn"))

# ...and the forms that must still fire, so the guard cannot be over-tightened.
for _real in (f"CHANGES-REQUESTED-AT: claude {CUR}", f"CHANGES-REQUESTED: claude {CUR}",
              f"CHANGES REQUESTED: claude {CUR}", f"REJECTED: claude {CUR}"):
    cases.append((ab.refusal_shaped(_real),
                  f"a real refusal still fires: {_real[:34]}",
                  "fires"))

# --- A MARKER INSIDE PROSE IS A MENTION. --------------------------------------
# hermit#2660 had BOTH lanes bound at the exact head by non-authors and could not
# land, because one clause of the codex reviewer's APPROVAL mentioned a refusal
# prepared for a DIFFERENT pull request. It parsed as a refusal with a lane and no
# sha, and a refusal that names no sha cannot be bound, so the gate returned rc=2.
#
# ⚠️ THE STRANDING IS THE DEFECT, NOT THE FALSE POSITIVE. Pushing cannot clear a
# clause that names no sha -- it never stops naming the head -- so the only exits
# were editing another reviewer's comment or posting a withdrawal for a refusal
# nobody made. The remedy is NOT to bind it to the current head: a marker that
# silently adopts whatever sha is current is how an approval outlives the code it
# approved. The line stays available to callers, but it is not a marker assertion.
_QUOTED_NO_SHA = "have a `CHANGES-REQUESTED-AT: codex` prepared for #2658, not posted"
cases.append((not any(ab.marker_assertion(line) for line in ab.marker_lines(_QUOTED_NO_SHA)),
              "a sha-less refusal in a code span is a citation, not a verdict",
              f"lines={ab.marker_lines(_QUOTED_NO_SHA)}"))

# ⚠️ THE CONTROL WITH A FULL SHA. Value length does not turn a mention into an
# assertion; the same shared boundary applies with or without the value.
_QUOTED_WITH_SHA = f"see `CHANGES-REQUESTED-AT: codex {CUR}` on the earlier head"
cases.append((not any(ab.marker_assertion(line) for line in ab.marker_lines(_QUOTED_WITH_SHA)),
              "a code-span mention carrying a SHA is still not an assertion",
              f"lines={ab.marker_lines(_QUOTED_WITH_SHA)}"))

# Malformed, not inferred: a sha-less refusal written as a verdict line is still
# seen. Dropping this one would admit a head its reviewer meant to block.
_PLAIN_NO_SHA = "CHANGES-REQUESTED-AT: claude -- see above"
cases.append((any(ab.marker_assertion(line) for line in ab.marker_lines(_PLAIN_NO_SHA)),
              "a sha-less refusal in PLAIN prose is still seen",
              f"lines={ab.marker_lines(_PLAIN_NO_SHA)}"))

_FULL_MARKER_MENTION = (
    "The migration note reproduces the retired review text exactly: "
    f"CHANGES-REQUESTED-AT: codex {CUR}"
)
cases.append((not any(ab.marker_assertion(line)
                      for line in ab.marker_lines(_FULL_MARKER_MENTION)),
              "a full marker in prose is a mention",
              f"lines={ab.marker_lines(_FULL_MARKER_MENTION)}"))

_LEAD_IN_REFUSAL = f"## CODEX-LANE CHANGES-REQUESTED-AT: codex `{CUR}` -- still blocked"
cases.append((any(ab.marker_assertion(line) for line in ab.marker_lines(_LEAD_IN_REFUSAL)),
              "the named CODEX-LANE refusal with trailing prose is seen",
              f"lines={ab.marker_lines(_LEAD_IN_REFUSAL)}"))

# Symmetric, exactly as the fence rule is: withholding an approval is the safe
# direction, and a sha-less approval could not bind a head anyway.
cases.append((not any(ab.marker_assertion(line) for line in
                      ab.marker_lines("as `APPROVED-AT: claude` would say")),
              "a sha-less approval in a code span does not bind either",
              "symmetric"))


def test_approval_binding_grammar():
    """Assert the cases collected above, so pytest ENFORCES them.

    This file used to end in a module-level ``sys.exit``. Under pytest that
    raised SystemExit during collection and aborted the whole mainloop, so all
    466 tests discovered in this directory reported zero executed. Printing a
    tally is not a test; only an assertion is.
    """
    failed = [f"{n}   [{d}]" for ok, n, d in cases if not ok]
    assert not failed, "approval-binding grammar cases failed:\n  " + "\n  ".join(failed)


if __name__ == "__main__":
    p=sum(1 for ok,_,_ in cases if ok); f=len(cases)-p
    for ok,n,d in cases: print(f"  {'PASS' if ok else 'FAIL'}  {n}   [{d}]")
    print(f"assertions: {p} passed, {f} failed")
    sys.exit(1 if f else 0)
