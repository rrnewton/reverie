#!/usr/bin/env bash
# Bracket unevaluated_head.py in BOTH directions.
#
# An inert detector is the failure mode this guards against, not a strict one.
# The review of PR #2026 found a mandatory-review gate that accepted six evasion
# mutations 6/6 -- it enforced nothing and nobody noticed, because a check that
# never fires looks exactly like a check with nothing to report. So this suite
# asserts the POSITIVE half as hard as the negative:
#
#   FLAGGED     a head with zero runs                      -> NOT_EVALUATED, gate rc=1
#   NOT FLAGGED a head whose runs all succeeded            -> EVALUATED_PASS,  gate rc=0
#   NOT FLAGGED a head with a FAILING run                  -> EVALUATED_FAIL,  gate rc=0
#
# That third case is the distinction the whole tool exists for. A failing gate is
# a RESULT; no gate at all is not. If a red head were reported as NOT_EVALUATED
# the tool would be laundering one problem into another.
#
# Inert: pure fixtures through --from-json. No network, no gh, no live repo.
set -uo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
tool="$here/../unevaluated_head.py"
tmp="$(mktemp -d "${TMPDIR:-/tmp}/unevaluated-head-test.XXXXXX")"
trap 'rm -rf "$tmp"' EXIT

pass=0; fail=0
ok()  { printf '  PASS  %s\n' "$*"; pass=$((pass+1)); }
bad() { printf '  FAIL  %s\n' "$*"; fail=$((fail+1)); }

# --- POSITIVE: a head with zero runs MUST be flagged -------------------------
cat >"$tmp/zero.json" <<'JSON'
[{"number": 1864, "head": "b2e44120e984e85ab6a70fd5749ad6d3282a352e",
  "mergeable": "CONFLICTING", "runs": []}]
JSON
out="$(python3 "$tool" --from-json "$tmp/zero.json" --gate 2>&1)"; rc=$?
if [[ $rc -eq 1 ]]; then ok "zero-run head: gate FAILS (rc=1)"; else bad "zero-run head: expected rc=1, got $rc"; fi
grep -q "NOT_EVALUATED  PR 1864" <<<"$out" && ok "zero-run head is named with its PR and state" \
  || bad "zero-run head not named: $out"
grep -q "cannot build refs/pull" <<<"$out" && ok "states the known cause (conflict) rather than shrugging" \
  || bad "no cause given for the conflicting case"

# --- NEGATIVE 1: a genuinely passing head MUST NOT be flagged ----------------
cat >"$tmp/pass.json" <<'JSON'
[{"number": 1666, "head": "26d47f007b54d2a60d31a8d22f2ce30245c0e95f",
  "mergeable": "MERGEABLE",
  "runs": [{"conclusion": "success"}, {"conclusion": "success"}]}]
JSON
out="$(python3 "$tool" --from-json "$tmp/pass.json" --gate 2>&1)"; rc=$?
if [[ $rc -eq 0 ]]; then ok "passing head: gate PASSES (rc=0)"; else bad "passing head: expected rc=0, got $rc"; fi
# Match the per-ROW marker, not the bare token: the summary line legitimately
# contains "NOT_EVALUATED: 0", and a looser grep here fails on correct output.
# (It did, on the first run of this suite.)
grep -qE "^  NOT_EVALUATED  PR" <<<"$out" && bad "passing head wrongly reported NOT_EVALUATED" \
  || ok "passing head is not reported as unevaluated"

# --- NEGATIVE 2: a FAILING head is evaluated, not unevaluated ----------------
# The distinction the tool exists for. A red head must not be laundered into
# "never ran": that would convert a visible failure into an invisible absence.
cat >"$tmp/red.json" <<'JSON'
[{"number": 1727, "head": "49321e67e6356d3f1f26d897ef8193a21c7e28ef",
  "mergeable": "CONFLICTING",
  "runs": [{"conclusion": "success"}, {"conclusion": "failure"}]}]
JSON
out="$(python3 "$tool" --from-json "$tmp/red.json" --gate --json 2>&1)"; rc=$?
if [[ $rc -eq 0 ]]; then ok "failing head: gate PASSES (a red gate is another check's job)"; else bad "failing head: expected rc=0, got $rc"; fi
grep -q '"state": "EVALUATED_FAIL"' <<<"$out" && ok "failing head classified EVALUATED_FAIL, not NOT_EVALUATED" \
  || bad "failing head misclassified: $out"

# --- Mixed population: both terms reported ----------------------------------
cat >"$tmp/mixed.json" <<'JSON'
[{"number": 1, "head": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", "mergeable": "CONFLICTING", "runs": []},
 {"number": 2, "head": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb", "mergeable": "MERGEABLE",   "runs": [{"conclusion": "success"}]},
 {"number": 3, "head": "cccccccccccccccccccccccccccccccccccccccc", "mergeable": "MERGEABLE",   "runs": [{"conclusion": "failure"}]}]
JSON
out="$(python3 "$tool" --from-json "$tmp/mixed.json" 2>&1)"
grep -q "inspected 3 open PR head(s); NOT_EVALUATED: 1" <<<"$out" \
  && ok "reports BOTH terms (1 of 3), not a bare count" \
  || bad "did not report both terms: $out"

# --- A cause is not invented where none is known -----------------------------
cat >"$tmp/unknown.json" <<'JSON'
[{"number": 9, "head": "dddddddddddddddddddddddddddddddddddddddd", "mergeable": "MERGEABLE", "runs": []}]
JSON
out="$(python3 "$tool" --from-json "$tmp/unknown.json" 2>&1)"
grep -q "cause not established" <<<"$out" \
  && ok "mergeable-but-unevaluated says the cause is NOT established rather than blaming conflict" \
  || bad "invented a cause for a non-conflicting unevaluated head: $out"

echo "======================================================================"
echo "flagged-when-it-should:     1 case   (zero runs)"
echo "not-flagged-when-it-should: 2 cases  (all-success, and a genuine red)"
echo "assertions: $pass passed, $fail failed"
if (( fail )); then echo "RESULT: FAIL"; exit 1; fi
echo "RESULT: PASS"
