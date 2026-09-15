#!/usr/bin/env bash
# INERT both-sided bracket for no-op-rebase routing in the canonical lander.
#
# THE DEFECT THIS EXISTS FOR, measured on
# https://github.com/rrnewton/hermit/pull/2944 at exact head
# 208c63eb89f0dddb5dc7ecacdf9f75074de1b821:
#
#   1. the pre-check refuses with "no rebase record for result X",
#   2. the lander DEFERS that refusal on the stated promise that step 2's rebase
#      will create the record -- which also skips the just-in-time exact-head
#      block, because that block is guarded on the very VRC the deferral zeroes,
#   3. the rebase is a NO-OP, because the head already contains current main, so
#      step 3 records nothing (it records only when HEAD differs from ORIG),
#   4. step 4's unconditional re-authorize refuses the unchanged head for the
#      same missing record, and abandons.
#
# Deterministic, and no retry can ever succeed. The repair routes the exact
# unchanged head through the supported just-in-time exact-head validation path
# before post-push authorization.
#
# ⚠️ WHAT MUST NOT BE THE FIX, and is asserted against below. Writing the
# missing record would satisfy the message and land nothing: the record is a
# SECONDARY message emitted after hard-green has already been demoted, so an
# agent that reads it literally fabricates provenance and is still refused.
# Fabricating provenance to satisfy a gate is the thing a previous lane
# correctly declined to do.
#
# This runs no git, no network, no PR read, and no merge. It audits the
# production lander's wiring, and -- the leg that is usually skipped -- proves
# that audit can FAIL, by running it against mutants with the repair removed.
# An audit that passes on a lander with the fix deleted is not evidence.
set -euo pipefail

script_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
lander=$script_dir/land-pr.sh
tmp=$(mktemp -d)
trap 'rm -rf -- "$tmp"' EXIT

pos_ok=0; pos_total=0
neg_caught=0; neg_total=0

# The wiring the repair consists of. Each clause is a separate, quotable fact
# about the lander rather than one opaque regex, so a failure names which part
# went missing.
audit_lander() {
  local f=$1 quiet=${2:-}
  local missing=()
  grep -Fq 'deferred_missing_record=0' "$f" || missing+=("flag-initialised")
  grep -Fq 'deferred_missing_record=1' "$f" || missing+=("flag-set-on-deferral")
  grep -Fq 'attempt_jit_exact_head_validation() {' "$f" || missing+=("jit-path-factored")
  grep -Fq 'attempt_jit_exact_head_validation "$ORIG"' "$f" || missing+=("precheck-still-uses-jit")
  grep -Fq 'attempt_jit_exact_head_validation "$HEAD"' "$f" || missing+=("no-op-discharges-to-jit")
  grep -Fq '[ "$deferred_missing_record" -eq 1 ]' "$f" || missing+=("discharge-gated-on-deferral")
  # Preserved, not replaced: the rewritten-rebase provenance path is still
  # exactly the condition the existing eligibility bracket asserts at its
  # consumer audit, and the post-push re-authorize is still unconditional.
  grep -Fq '[ "$plain_rebase" -eq 1 ] && [ "$HEAD" != "$ORIG" ]' "$f" \
    || missing+=("rewritten-rebase-record-preserved")
  grep -Fq 'rebase_wrapper.py" record' "$f" || missing+=("record-path-present")
  if [ "${#missing[@]}" -ne 0 ]; then
    [ -n "$quiet" ] || printf '   missing: %s\n' "${missing[*]}"
    return 1
  fi
  return 0
}

# NON-SOLUTION GUARD. The no-op branch must not invoke `record`. Returns
# non-zero when the branch fabricates provenance.
audit_no_fabricated_record() {
  local f=$1
  # The span from the no-op branch to the end of the enclosing if.
  awk '/rebase provenance unchanged: rebase did not rewrite/,/^fi$/' "$f" \
    | grep -Fq 'rebase_wrapper.py" record' && return 1
  return 0
}

echo "== POSITIVE: the production lander carries the repair =="
pos_total=$((pos_total + 1))
if bash -n "$lander"; then
  echo "   lander parses"
  pos_ok=$((pos_ok + 1))
else
  echo "   lander DOES NOT PARSE"
fi
pos_total=$((pos_total + 1))
if audit_lander "$lander"; then
  echo "   all repair wiring present, and the rewritten-rebase record path is preserved"
  pos_ok=$((pos_ok + 1))
else
  echo "   repair wiring INCOMPLETE"
fi
pos_total=$((pos_total + 1))
if audit_no_fabricated_record "$lander"; then
  echo "   no-op branch does not write a rebase record"
  pos_ok=$((pos_ok + 1))
else
  echo "   no-op branch FABRICATES a rebase record"
fi

echo
echo "== NEGATIVE: the audit above must FAIL when the repair is removed =="
# Each mutant deletes exactly one load-bearing piece. If the audit still passes,
# the audit is not measuring that piece and this test would be decoration.
mutate() {
  local name=$1 sed_expr=$2
  neg_total=$((neg_total + 1))
  local m=$tmp/$name.sh
  sed "$sed_expr" "$lander" > "$m"
  if audit_lander "$m" quiet; then
    echo "   NOT CAUGHT: $name -- the audit passes with this removed"
  else
    echo "   caught: $name"
    neg_caught=$((neg_caught + 1))
  fi
}
mutate no-discharge         '/attempt_jit_exact_head_validation "\$HEAD"/d'
mutate no-flag-set          '/deferred_missing_record=1/d'
mutate no-flag-init         '/deferred_missing_record=0/d'
mutate discharge-ungated    '/\[ "\$deferred_missing_record" -eq 1 \]/d'
mutate jit-not-factored     '/attempt_jit_exact_head_validation() {/d'
mutate record-path-dropped  '/\[ "\$plain_rebase" -eq 1 \] \&\& \[ "\$HEAD" != "\$ORIG" \]/d'

# And the non-solution: a lander that fabricates a record on the no-op path must
# be caught by the guard, not waved through.
neg_total=$((neg_total + 1))
fab=$tmp/fabricated-record.sh
awk '{print}
  /rebase provenance unchanged: rebase did not rewrite/ {
    print "    python3 \"$SCRIPT_DIR/rebase_wrapper.py\" record --source \"$ORIG\""
  }' "$lander" > "$fab"
if audit_no_fabricated_record "$fab"; then
  echo "   NOT CAUGHT: fabricated-record -- the guard permits writing a record on the no-op path"
else
  echo "   caught: fabricated-record"
  neg_caught=$((neg_caught + 1))
fi

echo
printf 'POSITIVE checks passed: %d/%d   NEGATIVE mutants caught: %d/%d\n' \
  "$pos_ok" "$pos_total" "$neg_caught" "$neg_total"
if [ "$pos_ok" -ne "$pos_total" ] || [ "$neg_caught" -ne "$neg_total" ]; then
  echo "FAIL: no-op rebase routing bracket" >&2
  exit 1
fi
echo "PASS: a deferred missing-record pre-check followed by a no-op rebase routes the unchanged head through just-in-time exact-head validation; no record is fabricated; the rewritten-rebase provenance path is unchanged"
