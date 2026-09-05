#!/usr/bin/env bash
# Bracket approval_binding.py in BOTH directions, plus the composed state.
#
# An inert detector is the failure mode here, not a strict one. The review of PR
# #2026 found a mandatory-review gate that accepted six evasion mutations 6/6 --
# it enforced nothing and nobody noticed, because a check that never fires looks
# exactly like a check with nothing to report. So the POSITIVE half is asserted
# as hard as the negative, and a mutation run at the end proves the suite is not
# vacuous.
#
#   REFUSED     selected PR with ZERO review artifacts -> missing authority
#   FLAGGED     approval bound to an OLDER head        -> SUPERSEDED
#   FLAGGED     label with NO SHA in any comment       -> UNDETERMINED
#   NOT FLAGGED approval naming the CURRENT head       -> BOUND_CURRENT, rc=0
#   FLAGGED     superseded AND zero runs at head       -> the composed state
#
# UNDETERMINED and SUPERSEDED are kept distinct on purpose. Superseded means we know
# what was reviewed and the head moved past it. Unbound means we cannot tell what
# was ever reviewed. Collapsing them would hide the second, which is the worse of
# the two -- it is a cache with no authority anywhere behind it.
#
# Inert: pure fixtures through --from-json. No network, no gh, no live repo.
set -uo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
tool="${APPROVAL_BINDING_TOOL:-$here/../approval_binding.py}"
# ⚠️ EVERY MUTATION CONTROL IN THIS FILE WAS DEAD WITHOUT THIS LINE, AND THEY FAIL
# LOUDLY RATHER THAN SILENTLY, WHICH IS THE ONLY REASON IT WAS FINDABLE.
# `approval_binding.py` reaches its sibling `bounded_capture` with
# `sys.path.insert(0, Path(__file__).resolve().parents[1] / "lib")` -- resolved
# from the FILE's location. Every mutation control below `sed`s the tool into
# `$tmp` and runs the copy, where `parents[1]/lib` is `/lib`, so the copy dies at
# import before executing a single assertion. The control then sees a nonzero
# child and no named failure, and reports "the mutant did not fail" -- which reads
# as "the guard is not load-bearing" when it means "the mutant never ran".
#
# Measured 2026-08-26 at 57a21d85: two controls (#2132 zero-approval guard, and the
# source guard) were already failing this way on main. Exporting the lib directory
# makes a relocated copy importable and restores all of them.
export PYTHONPATH="$here/../../lib${PYTHONPATH:+:$PYTHONPATH}"
tmp="$(mktemp -d "${TMPDIR:-/tmp}/approval-binding-test.XXXXXX")"
trap 'rm -rf "$tmp"' EXIT

CUR=b2e44120e984e85ab6a70fd5749ad6d3282a352e
OLD=87f62930d3e24ab75b516d01cba106161e0cca2e

pass=0; fail=0
ok()  { printf '  PASS  %s\n' "$*"; pass=$((pass+1)); }
bad() { printf '  FAIL  %s\n' "$*"; fail=$((fail+1)); }

run() { python3 "$tool" --from-json "$1" "${@:2}" 2>&1; }

# --- LOAD-BEARING REFUSAL: the exact #2132 planted shape --------------------
# Before this repair the final lander selected #2132, the collector discarded
# it because there were no passed-review labels, and --gate returned rc0 over
# an empty population: approvals=0, rows=[].  Keep that exact PR number and
# shape so a synthetic near-miss cannot conceal a regression.
cat >"$tmp/zero.json" <<JSON
[{"number":2132,"head":"$CUR","labels":[],
  "lane_shas":{},"runs_at_head":9,"mergeable":"MERGEABLE"}]
JSON
out="$(run "$tmp/zero.json" --pr 2132 --gate)"; rc=$?
[[ $rc -eq 1 ]] && ok "zero approvals: selected PR gate REFUSES (rc=1)" \
  || bad "zero approvals: expected rc=1, got $rc"
grep -q "PR #2132 has zero review artifacts" <<<"$out" \
  && ok "zero-approval refusal identifies the selected PR and missing evidence" \
  || bad "zero-approval refusal is not explicit: $out"
out="$(run "$tmp/zero.json" --pr 2132 --gate --json)"; rc=$?
[[ $rc -eq 1 ]] && grep -q '"approval_set_missing": true' <<<"$out" \
  && grep -q '"required_approval_sets": 1' <<<"$out" \
  && grep -q '"approval_sets_missing": 1' <<<"$out" \
  && ok "zero-approval JSON reports 1/1 required approval sets missing" \
  || bad "zero-approval JSON does not carry its required-set denominator: $out"

# --- POSITIVE 1: superseded approval must be flagged -------------------------
# This is #1864's real shape: codex approved 87f62930, head moved to b2e44120.
cat >"$tmp/superseded.json" <<JSON
[{"number":1864,"head":"$CUR","labels":["passed-review-codex"],
  "lane_shas":{"codex":["$OLD"]},"runs_at_head":3,"mergeable":"CONFLICTING"}]
JSON
out="$(run "$tmp/superseded.json" --gate)"; rc=$?
[[ $rc -eq 1 ]] && ok "superseded approval: gate FAILS (rc=1)" || bad "superseded: expected rc=1, got $rc"
grep -q "SUPERSEDED    PR 1864" <<<"$out" && ok "superseded names the PR, the label and both SHAs" \
  || bad "superseded not named: $out"
grep -q "^summary=1/1 review/local-validation artifacts fail the all binding check; affected=#1864;" <<<"$out" \
  && ok "gate emits a concrete scheduled-tick summary" \
  || bad "scheduled-tick summary missing or unrendered: $out"

# --- POSITIVE 2: a label with no SHA anywhere is UNDETERMINED, not assumed good ---
cat >"$tmp/unbound.json" <<JSON
[{"number":999,"head":"$CUR","labels":["passed-review-claude"],
  "lane_shas":{},"runs_at_head":5,"mergeable":"MERGEABLE"}]
JSON
out="$(run "$tmp/unbound.json" --gate)"; rc=$?
[[ $rc -eq 1 ]] && ok "undetermined approval: gate FAILS (rc=1)" || bad "unbound: expected rc=1, got $rc"
grep -q "no APPROVED-AT binding" <<<"$out" \
  && ok "undetermined says WHY -- no APPROVED-AT binding -- rather than just failing" \
  || bad "undetermined gave no reason: $out"
grep -q "bound to current head: 0; authority absent: 1" <<<"$out" \
  && ok "unbound label counts as 0/1 authority, not present-but-unverified" \
  || bad "unbound authority count is not explicit: $out"

# --- NEGATIVE: an approval naming the CURRENT head must NOT be flagged -------
# Without this the tool could reject everything and look diligent.
cat >"$tmp/current.json" <<JSON
[{"number":1666,"head":"$CUR","labels":["passed-review-claude","passed-review-codex"],
  "lane_shas":{"claude":["$OLD","$CUR"],"codex":["$CUR"]},"runs_at_head":16,"mergeable":"MERGEABLE"}]
JSON
out="$(run "$tmp/current.json" --pr 1666 --gate)"; rc=$?
[[ $rc -eq 0 ]] && ok "current approvals: selected PR gate PASSES (rc=0)" || bad "current: expected rc=0, got $rc"
grep -q "every review/local-validation artifact is bound to the current head" <<<"$out" \
  && ok "current approvals reported as holding" || bad "current approvals not reported clean: $out"
grep -q "bound to current head: 2; authority absent: 0" <<<"$out" \
  && ok "exact-head bindings count as 2/2 authority" \
  || bad "bound authority count is not explicit: $out"
# The claude lane re-reviewed: an older SHA followed by the current one still holds.
grep -qE "SUPERSEDED|UNDETERMINED" <<<"$out" && bad "a re-review (old SHA then current) was wrongly flagged" \
  || ok "a lane that re-reviewed at the new head holds, despite an older SHA in its history"

# --- LOCALLY-VALIDATED USES ITS EXISTING EXACT-HEAD RECEIPT MARKER -----------
cat >"$tmp/local-stale.json" <<JSON
[{"number":2108,"head":"$CUR","labels":["locally-validated"],
  "comments":["<!-- locally-validated-receipt commit=$CUR path=validation-receipts/rrnewton/hermit/$OLD/$(printf 'a%.0s' {1..64}).json sha256=$(printf 'b%.0s' {1..64}) -->"],
  "runs_at_head":0,"mergeable":"CONFLICTING"}]
JSON
out="$(run "$tmp/local-stale.json" --gate --gate-on superseded)"; rc=$?
[[ $rc -eq 1 ]] && grep -q "SUPERSEDED    PR 2108  locally-validated" <<<"$out" \
  && ok "stale locally-validated receipt marker FAILS at the moved head" \
  || bad "stale locally-validated label was not reported (rc=$rc): $out"

cat >"$tmp/local-current.json" <<JSON
[{"number":2108,"head":"$CUR","labels":["locally-validated"],
  "comments":["<!-- locally-validated-receipt commit=$OLD path=validation-receipts/rrnewton/hermit/$CUR/$(printf 'c%.0s' {1..64}).json sha256=$(printf 'd%.0s' {1..64}) -->"],
  "runs_at_head":0,"mergeable":"MERGEABLE"}]
JSON
out="$(run "$tmp/local-current.json" --gate --gate-on superseded)"; rc=$?
[[ $rc -eq 0 ]] && grep -q "every review/local-validation artifact is bound to the current head" <<<"$out" \
  && ok "current-head locally-validated receipt marker does not false-fire" \
  || bad "current locally-validated label was wrongly rejected (rc=$rc): $out"

out="$(run "$tmp/local-current.json" --pr 2108 --gate)"; rc=$?
[[ $rc -eq 1 ]] && grep -q "PR #2108 has zero review artifacts" <<<"$out" \
  && ok "locally-validated alone cannot satisfy the selected-PR approval set" \
  || bad "local validation cache bypassed the approval requirement (rc=$rc): $out"

# --- OWNER CRITICAL PROTOCOL ------------------------------------------------
# review-attest is the existing closed-enum critical classifier. Its exact-head
# comment makes criticality independent of the labels whose presence is being
# checked here.
cat >"$tmp/critical-missing-label.json" <<JSON
[{"number":2201,"head":"$CUR","labels":["passed-review-codex"],
  "comments":["REVIEW-DISPATCH-ATTESTATION\ncritical-class: hermit-syscall\nhead: $CUR\nAPPROVED-AT: codex $CUR"],
  "runs_at_head":4,"mergeable":"MERGEABLE"}]
JSON
out="$(run "$tmp/critical-missing-label.json" --pr 2201 --gate)"; rc=$?
[[ $rc -eq 1 ]] && grep -q "CRITICAL-LABEL-MISSING" <<<"$out" \
  && ok "critical attestation without post-facto-human-review REFUSES" \
  || bad "critical PR passed without post-facto-human-review (rc=$rc): $out"

cat >"$tmp/critical-one-family.json" <<JSON
[{"number":2202,"head":"$CUR","labels":["post-facto-human-review","passed-review-codex"],
  "comments":["REVIEW-DISPATCH-ATTESTATION\ncritical-class: hermit-syscall\nhead: $CUR\nAPPROVED-AT: codex $CUR"],
  "runs_at_head":4,"mergeable":"MERGEABLE"}]
JSON
out="$(run "$tmp/critical-one-family.json" --pr 2202 --gate)"; rc=$?
[[ $rc -eq 1 ]] && grep -q "CRITICAL-REVIEW-MISSING.*claude" <<<"$out" \
  && ok "critical PR with only codex exact-head approval REFUSES" \
  || bad "critical PR passed without claude approval (rc=$rc): $out"

cat >"$tmp/critical-complete.json" <<JSON
[{"number":2203,"head":"$CUR","labels":["post-facto-human-review","passed-review-codex","passed-review-claude"],
  "comments":["REVIEW-DISPATCH-ATTESTATION\ncritical-class: hermit-syscall\nhead: $CUR\nAPPROVED-AT: codex $CUR",
              "REVIEW-DISPATCH-ATTESTATION\ncritical-class: hermit-syscall\nhead: $CUR\nAPPROVED-AT: claude $CUR"],
  "runs_at_head":4,"mergeable":"MERGEABLE"}]
JSON
out="$(run "$tmp/critical-complete.json" --pr 2203 --gate)"; rc=$?
[[ $rc -eq 0 ]] && ok "critical PR with label plus codex+claude exact-head approvals PASSES" \
  || bad "complete critical protocol refused (rc=$rc): $out"

cat >"$tmp/ordinary-current-stale.json" <<JSON
[{"number":2205,"head":"$CUR","labels":["post-facto-human-review","passed-review-codex","passed-review-claude"],
  "comments":["APPROVED-AT: codex $CUR","APPROVED-AT: claude $OLD"],
  "runs_at_head":4,"mergeable":"MERGEABLE"}]
JSON
out="$(run "$tmp/ordinary-current-stale.json" --pr 2205 --gate)"; rc=$?
[[ $rc -eq 0 ]] && grep -q "required review approval binds to the current head" <<<"$out" \
  && ! grep -q "SUPERSEDED.*lane claude" <<<"$out" \
  && ok "ordinary PR accepts one current lane and does not require a stale optional lane" \
  || bad "ordinary PR required its stale optional lane (rc=$rc): $out"

cat >"$tmp/ordinary-current-undetermined.json" <<JSON
[{"number":2206,"head":"$CUR","labels":["post-facto-human-review","passed-review-codex","passed-review-claude"],
  "comments":["APPROVED-AT: codex $CUR"],
  "runs_at_head":4,"mergeable":"MERGEABLE"}]
JSON
out="$(run "$tmp/ordinary-current-undetermined.json" --pr 2206 --gate --json)"; rc=$?
[[ $rc -eq 0 ]] && grep -q '"review_requirement_satisfied": true' <<<"$out" \
  && grep -q '"authority_absent": 0' <<<"$out" \
  && grep -q '"noncurrent_optional_review_artifacts": 1' <<<"$out" \
  && ok "ordinary PR accepts one current lane with an undetermined optional lane" \
  || bad "ordinary PR required its undetermined optional lane (rc=$rc): $out"

cat >"$tmp/critical-current-stale.json" <<JSON
[{"number":2207,"head":"$CUR","labels":["post-facto-human-review","passed-review-codex","passed-review-claude"],
  "comments":["REVIEW-DISPATCH-ATTESTATION\ncritical-class: hermit-syscall\nhead: $CUR",
              "APPROVED-AT: codex $CUR","APPROVED-AT: claude $OLD"],
  "runs_at_head":4,"mergeable":"MERGEABLE"}]
JSON
out="$(run "$tmp/critical-current-stale.json" --pr 2207 --gate)"; rc=$?
[[ $rc -eq 1 ]] && grep -q "CRITICAL-REVIEW-MISSING.*claude" <<<"$out" \
  && ok "critical PR still requires the stale second lane to be renewed" \
  || bad "critical PR accepted one current lane (rc=$rc): $out"

cat >"$tmp/ordinary-current-refused.json" <<JSON
[{"number":2208,"head":"$CUR","labels":["post-facto-human-review","passed-review-codex"],
  "comments":["APPROVED-AT: codex $CUR","CHANGES-REQUESTED-AT: claude $CUR"],
  "runs_at_head":4,"mergeable":"MERGEABLE"}]
JSON
out="$(run "$tmp/ordinary-current-refused.json" --pr 2208 --gate)"; rc=$?
[[ $rc -eq 1 ]] && grep -q "REFUSED" <<<"$out" \
  && ok "an objection in an optional lane still blocks an ordinary PR" \
  || bad "ordinary PR ignored an unresolved optional-lane objection (rc=$rc): $out"

cat >"$tmp/post-facto-noncritical.json" <<JSON
[{"number":2204,"head":"$CUR","labels":["post-facto-human-review","passed-review-codex"],
  "comments":["APPROVED-AT: codex $CUR"],
  "runs_at_head":4,"mergeable":"MERGEABLE"}]
JSON
out="$(run "$tmp/post-facto-noncritical.json" --pr 2204 --gate)"; rc=$?
[[ $rc -eq 0 ]] && ok "post-facto label alone does not invent a critical classification" \
  || bad "noncritical post-facto trigger was incorrectly forced to dual review (rc=$rc): $out"

# --- MALFORMED SYNTAX IS NOT LOST AUTHORITY ---------------------------------
# One comment carries an incomplete marker assertion and the canonical exact-head
# binding. The syntax debt stays loud,
# but the authority gate must not turn it into a missing review.  Re-emitting
# APPROVED-AT cannot repair this case because the comment already emitted it.
cat >"$tmp/malformed-authoritative.json" <<JSON
[{"number":1847,"head":"$CUR","labels":["passed-review-claude"],
  "comments":["[adversarial-reviewer agent, claude-opus-5]\\nAPPROVED-AT: claude\\nAPPROVED-AT: claude $CUR"],
  "runs_at_head":12,"mergeable":"MERGEABLE"}]
JSON
out="$(run "$tmp/malformed-authoritative.json" --gate)"; rc=$?
[[ $rc -eq 0 ]] && ok "malformed-but-authoritative: authority gate PASSES (rc=0)" \
  || bad "malformed-but-authoritative: expected advisory rc=0, got $rc: $out"
grep -q "bound to current head: 1; authority absent: 0" <<<"$out" \
  && ok "malformed-but-authoritative remains 1/1 authoritative" \
  || bad "malformed-but-authoritative was reported as lost authority: $out"
grep -q '^malformed_state=present$' <<<"$out" \
  && grep -q 'malformed_summary=1 malformed source line(s) across 1 affected approval label(s) on 1 PR(s)' <<<"$out" \
  && ok "malformed source line reports through its distinct lower-severity channel" \
  || bad "malformed advisory channel/count missing: $out"
grep -q '^malformed_remedy=edit or remove the malformed historical source line;' <<<"$out" \
  && ok "malformed-only remedy names edit/removal, not ineffective re-emission" \
  || bad "malformed-only remedy is absent or wrong: $out"

# Real #2059 shape: a current-head APPROVED synonym with no canonical binding.
# Separation must never suppress this actual authority loss.
cat >"$tmp/malformed-lost.json" <<JSON
[{"number":2059,"head":"$CUR","labels":["passed-review-codex"],
  "comments":["[adversarial-reviewer agent, GPT-5]\\nAPPROVED — exact head $CUR."],
  "runs_at_head":9,"mergeable":"MERGEABLE"}]
JSON
out="$(run "$tmp/malformed-lost.json" --gate)"; rc=$?
[[ $rc -eq 1 ]] && ok "malformed-plus-lost-authority: gate STILL FAILS (rc=1)" \
  || bad "malformed-plus-lost-authority was over-suppressed (rc=$rc): $out"
grep -q "bound to current head: 0; authority absent: 1" <<<"$out" \
  && grep -q '^state=unbound$' <<<"$out" \
  && ok "genuinely lost binding remains a named 0/1 authority failure" \
  || bad "lost authority disappeared behind syntax reporting: $out"
grep -q '^malformed_state=present$' <<<"$out" \
  && ok "lost authority and malformed syntax are both reported, not summed" \
  || bad "combined case lost its independent malformed diagnostic: $out"

out="$(run "$tmp/malformed-authoritative.json" --gate --json)"; rc=$?
[[ $rc -eq 0 ]] \
  && grep -q '"authority_absent": 0' <<<"$out" \
  && grep -q '"malformed_source_lines": 1' <<<"$out" \
  && grep -q '"malformed_affected_labels": 1' <<<"$out" \
  && ok "JSON separates authority loss from malformed source-line/affected-label counts" \
  || bad "JSON conflates malformed syntax and authority: $out"

# --- THE COMPOSED STATE: superseded AND zero runs ---------------------------
cat >"$tmp/combined.json" <<JSON
[{"number":1864,"head":"$CUR","labels":["passed-review-codex"],
  "lane_shas":{"codex":["$OLD"]},"runs_at_head":0,"mergeable":"CONFLICTING"}]
JSON
out="$(run "$tmp/combined.json" --gate)"
grep -q "APPROVED + UNEVALUATED" <<<"$out" \
  && ok "approved-and-unevaluated is called out specifically, not just as one more stale label" \
  || bad "composed state not distinguished: $out"
grep -q "cached review/local-validation artifact(s) AND unevaluated: 1" <<<"$out" \
  && ok "reports the crossed count with its population" || bad "no crossed count: $out"

# The same zero cannot support that claim when GitHub Actions are disabled.
# Use an UNDETERMINED row so --gate-on superseded has no other reason to page;
# this proves the precondition changes the classification, not the row set.
cat >"$tmp/actions-disabled-zero.json" <<JSON
{"actions_enabled":false,"prs":[
  {"number":1865,"head":"$CUR","labels":["passed-review-codex"],
   "lane_shas":{},"runs_at_head":0,"mergeable":"MERGEABLE"}
]}
JSON
out="$(run "$tmp/actions-disabled-zero.json" --gate --gate-on superseded)"; rc=$?
[[ $rc -eq 0 ]] \
  && grep -q '^actions_enabled=false$' <<<"$out" \
  && grep -q '^runs_at_head_zero=expected while GitHub Actions are disabled;' <<<"$out" \
  && ! grep -q 'APPROVED + UNEVALUATED' <<<"$out" \
  && ok "Actions disabled: a genuine zero is explicit and does NOT escalate as unevaluated" \
  || bad "Actions-disabled zero was hidden or escalated (rc=$rc): $out"

# --- Nothing is deleted ------------------------------------------------------
# Match an actual deletion CALL, not the word. The first version of this
# assertion grepped for /DELETE/ and fired on the tool's own docstring, which
# explains that the existing merge-gate job deletes -- prose about the defect
# read as the defect. Keep it bound to call syntax.
grep -qE -- "--method[ =]DELETE|method=[\"']DELETE|\.delete\(" "$tool" \
  && bad "tool contains a label-deleting call; invalidation must not mean deletion" \
  || ok "tool performs NO label mutation -- invalidation without deletion"


# --- --gate-on selector: the standing condition must not page every tick ------
# UNDETERMINED is currently universal (no reviewer emits APPROVED-AT yet), so
# gating a 30-minute tick on it would fire forever and train people to ignore the
# check -- the same corrosion tick-hub.yaml already avoids by splitting
# validate_wall_regression (gates the event) from validate_wall_series_readiness
# (surfaces the standing blocker daily). These assert the split actually works,
# in BOTH directions: superseded still pages, unverifiable-only does not.
out="$(run "$tmp/unbound.json" --gate --gate-on superseded)"; rc=$?
[[ $rc -eq 0 ]] && ok "--gate-on superseded: an UNDETERMINED-only population does NOT page" \
  || bad "--gate-on superseded paged on undetermined only (rc=$rc)"
out="$(run "$tmp/superseded.json" --gate --gate-on superseded)"; rc=$?
[[ $rc -eq 1 ]] && ok "--gate-on superseded: a genuinely superseded approval STILL pages" \
  || bad "--gate-on superseded failed to page on superseded (rc=$rc)"
out="$(run "$tmp/combined.json" --gate --gate-on superseded)"; rc=$?
[[ $rc -eq 1 ]] \
  && grep -q '^actions_enabled=true$' <<<"$out" \
  && ok "Actions enabled: the combined approved+unevaluated state STILL pages" \
  || bad "--gate-on superseded missed the combined state (rc=$rc)"
# --- A LOCAL RECEIPT IS EVALUATION, AND BOTH DIRECTIONS MUST HOLD ------------
# The owner ruled local validate canonical and hosted Actions supplemental, and
# `actions/permissions` reports enabled=true on the live repo -- so the
# Actions-disabled guard above is UNREACHABLE in production and a zero run count
# was counting correct at-head approvals as failures. Measured on the live repo
# 2026-08-26: `--gate-on superseded` went 7-of-8 to 5-of-8 once a receipt
# counted, with every per-row STATE unchanged.
#
# ⚠️ THE FIXTURE MUST BE A *BAD* ROW BOUND AT THE HEAD, WHICH IS THE ONLY SHAPE
# THE RECEIPT CAN CHANGE. My first attempt used a BOUND_CURRENT row; it cleared
# whether or not a receipt existed, because `combined` is computed over `bad`
# only -- the cell passed for a reason unrelated to the change. This is the real
# hermit#2686 shape: an approval AT the head plus an un-discharged refusal at an
# older sha, which classifies REFUSED_STALE and lands in `bad`.
_refusal_row () {
  cat <<JSON
  {"number":1866,"head":"$CUR","labels":["passed-review-claude"],
   "comments":["APPROVED-AT: claude $CUR","CHANGES-REQUESTED-AT: claude $OLD"],
   "runs_at_head":0,"mergeable":"MERGEABLE"}
JSON
}

# DIRECTION 1: with a receipt for that head, it leaves the unevaluated set.
cat >"$tmp/receipted-at-head.json" <<JSON
{"actions_enabled":true,"evaluated_heads":["$CUR"],"prs":[
$(_refusal_row)
]}
JSON
out="$(run "$tmp/receipted-at-head.json" --gate --gate-on superseded)"; rc=$?
[[ $rc -eq 0 ]] \
  && ! grep -q 'APPROVED + UNEVALUATED' <<<"$out" \
  && ok "an at-head binding whose head has a LOCAL RECEIPT stops paging" \
  || bad "a receipted at-head binding still paged (rc=$rc): $out"

# ⚠️ THE CONTROL. The identical fixture with NO receipt must still page and must
# still carry the marker -- otherwise the cell above proves nothing.
cat >"$tmp/unreceipted-at-head.json" <<JSON
{"actions_enabled":true,"evaluated_heads":[],"prs":[
$(_refusal_row)
]}
JSON
out="$(run "$tmp/unreceipted-at-head.json" --gate --gate-on superseded)"; rc=$?
[[ $rc -eq 1 ]] \
  && grep -q 'APPROVED + UNEVALUATED' <<<"$out" \
  && ok "the same head with NO receipt is still unevaluated and still pages" \
  || bad "the receipt set is not what cleared it (rc=$rc): $out"

# ⚠️ AND THE REMEDY MUST NAME THE RIGHT ACTION. Telling a reviewer whose binding
# is already AT the head to "re-review the exact head" is the sentence that cost
# four re-reviews on #2686, #2683, #2674 and #2602.
grep -q 'validate the exact head' <<<"$out" \
  && ok "an unevaluated at-head binding is told to VALIDATE, not to re-review" \
  || bad "the remedy still sends an at-head binding back to re-review: $out"

# DIRECTION 2: a GENUINELY STALE binding must still REFUSE even when its head
# carries a receipt. A receipt says the head was evaluated; it says nothing
# about which sha the reviewer looked at.
cat >"$tmp/receipted-superseded.json" <<JSON
{"actions_enabled":true,"evaluated_heads":["$CUR"],"prs":[
  {"number":1867,"head":"$CUR","labels":["passed-review-codex"],
   "lane_shas":{"codex":["$OLD"]},"runs_at_head":0,"mergeable":"MERGEABLE"}
]}
JSON
out="$(run "$tmp/receipted-superseded.json" --gate --gate-on superseded)"; rc=$?
[[ $rc -eq 1 ]] \
  && ok "a genuinely stale binding STILL refuses, receipt or no receipt" \
  || bad "a receipt cleared a superseded binding (rc=$rc): $out"

out="$(run "$tmp/unbound.json" --gate --gate-on unverifiable)"; rc=$?
[[ $rc -eq 1 ]] && ok "--gate-on unverifiable: the standing condition pages on its own daily entry" \
  || bad "--gate-on unverifiable did not page (rc=$rc)"
out="$(run "$tmp/unbound.json" --gate)"; rc=$?
[[ $rc -eq 1 ]] && ok "default --gate is UNCHANGED: still pages on anything not BOUND_CURRENT" \
  || bad "default gate contract regressed (rc=$rc)"


# --- A REFUSED LANE IS NOT AN UNREVIEWED LANE ---------------------------------
# Both were UNDETERMINED, so a head two reviewers had BLOCKED read exactly like a
# head nobody had opened -- and the wrong response follows from the wrong reading:
# an absence invites a fresh reviewer. Measured 2026-08-26 on hermit#2655 (two
# lanes, two refusals, two blocking findings, both lanes UNDETERMINED).
# --- A REFUSAL WHOSE HEAD HAS MOVED IS STILL A REFUSAL, AND SAYS SO ------------
# ⚠️ REFUSED_STALE BLOCKS. It reports that the head moved after the refusal was
# written, not that the refusal lapsed. Measured 2026-08-26 on hermit#2680: a
# refusal at 099290c1 correctly suppressed the lane at 642a2990 after the finding
# had been fixed and pushed -- the right answer, unreadable, so the head was
# re-dispatched as if freshly blocked.
cat >"$tmp/refused-stale.json" <<JSON
[{"number":2680,"head":"$CUR","labels":["passed-review-claude"],
  "lane_shas":{},"lane_refused":{"claude":["$OLD"]},
  "runs_at_head":4,"mergeable":"MERGEABLE"}]
JSON
out="$(run "$tmp/refused-stale.json" --gate)"; rc=$?
grep -q "REFUSED_STALE" <<<"$out" \
  && ok "a refusal at a superseded sha reports REFUSED_STALE" \
  || bad "stale refusal did not report REFUSED_STALE: $out"
[[ $rc -eq 1 ]] \
  && ok "REFUSED_STALE STILL BLOCKS -- default --gate refuses (no one-commit bypass)" \
  || bad "a stale refusal became landable (rc=$rc) -- that is the outrun bypass"
out="$(run "$tmp/refused-stale.json" --gate --gate-on unverifiable)"; rc=$?
[[ $rc -eq 1 ]] \
  && ok "--gate-on unverifiable STILL pages on REFUSED_STALE" \
  || bad "REFUSED_STALE silently stopped paging (rc=$rc)"

cat >"$tmp/refused.json" <<JSON
[{"number":2655,"head":"$CUR","labels":["passed-review-claude"],
  "lane_shas":{},"lane_refused":{"claude":["$CUR"]},
  "runs_at_head":4,"mergeable":"MERGEABLE"}]
JSON
out="$(run "$tmp/refused.json" --gate)"; rc=$?
grep -q "REFUSED       PR 2655" <<<"$out" \
  && ok "a refused lane reports REFUSED, distinctly from UNDETERMINED" \
  || bad "refused lane did not report REFUSED: $out"
# THE CONTROL THAT MAKES THE ABOVE MEAN ANYTHING: the unreviewed fixture must
# still say UNDETERMINED. Without it, renaming every state to REFUSED would pass.
out_unbound="$(run "$tmp/unbound.json" --gate)"
grep -q "UNDETERMINED  PR 999" <<<"$out_unbound" \
  && ok "control: a lane nobody bound is STILL UNDETERMINED, not REFUSED" \
  || bad "control failed -- unreviewed lane no longer reports UNDETERMINED: $out_unbound"

# ⚠️ THE PAGING HALF. Splitting REFUSED out of UNDETERMINED silently removes
# refused heads from this selector unless it lists both -- rc 1 -> 0, a live
# block vanishing from the report whose job is to surface it. tick-hub.yaml runs
# exactly this selector against the live repo.
out="$(run "$tmp/refused.json" --gate --gate-on unverifiable)"; rc=$?
[[ $rc -eq 1 ]] \
  && ok "--gate-on unverifiable STILL pages on a refused lane (the paging half)" \
  || bad "REFUSED silently stopped paging on --gate-on unverifiable (rc=$rc)"
out="$(run "$tmp/refused.json" --gate)"; rc=$?
[[ $rc -eq 1 ]] && ok "default --gate: REFUSED is not BOUND_CURRENT, so it still refuses" \
  || bad "a refused lane became landable (rc=$rc)"

# A refusal must not outlive a later approval: REFUSED is the state only when the
# refusal was the lane's LAST word. Same fixture shape, plus a current binding.
cat >"$tmp/refused-then-approved.json" <<JSON
[{"number":2656,"head":"$CUR","labels":["passed-review-claude"],
  "lane_shas":{"claude":["$CUR"]},"lane_refused":{"claude":["$OLD"]},
  "runs_at_head":4,"mergeable":"MERGEABLE"}]
JSON
out="$(run "$tmp/refused-then-approved.json" --gate)"; rc=$?
[[ $rc -eq 0 ]] && ! grep -q "REFUSED" <<<"$out" \
  && ok "a refusal followed by a current approval is BOUND_CURRENT, not REFUSED" \
  || bad "REFUSED over-fired past a later approval (rc=$rc): $out"

# An UNLABELLED refusal must produce a row at all. `if not shas: continue` in the
# comment-row builder dropped it entirely -- not merely mislabelled, ABSENT from
# the census, which is #2655's real shape (zero labels, two refusals, zero rows).
cat >"$tmp/refused-unlabelled.json" <<JSON
[{"number":2657,"head":"$CUR","labels":[],
  "lane_shas":{},"lane_refused":{"codex":["$CUR"]},
  "runs_at_head":2,"mergeable":"MERGEABLE"}]
JSON
out="$(run "$tmp/refused-unlabelled.json" --gate)"
grep -q "REFUSED" <<<"$out" \
  && ok "an unlabelled refusal appears in the census instead of vanishing" \
  || bad "unlabelled refusal produced no row: $out"


# --- A MALFORMED REFUSAL MUST REFUSE; A MALFORMED APPROVAL MUST NOT BIND -------
# `CHANGES-REQUESTED:` without `-AT` -- bare line, column zero, correct lane,
# correct 40-hex, one token wrong -- was flagged malformed and NOT HONOURED in
# BOTH landers. changes-requested-gate returned CLEAR; approval_binding left
# state=ok with the approval standing. hermit#2611 merged 24 minutes after a
# block written that way.
#
# THE TWO ERRORS ARE NOT SYMMETRIC and both halves are asserted, because fixing
# only the refusal half by loosening BOTH matchers is the obvious wrong edit.
crg="$here/../../bin/changes-requested-gate"

cat >"$tmp/suffixless.json" <<JSON
[{"number":9101,"head":"$CUR","labels":["passed-review-claude"],
  "comments":["APPROVED-AT: claude $CUR","CHANGES-REQUESTED: claude $CUR"],
  "runs_at_head":3,"mergeable":"MERGEABLE"}]
JSON
out="$(run "$tmp/suffixless.json" --pr 9101 --gate)"; rc=$?
[[ $rc -eq 1 ]] && grep -q "^state=unbound" <<<"$out" \
  && ok "approval_binding: a suffix-less refusal CLEARS the approval (state=unbound)" \
  || bad "approval_binding: suffix-less refusal did not clear the approval: $out"

cat >"$tmp/canonrefuse.json" <<JSON
[{"number":9102,"head":"$CUR","labels":["passed-review-claude"],
  "comments":["APPROVED-AT: claude $CUR","CHANGES-REQUESTED-AT: claude $CUR"],
  "runs_at_head":3,"mergeable":"MERGEABLE"}]
JSON
out="$(run "$tmp/canonrefuse.json" --pr 9102 --gate)"
grep -q "^state=unbound" <<<"$out" \
  && ok "approval_binding: canonical refusal still clears it (control)" \
  || bad "approval_binding control broke: $out"

cat >"$tmp/cleanapp.json" <<JSON
[{"number":9103,"head":"$CUR","labels":["passed-review-claude"],
  "comments":["APPROVED-AT: claude $CUR"],"runs_at_head":3,"mergeable":"MERGEABLE"}]
JSON
out="$(run "$tmp/cleanapp.json" --pr 9103 --gate)"; rc=$?
[[ $rc -eq 0 ]] && ok "approval_binding: a clean head still merges (the negative control)" \
  || bad "clean head no longer passes -- the fix over-blocked: $out"

cat >"$tmp/malapp.json" <<JSON
[{"number":9104,"head":"$CUR","labels":["passed-review-claude"],
  "comments":["APPROVED: claude $CUR"],"runs_at_head":3,"mergeable":"MERGEABLE"}]
JSON
out="$(run "$tmp/malapp.json" --pr 9104 --gate)"
grep -q "^state=unbound" <<<"$out" \
  && ok "approval_binding: a suffix-less APPROVAL still does NOT bind -- the asymmetry" \
  || bad "a malformed approval bound; loosening went the wrong way too: $out"

# ---- the other lander. A fix on one is the seventh workaround. ----
# Convert the compact body arrays below into complete GitHub-shaped records.
# The gate deliberately refuses body strings now: they carry no stable id,
# authoritative version time, review surface, or GitHub actor permission.
gate_fixture() {
  python3 -c '
import json, sys
bodies = json.loads(sys.argv[2])
records = [
    {
        "id": 990000 + position,
        "body": body,
        "user": {"login": "fixture-reviewer"},
        "_review_surface": "issue-comment",
        "_current_review_actor_authorized": True,
        "_current_review_actor_login": "fixture-reviewer",
        "_current_review_actor_permission": "write",
        "created_at": f"2026-09-05T00:00:{position:02d}Z",
        "updated_at": f"2026-09-05T00:00:{position:02d}Z",
    }
    for position, body in enumerate(bodies, 1)
]
with open(sys.argv[1], "w", encoding="utf-8") as output:
    json.dump(records, output)
' "$tmp/g.json" "$1"
}
gate() {
  gate_fixture "$1"
  python3 "$crg" --repo r/r --pr 1 --head "$CUR" \
    --from-json "$tmp/g.json" >/dev/null 2>&1
}
printf '%s' "[\"APPROVED-AT: claude $CUR\"]" >"$tmp/g-strings.json"
out="$(python3 "$crg" --repo r/r --pr 1 --head "$CUR" \
  --from-json "$tmp/g-strings.json" 2>&1)"; rc=$?
[[ $rc -eq 2 ]] && grep -q "body-string input is incomplete" <<<"$out" \
  && ok "changes-requested-gate: body-string input is explicitly INDETERMINATE" \
  || bad "body-string input did not fail closed with its missing evidence: $out"
gate "[\"CHANGES-REQUESTED: claude $CUR\"]" \
  && bad "changes-requested-gate: suffix-less refusal still CLEARS (fails open)" \
  || ok "changes-requested-gate: a suffix-less refusal BLOCKS"
gate "[\"CHANGES-REQUESTED-AT: claude $CUR\"]" \
  && bad "changes-requested-gate: canonical refusal stopped blocking" \
  || ok "changes-requested-gate: canonical refusal still blocks (control)"
gate "[\"APPROVED-AT: claude $CUR\"]" \
  && ok "changes-requested-gate: a clean head still merges" \
  || bad "changes-requested-gate: clean head now blocked -- over-blocking"
gate "[\"[hermit2, hermit-007, unresolved, devbig014, role=reviewer]\\nCHANGES-REQUESTED: claude $CUR\",\"[hermit2, hermit-099, unresolved, devbig014, role=reviewer]\\nCHANGES-REQUESTED-WITHDRAWN-AT: claude $CUR\"]" \
  && ok "changes-requested-gate: a later current-head withdrawal clears without fleet-name equality" \
  || bad "a later current-head withdrawal did not clear a suffix-less refusal"
gate "[\"CHANGES-REQUESTED: claude $CUR\",\"CHANGES-REQUESTED-WITHDRAWN-AT: claude $CUR\"]" \
  && ok "missing fleet names do not prevent a canonical current-head withdrawal" \
  || bad "missing fleet names prevented a canonical current-head withdrawal"


# --- THE ASSERTION THE MUTATION CONTROL HAS ALWAYS WAITED FOR ------------------
# ⚠️ IT WAS NEVER WRITTEN. The control below greps the mutant run for
#   `FAIL  current CHANGES-REQUESTED-AT revokes an earlier exact-head approval`
# and that string appeared exactly ONCE in this file -- inside the control's own
# grep. So the control was correct and honest: it reported that the suite could
# not detect a broken revocation, and it reported that for months.
#
# What the suite did assert was that a refusal is REPORTED. Nothing asserted that
# a refusal REVOKES. Those are different properties and only the second is the
# one `found.clear()` implements, which is why deleting `found.clear()` changed no
# assertion.
#
# ⚠️ THE FIX IS HERE, NOT IN THE CONTROL. A control tuned until it goes red proves
# the control; an assertion that dies under the mutant proves the test. Verified
# both ways below: this fails under the mutant and passes without it.
cat >"$tmp/revokes.json" <<JSON
[{"number":9201,"head":"$CUR","labels":["passed-review-claude"],
  "comments":["APPROVED-AT: claude $CUR","CHANGES-REQUESTED-AT: claude $CUR"],
  "runs_at_head":3,"mergeable":"MERGEABLE"}]
JSON
out="$(run "$tmp/revokes.json" --pr 9201 --gate --json)"
if grep -q '"authoritative": 0' <<<"$out" && ! grep -q '"authoritative": 1' <<<"$out"; then
  ok "current CHANGES-REQUESTED-AT revokes an earlier exact-head approval"
else
  bad "current CHANGES-REQUESTED-AT revokes an earlier exact-head approval"
fi

# --- THE SEVENTH DECAY MODE: AN APPROVAL FROM ANOTHER ISSUER ------------------
# ⚠️ MEASURED ON hermit#2678, NOT IMAGINED. One unmoved head, claude lane:
#     01:52:02Z  CHANGES-REQUESTED-AT  agent(hermit-007)
#     01:54:39Z  APPROVED-AT           a different, unsigned reviewer
# The lane read BOUND_CURRENT and carried `passed-review-claude` while
# hermit-007's blocking finding stood unanswered. The approving agent had not
# seen the refusal -- their own comment says "zero comments when I started" --
# so nobody overrode anything deliberately. That is exactly why the grammar has
# to hold this and reviewer attention cannot.
#
# A same-head approval does not withdraw a standing objection. Fleet-name
# metadata cannot change that conflict; a canonical withdrawal or exact RETIRES
# record is required.
cat >"$tmp/self-reversal.json" <<JSON
[{"number":9210,"head":"$CUR","labels":["passed-review-claude"],
  "comments":["[hermit2, hermit-007, unresolved, devbig014, role=reviewer]\nCHANGES-REQUESTED-AT: claude $CUR",
              "[hermit2, hermit-007, unresolved, devbig014, role=reviewer]\nAPPROVED-AT: claude $CUR"],
  "runs_at_head":3,"mergeable":"MERGEABLE"}]
JSON
out="$(run "$tmp/self-reversal.json" --pr 9210 --gate --json)"
if grep -q '"authoritative": 0' <<<"$out"; then
  ok "a same-head approval does not discharge an earlier refusal"
else
  bad "a same-head approval incorrectly discharged an earlier refusal"
fi

cat >"$tmp/other-issuer.json" <<JSON
[{"number":9211,"head":"$CUR","labels":["passed-review-claude"],
  "comments":["[hermit2, hermit-007, unresolved, devbig014, role=reviewer]\nCHANGES-REQUESTED-AT: claude $CUR",
              "[hermit2, hermit-005, unresolved, devbig014, role=reviewer]\nAPPROVED-AT: claude $CUR"],
  "runs_at_head":3,"mergeable":"MERGEABLE"}]
JSON
out="$(run "$tmp/other-issuer.json" --pr 9211 --gate --json)"
if grep -q '"authoritative": 0' <<<"$out"; then
  ok "different fleet-name metadata does not change the same-head conflict"
else
  bad "fleet-name metadata changed the same-head conflict"
fi

cat >"$tmp/unsigned-approval.json" <<JSON
[{"number":9212,"head":"$CUR","labels":["passed-review-claude"],
  "comments":["[hermit2, hermit-007, unresolved, devbig014, role=reviewer]\nCHANGES-REQUESTED-AT: claude $CUR",
              "APPROVED-AT: claude $CUR"],
  "runs_at_head":3,"mergeable":"MERGEABLE"}]
JSON
out="$(run "$tmp/unsigned-approval.json" --pr 9212 --gate --json)"
if grep -q '"authoritative": 0' <<<"$out"; then
  ok "missing fleet-name metadata does not change the same-head conflict"
else
  bad "missing fleet-name metadata changed the same-head conflict"
fi

# The grammar's own escape hatch must still work, or a refusal becomes a
# permanent park with no door. Fleet-name metadata does not control this path.
cat >"$tmp/withdrawn.json" <<JSON
[{"number":9213,"head":"$CUR","labels":["passed-review-claude"],
  "comments":["[hermit2, hermit-007, unresolved, devbig014, role=reviewer]\nCHANGES-REQUESTED-AT: claude $CUR",
              "[hermit2, hermit-007, unresolved, devbig014, role=reviewer]\nCHANGES-REQUESTED-WITHDRAWN-AT: claude $CUR",
              "[hermit2, hermit-005, unresolved, devbig014, role=reviewer]\nAPPROVED-AT: claude $CUR"],
  "runs_at_head":3,"mergeable":"MERGEABLE"}]
JSON
out="$(run "$tmp/withdrawn.json" --pr 9213 --gate --json)"
if grep -q '"authoritative": 1' <<<"$out"; then
  ok "a later current-head CHANGES-REQUESTED-WITHDRAWN-AT reopens the lane"
else
  bad "a later current-head withdrawal did not clear the earlier objection"
fi

# The negative control for it. Without this, the assertion above would also pass
# on a build where NOTHING is ever authoritative -- revocation would be
# indistinguishable from total breakage.
cat >"$tmp/norevoke.json" <<JSON
[{"number":9202,"head":"$CUR","labels":["passed-review-claude"],
  "comments":["APPROVED-AT: claude $CUR"],"runs_at_head":3,"mergeable":"MERGEABLE"}]
JSON
out="$(run "$tmp/norevoke.json" --pr 9202 --gate --json)"
grep -q '"authoritative": 1' <<<"$out" \
  && ok "an approval with no refusal REMAINS authoritative (negative control)" \
  || bad "approvals are not authoritative even unrefused; the assertion above proves nothing: $out"


# --- PROSE IS NOT A VERDICT, AND COUNTING IT STRANDED A HEAD ------------------
# The near-miss net is case-insensitive and allows a SPACE, so an ordinary
# English sentence opening a review -- "Changes requested, on the deleted value
# pin alone." -- matched at position 0 and was accepted with NO lane and NO sha.
# A marker with no readable sha cannot bind, so it became an UNCLEARABLE refusal:
# rc=2 forever, and pushing does not help because the sentence never named a sha
# to stop naming. Measured on hermit#2625 at 6457ba0acfb4, whose claude lane was
# bound at the exact head and still could not land.
#
# ⚠️ THE THREE OUTCOMES MUST STAY DISTINCT, so all three are asserted here.
gatep() { gate "$1"; }

gatep "[\"Changes requested, on the deleted value pin alone. Everything else here is right.\"]" \
  && ok "prose opening a review counts as NOTHING (rc=0)" \
  || bad "an English sentence is still treated as a refusal -- the unclearable state"

gatep "[\"CHANGES-REQUESTED-AT: claude $CUR\"]" \
  && bad "a well-formed refusal stopped blocking -- the fix went too far" \
  || ok "a well-formed refusal still BLOCKS (control)"

gatep "[\"CHANGES-REQUESTED: claude $CUR\"]" \
  && bad "a malformed refusal carrying lane AND sha stopped blocking" \
  || ok "a malformed refusal carrying lane+sha still BLOCKS (control)"

# ⚠️ THE DISCRIMINATOR ITSELF: a near-miss with a lane but no sha is somebody
# ATTEMPTING a verdict and getting it wrong, which must stay INDETERMINATE rather
# than being swept into "prose". If this ever returns 0 the fix has become a way
# to ignore real refusals.
gatep "[\"CHANGES-REQUESTED: claude please fix the thing\"]"
[[ $? -eq 2 ]] \
  && ok "a near-miss with a lane but no sha is still INDETERMINATE, not ignored" \
  || bad "a lane-bearing refusal attempt was swallowed as prose"

# The withdrawal path is deliberately untouched: widening it would let an
# accidental sentence CLEAR a real refusal, which is the dangerous direction.
gatep "[\"CHANGES-REQUESTED-AT: claude $CUR\",\"Withdrawing my concerns, this looks fine now.\"]" \
  && bad "a prose sentence CLEARED a real refusal -- withdrawal was loosened" \
  || ok "prose does NOT clear a real refusal (withdrawal untouched)"


# --- the binding GRAMMAR, including the rejection trap -----------------------
if python3 "$here/test_approval_binding_grammar.py" >"$tmp/grammar.out" 2>&1; then
  ok "binding grammar suite: $(grep -c '^  PASS' "$tmp/grammar.out") assertions pass"
else
  bad "binding grammar suite FAILED"; sed 's/^/        /' "$tmp/grammar.out" | grep FAIL | head -4
fi

# --- MUTATION CONTROL: removing either load-bearing guard breaks this suite --
# Run this complete bracket against a temporary copy whose single-PR empty-set
# predicate is disabled.  Nonzero alone is insufficient: name the exact
# #2132 assertion so an unrelated mutant failure cannot masquerade as proof.
if [[ -z "${APPROVAL_BINDING_MUTATION_CHILD:-}" ]]; then
  mutant="$tmp/approval_binding-without-zero-guard.py"
  sed \
    's/approval_set_missing = a.pr is not None and not approval_rows/approval_set_missing = False  # MUTANT: zero-approval guard removed/' \
    "$tool" >"$mutant"
  if ! grep -q 'MUTANT: zero-approval guard removed' "$mutant"; then
    bad "mutation control could not locate the zero-approval guard"
  else
    APPROVAL_BINDING_TOOL="$mutant" APPROVAL_BINDING_MUTATION_CHILD=1 \
      bash "$0" >"$tmp/mutant-suite.out" 2>&1
    mutant_rc=$?
    if [[ $mutant_rc -ne 0 ]] \
      && grep -q 'FAIL  zero approvals: expected rc=1, got 0' "$tmp/mutant-suite.out"; then
      ok "mutation control: full suite FAILS on #2132 when zero-approval guard is removed"
    else
      bad "mutation control did not fail on the #2132 guard (rc=$mutant_rc)"
    fi
  fi
fi

# ⚠️ MUTATION CONTROL FOR THE PAGING HALF. Drop REFUSED from the `unverifiable`
# selector -- the one and only place in the tool that enumerates a state
# POSITIVELY, and therefore the one place a new state can silently NARROW what
# pages. Every other reader asks `== BOUND_CURRENT` and fails closed. Without
# this control the omission is invisible: the gate just returns 0 and reports
# nothing wrong, which is precisely how a blocked head disappears from the
# report meant to surface it.
if [[ -z "${APPROVAL_BINDING_MUTATION_CHILD:-}" ]]; then
  mutant="$tmp/approval_binding-refused-not-paged.py"
  sed 's/r\["state"\] in (UNDETERMINED, REFUSED, REFUSED_STALE)/r["state"] == UNDETERMINED  # MUTANT: refused no longer pages/' \
    "$tool" >"$mutant"
  if ! grep -q 'MUTANT: refused no longer pages' "$mutant"; then
    bad "paging mutation control could not locate the unverifiable selector"
  else
    APPROVAL_BINDING_TOOL="$mutant" APPROVAL_BINDING_MUTATION_CHILD=1 \
      bash "$0" >"$tmp/paging-mutant-suite.out" 2>&1
    mutant_rc=$?
    if [[ $mutant_rc -ne 0 ]] \
      && grep -q 'FAIL  REFUSED silently stopped paging on --gate-on unverifiable' \
        "$tmp/paging-mutant-suite.out"; then
      ok "mutation control: suite FAILS when REFUSED stops paging on --gate-on unverifiable"
    else
      bad "paging mutation control did not fail (rc=$mutant_rc)"
    fi
  fi
fi

# ⚠️ MUTATION CONTROL FOR THE BOUNDARY THIS CHANGE DELIBERATELY DID NOT MOVE.
# The tempting "fix" for a stale refusal is to let it lapse -- return UNDETERMINED
# once the head moves. That is a one-commit bypass: push anything and an
# unanswered objection evaporates. Make that edit and the suite must fail, or the
# distinction between "reported as stale" and "stopped applying" is unguarded.
if [[ -z "${APPROVAL_BINDING_MUTATION_CHILD:-}" ]]; then
  mutant="$tmp/approval_binding-stale-lapses.py"
  sed 's/            return REFUSED_STALE if _refusal_is_stale(head, refused_at) else REFUSED/            return UNDETERMINED if _refusal_is_stale(head, refused_at) else REFUSED  # MUTANT: stale refusal lapses/' \
    "$tool" >"$mutant"
  if ! grep -q 'MUTANT: stale refusal lapses' "$mutant"; then
    bad "stale-lapse mutation control could not locate the classifier branch"
  else
    APPROVAL_BINDING_TOOL="$mutant" APPROVAL_BINDING_MUTATION_CHILD=1 \
      bash "$0" >"$tmp/stale-mutant-suite.out" 2>&1
    mutant_rc=$?
    if [[ $mutant_rc -ne 0 ]] \
      && grep -q 'FAIL  stale refusal did not report REFUSED_STALE' "$tmp/stale-mutant-suite.out"; then
      ok "mutation control: suite FAILS when a stale refusal is allowed to lapse"
    else
      bad "stale-lapse mutation control did not fail (rc=$mutant_rc)"
    fi
  fi
fi

# Disable the current fleet rejection grammar. The suite must fail specifically
# because an approval followed by CHANGES-REQUESTED-AT remains authoritative.
if [[ -z "${APPROVAL_BINDING_MUTATION_CHILD:-}" ]]; then
  mutant="$tmp/approval_binding-without-current-rejection.py"
  # ⚠️ MUTATES BOTH MECHANISMS, AND IT USED TO MUTATE ONE. Revocation is now
  # defence in depth: `found.clear()` drops the accumulated approvals, and the
  # issuer-aware `unanswered` guard keeps the lane REFUSED even after a later
  # approval repopulates them. Removing only the first leaves the second holding
  # the line, so the suite stayed GREEN and this control reported that it could
  # no longer detect a broken revocation -- correctly. Widening it keeps the
  # control honest about what it proves: that revocation as a WHOLE is
  # load-bearing. The narrower property -- that the issuer-aware half is itself
  # load-bearing -- gets its own control below, so neither hides the other.
  sed -e 's/found.clear()/pass  # MUTANT: current rejection no longer revokes approval/' \
      -e 's/^    if unanswered:$/    if False:  # MUTANT: unanswered refusal no longer blocks/' \
    "$tool" >"$mutant"
  if ! grep -q 'MUTANT: current rejection no longer revokes approval' "$mutant"; then
    bad "rejection mutation control could not locate the revocation"
  else
    APPROVAL_BINDING_TOOL="$mutant" APPROVAL_BINDING_MUTATION_CHILD=1 \
      bash "$0" >"$tmp/rejection-mutant-suite.out" 2>&1
    mutant_rc=$?
    if [[ $mutant_rc -ne 0 ]] \
      && grep -q 'FAIL  current CHANGES-REQUESTED-AT revokes an earlier exact-head approval' \
        "$tmp/rejection-mutant-suite.out"; then
      ok "mutation control: suite FAILS on the exact changes-requested case when revocation is removed"
    else
      bad "rejection mutation control did not fail for CHANGES-REQUESTED-AT (rc=$mutant_rc)"
    fi
  fi
fi

# Disable critical classification. The suite must identify the missing
# post-facto label and second-family approval as the lost refusals.
if [[ -z "${APPROVAL_BINDING_MUTATION_CHILD:-}" ]]; then
  mutant="$tmp/approval_binding-without-critical-policy.py"
  sed 's/critical_required = bool(selected_critical_attestations)/critical_required = False  # MUTANT: critical policy disabled/' \
    "$tool" >"$mutant"
  if ! grep -q 'MUTANT: critical policy disabled' "$mutant"; then
    bad "critical mutation control could not locate the classifier"
  else
    APPROVAL_BINDING_TOOL="$mutant" APPROVAL_BINDING_MUTATION_CHILD=1 \
      bash "$0" >"$tmp/critical-mutant-suite.out" 2>&1
    mutant_rc=$?
    if [[ $mutant_rc -ne 0 ]] \
      && grep -q 'FAIL  critical PR passed without post-facto-human-review' \
        "$tmp/critical-mutant-suite.out" \
      && grep -q 'FAIL  critical PR passed without claude approval' \
        "$tmp/critical-mutant-suite.out"; then
      ok "mutation control: suite FAILS on both owner critical requirements when classification is removed"
    else
      bad "critical mutation control did not expose both missing owner checks (rc=$mutant_rc)"
    fi
  fi
fi

# Re-introduce the exact conflation fixed here: malformed syntax alone makes
# the authority gate fail.  The full suite must name the #1847-shaped break,
# proving the new positive/advisory half is not inert.
if [[ -z "${APPROVAL_BINDING_MUTATION_CHILD:-}" ]]; then
  mutant="$tmp/approval_binding-reconflate-malformed.py"
  sed \
    's/if a.gate and (gating or reconciled):/if a.gate and (gating or reconciled or malformed):  # MUTANT: syntax debt reconflated with authority/' \
    "$tool" >"$mutant"
  if ! grep -q 'MUTANT: syntax debt reconflated with authority' "$mutant"; then
    bad "separation mutation control could not locate the authority gate"
  else
    APPROVAL_BINDING_TOOL="$mutant" APPROVAL_BINDING_MUTATION_CHILD=1 \
      bash "$0" >"$tmp/separation-mutant-suite.out" 2>&1
    mutant_rc=$?
    if [[ $mutant_rc -ne 0 ]] \
      && grep -q 'FAIL  malformed-but-authoritative: expected advisory rc=0, got 1' \
        "$tmp/separation-mutant-suite.out"; then
      ok "mutation control: suite FAILS when malformed syntax is reconflated with lost authority"
    else
      bad "separation mutation control did not fail on the #1847-shaped case (rc=$mutant_rc)"
    fi
  fi
fi


# --- LABELS ARE CACHES: exact-head comments remain authority ------------------
# Literal authority shape from Hermit #2956 before its cache label was applied:
# issuecomment-5552507459 is a canonical exact-head approval and the PR had no
# passed-review-* label. The comment, not the label, must satisfy the gate.
PR2956_HEAD=88bd94fdfc841eb0512b81707e6e2e8f56d589a6
cat >"$tmp/pr2956-unlabeled.json" <<JSON
[{"number":2956,"head":"$PR2956_HEAD","labels":[],
  "comments":["[hermit2, review-cpuid, unresolved, devbig014, role=reviewer]\nAPPROVED-AT: claude $PR2956_HEAD"],
  "runs_at_head":0,"mergeable":"MERGEABLE"}]
JSON
out="$(run "$tmp/pr2956-unlabeled.json" --pr 2956 --gate --json)"; rc=$?
[[ $rc -eq 0 ]] \
  && grep -q '"approvals": 1' <<<"$out" \
  && grep -q '"authoritative": 1' <<<"$out" \
  && grep -q '"approval_set_missing": false' <<<"$out" \
  && ok "#2956 shape: an unlabeled exact-head approval is authoritative" \
  || bad "#2956 shape lost comment authority (rc=$rc): $out"

# Literal marker chronology from Hermit #2950 before label reconciliation. The
# old refusal is followed by a canonical lane decision and then a fresh exact-
# head approval. No fleet-name equality participates in the result.
PR2950_HEAD=58bf4f5c17880395400ab3cb6482d0de2ee03a84
cat >"$tmp/pr2950-unlabeled.json" <<JSON
[{"number":2950,"head":"$PR2950_HEAD","labels":[],
  "comments":["[hermit2, review-cpuid, unresolved, devbig014, role=reviewer]\nCHANGES-REQUESTED-AT: claude c3a85da26110fd8dfc31433c8d4109726127703b",
              "[hermit2, review-cpuid, unresolved, devbig014, role=reviewer]\nCHANGES-REQUESTED-WITHDRAWN-AT: claude 2a18b7bea06cf66f5cf4a973466afa506c0bd2a4\nAPPROVED-AT: claude 2a18b7bea06cf66f5cf4a973466afa506c0bd2a4",
              "[hermit2, another-name, unresolved, devbig030, role=reviewer]\nAPPROVED-AT: claude $PR2950_HEAD"],
  "runs_at_head":0,"mergeable":"MERGEABLE"}]
JSON
out="$(run "$tmp/pr2950-unlabeled.json" --pr 2950 --gate --json)"; rc=$?
[[ $rc -eq 0 ]] \
  && grep -q '"approvals": 1' <<<"$out" \
  && grep -q '"authoritative": 1' <<<"$out" \
  && grep -q '"approval_set_missing": false' <<<"$out" \
  && ok "#2950 shape: later current-head approval discharges the old refusal without name equality" \
  || bad "#2950 shape did not preserve the live clear verdict (rc=$rc): $out"

# A label alone remains non-authoritative.
out="$(run "$tmp/unbound.json" --pr 999 --gate --json)"; rc=$?
[[ $rc -eq 1 ]] \
  && grep -q '"authoritative": 0' <<<"$out" \
  && ok "passed-review label alone remains non-authoritative" \
  || bad "a label without an exact-head review artifact granted authority: $out"

# Stale and malformed records do not become authority merely because labels are
# optional caches.
cat >"$tmp/unlabeled-stale.json" <<JSON
[{"number":2960,"head":"$CUR","labels":[],
  "comments":["APPROVED-AT: claude $OLD"],
  "runs_at_head":2,"mergeable":"MERGEABLE"}]
JSON
out="$(run "$tmp/unlabeled-stale.json" --pr 2960 --gate --json)"; rc=$?
[[ $rc -eq 1 ]] && grep -q '"authoritative": 0' <<<"$out" \
  && ok "an unlabeled stale-head approval remains non-authoritative" \
  || bad "an unlabeled stale-head approval passed (rc=$rc): $out"

cat >"$tmp/unlabeled-malformed.json" <<JSON
[{"number":2961,"head":"$CUR","labels":[],
  "comments":["APPROVED-AT: claude"],
  "runs_at_head":2,"mergeable":"MERGEABLE"}]
JSON
out="$(run "$tmp/unlabeled-malformed.json" --pr 2961 --gate --json)"; rc=$?
[[ $rc -eq 1 ]] \
  && grep -q '"approval_set_missing": true' <<<"$out" \
  && grep -q '"malformed_source_lines": 1' <<<"$out" \
  && ok "an unlabeled malformed approval remains non-authoritative" \
  || bad "an unlabeled malformed approval passed (rc=$rc): $out"

cat >"$tmp/unlabeled-refused.json" <<JSON
[{"number":2962,"head":"$CUR","labels":[],
  "comments":["APPROVED-AT: claude $CUR","CHANGES-REQUESTED-AT: claude $CUR"],
  "runs_at_head":2,"mergeable":"MERGEABLE"}]
JSON
out="$(run "$tmp/unlabeled-refused.json" --pr 2962 --gate --json)"; rc=$?
[[ $rc -eq 1 ]] && grep -q '"authoritative": 0' <<<"$out" \
  && ok "an unresolved objection still blocks an unlabeled approval" \
  || bad "an unresolved objection was cleared without later evidence (rc=$rc): $out"

# The negative control distinguishes no review artifact from a missing cache.
cat >"$tmp/unlabeled-none.json" <<JSON
[{"number":2609,"head":"$CUR","labels":[],
  "lane_shas":{},"runs_at_head":2,"mergeable":"MERGEABLE"}]
JSON
out="$(run "$tmp/unlabeled-none.json" --pr 2609 --gate --json)"; rc=$?
[[ $rc -eq 1 ]] && grep -q '"approval_set_missing": true' <<<"$out" \
  && ok "a genuinely unreviewed PR still reports missing authority" \
  || bad "a genuinely unreviewed PR passed: $out"

# Critical review still requires both exact-head lanes; cache labels are not a
# second source of authority.
cat >"$tmp/unlabeled-critical.json" <<JSON
[{"number":2610,"head":"$CUR","labels":["post-facto-human-review"],
  "comments":["REVIEW-DISPATCH-ATTESTATION\ncritical-class: hermit-syscall\nhead: $CUR\nAPPROVED-AT: claude $CUR\nAPPROVED-AT: codex $CUR"],
  "runs_at_head":2,"mergeable":"MERGEABLE"}]
JSON
out="$(run "$tmp/unlabeled-critical.json" --pr 2610 --gate --json)"
grep -q '"critical_lanes_missing": \[\]' <<<"$out" \
  && grep -q '"authoritative": 2' <<<"$out" \
  && ok "two unlabeled exact-head lanes satisfy the critical review count" \
  || bad "cache labels still control critical review authority: $out"

# Mutation control: restore the cache-as-index defect. The #2956-shaped fixture
# must fail specifically because its comment row disappears from approval_rows.
mutant="$tmp/approval_binding-cache-as-index.py"
sed 's/row for row in rows if row.get("lane") in LANES/row for row in rows if row.get("source") != "comment" and row.get("lane") in LANES/' "$tool" >"$mutant"
if ! cmp -s "$tool" "$mutant"; then
  mout="$(python3 "$mutant" --from-json "$tmp/pr2956-unlabeled.json" --pr 2956 --gate --json 2>&1)"; mrc=$?
  [[ $mrc -eq 1 ]] && grep -q '"approval_set_missing": true' <<<"$mout" \
    && ok "mutation control: restoring cache-as-index fails the #2956 shape" \
    || bad "cache-as-index mutation did not fail the #2956 shape (rc=$mrc): $mout"
else
  bad "mutation control: could not construct cache-as-index mutant"
fi


# Keep the issuer split as provenance, not as a permission boundary. A currently
# authorized GitHub actor may answer either form with a later canonical exact-head
# lane decision or one exact withdrawal carrying the refusing comment id.
#
# Both directions are asserted, because a counter that always says "none" and one
# that always says "present" are each useless in a way the other's test hides.
cat >"$tmp/refusal-unsigned.json" <<JSON
[{"number":9301,"head":"$CUR","labels":["passed-review-claude"],
  "comments":["[adversarial-reviewer agent, codex - dispatched by agent(hermit-004)]\nCHANGES-REQUESTED-AT: claude $CUR",
              "[hermit2, hermit-005, unresolved, devbig014, role=reviewer]\nAPPROVED-AT: claude $OLD"],
  "runs_at_head":3,"mergeable":"MERGEABLE"}]
JSON
out="$(run "$tmp/refusal-unsigned.json" --pr 9301 --gate)"
if grep -q '^refusal_issuer_summary=1 unanswered refusal(s) at head across 1 PR(s); 0 name an issuer, 1 name none' <<<"$out"; then
  ok "a refusal with no readable issuer is counted as naming none"
else
  bad "an unsigned refusal was not reported as naming no issuer: $out"
fi
if grep -q 'RETIRES #<the exact refusing comment id>' <<<"$out"; then
  ok "the remedy names the exact mechanism that retires an objection"
else
  bad "the remedy did not name exact RETIRES, so the reader cannot resolve the objection"
fi

cat >"$tmp/refusal-signed.json" <<JSON
[{"number":9302,"head":"$CUR","labels":["passed-review-claude"],
  "comments":["[hermit2, hermit-016, unresolved, devbig014, role=reviewer]\nCHANGES-REQUESTED-AT: claude $CUR",
              "[hermit2, hermit-005, unresolved, devbig014, role=reviewer]\nAPPROVED-AT: claude $OLD"],
  "runs_at_head":3,"mergeable":"MERGEABLE"}]
JSON
out="$(run "$tmp/refusal-signed.json" --pr 9302 --gate)"
if grep -q '^refusal_issuer_summary=1 unanswered refusal(s) at head across 1 PR(s); 1 name an issuer, 0 name none' <<<"$out"; then
  ok "a refusal carrying a disclosure tag is counted as naming an issuer"
else
  bad "a signed refusal was miscounted; the split cannot separate the two remedies: $out"
fi
if grep -q 'currently authorized GitHub actor may post a canonical withdrawal' <<<"$out"; then
  ok "the signed-refusal remedy no longer depends on a departed issuer"
else
  bad "the signed-refusal remedy still leaves a departed issuer as a permanent gate: $out"
fi

# The negative half. Without it the counter could report "present" unconditionally.
cat >"$tmp/refusal-none.json" <<JSON
[{"number":9303,"head":"$CUR","labels":["passed-review-claude"],
  "comments":["[hermit2, hermit-005, unresolved, devbig014, role=reviewer]\nAPPROVED-AT: claude $CUR"],
  "runs_at_head":3,"mergeable":"MERGEABLE"}]
JSON
out="$(run "$tmp/refusal-none.json" --pr 9303 --gate)"
if grep -q '^refusal_issuer_state=none' <<<"$out"; then
  ok "a head with no unanswered refusal reports refusal_issuer_state=none"
else
  bad "a clean head reported a refusal issuer split, so the counter is not reading refusals: $out"
fi

# ⚠️ ITS OWN MUTATION CONTROL. A split that always reports zero unsigned refusals
# would pass the signed and the clean cases above and fail only the first, so the
# control removes the DISCRIMINATION rather than the whole block: it drops the
# issuer from every row, which must red the signed case.
if [[ -z "${APPROVAL_BINDING_MUTATION_CHILD:-}" ]]; then
  mutant="$tmp/approval_binding-without-refusal-issuers.py"
  # ⚠️ MATCHES BOTH ROW BUILDERS. The live path keys on `head` and the fixture
  # path on `pr["head"]`, so a pattern anchored to either one leaves the other
  # intact -- and the fixture path is the ONLY one this inert suite exercises, so
  # anchoring on `head` produced a mutant that ran unmutated and reported the
  # guard as not load-bearing. That is the same "the mutant never ran" failure
  # this file's header records, one substitution down.
  sed 's/"refusal_issuers": \[w for _s, w in unanswered/"refusal_issuers": [None for _s, w in unanswered/g' \
    "$tool" >"$mutant"
  if ! grep -q '"refusal_issuers": \[None for _s, w in unanswered' "$mutant"; then
    bad "refusal-issuer mutation control could not locate the row field"
  else
    mout="$(python3 "$mutant" --from-json "$tmp/refusal-signed.json" --pr 9302 --gate 2>&1)"
    if grep -q '1 name an issuer, 0 name none' <<<"$mout"; then
      bad "erasing every refusal issuer did not change the split -- the field is not load-bearing"
    else
      ok "mutation control: erasing the refusal issuer reds the signed-refusal case"
    fi
  fi
fi

echo "======================================================================"
echo "refused-when-it-should:     1 case   (selected PR with zero approvals)"
echo "flagged-when-it-should:     4 cases  (superseded, unbound 0/1 authority, composed, malformed+lost)"
echo "not-flagged-when-it-should: 2 cases  (exact-head bindings; malformed-but-authoritative)"
echo "assertions: $pass passed, $fail failed"
if (( fail )); then echo "RESULT: FAIL"; exit 1; fi
echo "RESULT: PASS"
