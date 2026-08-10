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
tool="$here/../approval_binding.py"
tmp="$(mktemp -d "${TMPDIR:-/tmp}/approval-binding-test.XXXXXX")"
trap 'rm -rf "$tmp"' EXIT

CUR=b2e44120e984e85ab6a70fd5749ad6d3282a352e
OLD=87f62930d3e24ab75b516d01cba106161e0cca2e

pass=0; fail=0
ok()  { printf '  PASS  %s\n' "$*"; pass=$((pass+1)); }
bad() { printf '  FAIL  %s\n' "$*"; fail=$((fail+1)); }

run() { python3 "$tool" --from-json "$1" "${@:2}" 2>&1; }

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

# --- NEGATIVE: an approval naming the CURRENT head must NOT be flagged -------
# Without this the tool could reject everything and look diligent.
cat >"$tmp/current.json" <<JSON
[{"number":1666,"head":"$CUR","labels":["passed-review-claude","passed-review-codex"],
  "lane_shas":{"claude":["$OLD","$CUR"],"codex":["$CUR"]},"runs_at_head":16,"mergeable":"MERGEABLE"}]
JSON
out="$(run "$tmp/current.json" --gate)"; rc=$?
[[ $rc -eq 0 ]] && ok "current approvals: gate PASSES (rc=0)" || bad "current: expected rc=0, got $rc"
grep -q "every approval label is bound to the current head" <<<"$out" \
  && ok "current approvals reported as holding" || bad "current approvals not reported clean: $out"
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

echo "======================================================================"
echo "flagged-when-it-should:     3 cases  (superseded, unbound, composed)"
echo "not-flagged-when-it-should: 1 case   (approval naming the current head)"
echo "assertions: $pass passed, $fail failed"
if (( fail )); then echo "RESULT: FAIL"; exit 1; fi
echo "RESULT: PASS"
