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
#   REFUSED     selected PR with ZERO approval labels  -> missing authority
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
grep -q "PR #2132 has zero passed-review labels" <<<"$out" \
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
grep -q "^summary=1/1 approval labels fail the all binding check; affected=#1864;" <<<"$out" \
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
grep -q "authoritative at current head: 0; authority absent: 1" <<<"$out" \
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
grep -q "every approval label is bound to the current head" <<<"$out" \
  && ok "current approvals reported as holding" || bad "current approvals not reported clean: $out"
grep -q "authoritative at current head: 2; authority absent: 0" <<<"$out" \
  && ok "exact-head bindings count as 2/2 authority" \
  || bad "bound authority count is not explicit: $out"
# The claude lane re-reviewed: an older SHA followed by the current one still holds.
grep -qE "SUPERSEDED|UNDETERMINED" <<<"$out" && bad "a re-review (old SHA then current) was wrongly flagged" \
  || ok "a lane that re-reviewed at the new head holds, despite an older SHA in its history"

# --- THE COMPOSED STATE: superseded AND zero runs ---------------------------
cat >"$tmp/combined.json" <<JSON
[{"number":1864,"head":"$CUR","labels":["passed-review-codex"],
  "lane_shas":{"codex":["$OLD"]},"runs_at_head":0,"mergeable":"CONFLICTING"}]
JSON
out="$(run "$tmp/combined.json" --gate)"
grep -q "APPROVED + UNEVALUATED" <<<"$out" \
  && ok "approved-and-unevaluated is called out specifically, not just as one more stale label" \
  || bad "composed state not distinguished: $out"
grep -q "approved AND unevaluated: 1" <<<"$out" \
  && ok "reports the crossed count with its population" || bad "no crossed count: $out"

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
[[ $rc -eq 1 ]] && ok "--gate-on superseded: the combined approved+unevaluated state STILL pages" \
  || bad "--gate-on superseded missed the combined state (rc=$rc)"
out="$(run "$tmp/unbound.json" --gate --gate-on unverifiable)"; rc=$?
[[ $rc -eq 1 ]] && ok "--gate-on unverifiable: the standing condition pages on its own daily entry" \
  || bad "--gate-on unverifiable did not page (rc=$rc)"
out="$(run "$tmp/unbound.json" --gate)"; rc=$?
[[ $rc -eq 1 ]] && ok "default --gate is UNCHANGED: still pages on anything not BOUND_CURRENT" \
  || bad "default gate contract regressed (rc=$rc)"


# --- the binding GRAMMAR, including the rejection trap -----------------------
if python3 "$here/test_approval_binding_grammar.py" >"$tmp/grammar.out" 2>&1; then
  ok "binding grammar suite: $(grep -c '^  PASS' "$tmp/grammar.out") assertions pass"
else
  bad "binding grammar suite FAILED"; sed 's/^/        /' "$tmp/grammar.out" | grep FAIL | head -4
fi

# --- MUTATION CONTROL: removing the load-bearing guard breaks this suite ----
# Run this complete bracket against a temporary copy whose single-PR empty-set
# predicate is disabled.  Nonzero alone is insufficient: name the exact
# #2132 assertion so an unrelated mutant failure cannot masquerade as proof.
if [[ -z "${APPROVAL_BINDING_MUTATION_CHILD:-}" ]]; then
  mutant="$tmp/approval_binding-without-zero-guard.py"
  sed \
    's/approval_set_missing = a.pr is not None and not rows/approval_set_missing = False  # MUTANT: zero-approval guard removed/' \
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

echo "======================================================================"
echo "refused-when-it-should:     1 case   (selected PR with zero approvals)"
echo "flagged-when-it-should:     3 cases  (superseded, unbound 0/1 authority, composed)"
echo "not-flagged-when-it-should: 1 case   (exact-head bindings 2/2 authority)"
echo "assertions: $pass passed, $fail failed"
if (( fail )); then echo "RESULT: FAIL"; exit 1; fi
echo "RESULT: PASS"
