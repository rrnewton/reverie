#!/usr/bin/env bash
# BEHAVIOURAL bracket for no-op-rebase routing: it EXECUTES land-pr.sh.
#
# Its companion, test-no-op-rebase-routing.sh, audits the lander's source text.
# An independent review of PR 365 pointed out that a source audit can pass while
# the named execution path never runs, and that a source audit is not adequate
# evidence for a change to a live lander. It is right, so this runs the real
# script against stub programs and asserts on what it actually did.
#
# NOTHING REAL IS TOUCHED. `git`, `gh`, `python3`, `jq`, `with-proxy`, `flock`
# and `timeout` are replaced on PATH by the stubs in testdata/, `ci-hub` and
# `pr-head-ci` by stubs in a temp tree. No network, no repository, no PR read,
# no push, no merge.
#
# THE AUTHORITY IS MODELLED FAITHFULLY, which is what makes this a test rather
# than a fixture: the stub authorizes a head when it has EITHER a qualifying
# exact-head verdict (hard green) OR recorded rebase provenance (soft green),
# and otherwise refuses with "no rebase record for result X" -- the secondary
# message that is only emitted after hard-green has already been demoted.
#
# --UNION IS COVERED TOO, and finding that mattered: making this harness honest
# is what revealed that the union path takes the same deferral while nothing on
# it writes a record. That is fixed in land-pr.sh rather than pinned here.
#
# THE CONTROL IS A MUTANT THAT MUST REPRODUCE THE DEFECT. The discharge is
# removed from a copy of the lander with sed, and that copy must abandon on the
# no-op scenario. A fix whose absence changes nothing is not a fix, and a test
# that passes either way is not evidence.
set -euo pipefail

script_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
lander=$script_dir/land-pr.sh
stubs=$script_dir/testdata
tmp=$(mktemp -d)
# Resolved before the stub directory goes first on PATH; only the scenarios
# that must read a real validate-status report hand it to the jq stub.
real_jq=$(command -v jq) || { echo "FAIL: this bracket needs jq on PATH" >&2; exit 1; }
trap 'rm -rf -- "$tmp"' EXIT

root=$tmp/root
mkdir -p "$root/ci-hub/landing" "$root/ci-hub/bin" "$tmp/bin" "$tmp/wt"
cp "$lander" "$root/ci-hub/landing/land-pr.sh"
cp "$script_dir/worktree-mutation-fence.sh" "$root/ci-hub/landing/" 2>/dev/null || true
# Required: the lander refuses to start without its --no-rebase predicate.
cp "$script_dir/head-contains-main.sh" "$root/ci-hub/landing/"
cp "$script_dir/merge-gate-status.jq" "$root/ci-hub/landing/" 2>/dev/null || true
cp "$stubs"/git "$stubs"/gh "$stubs"/python3 "$stubs"/jq \
   "$stubs"/with-proxy "$stubs"/flock "$stubs"/timeout "$tmp/bin/"
cp "$stubs/union-rebase-stub" "$root/ci-hub/landing/union-rebase.sh"
cp "$stubs/ci-hub-stub" "$root/ci-hub/ci-hub"
cp "$stubs/pr-head-ci-stub" "$root/ci-hub/bin/pr-head-ci"
chmod +x "$tmp/bin"/* "$root/ci-hub/ci-hub" "$root/ci-hub/bin/pr-head-ci" \
   "$root/ci-hub/landing/union-rebase.sh" "$root/ci-hub/landing/land-pr.sh"

# The control: the same lander with the discharge deleted. If this does not
# reproduce the abandon, the scenario is not exercising the defect.
sed '/attempt_jit_exact_head_validation "\$HEAD"/d' "$lander" \
  > "$root/ci-hub/landing/land-pr-nodischarge.sh"
chmod +x "$root/ci-hub/landing/land-pr-nodischarge.sh"

pass=0; fail=0
check() { # description, expected, actual
  if [ "$2" = "$3" ]; then printf '   ok    %-58s %s\n' "$1" "$3"; pass=$((pass+1))
  else printf '   FAIL  %-58s want=%s got=%s\n' "$1" "$2" "$3"; fail=$((fail+1)); fi
}

run() { # label, lander, then extra lander args; scenario via SCEN_* in env
  local label=$1 which=$2; shift 2
  export SCEN_DIR=$tmp/scen-$label; rm -rf "$SCEN_DIR"; mkdir -p "$SCEN_DIR"
  export CALLS=$tmp/calls-$label.log; : > "$CALLS"
  set +e
  PATH="$tmp/bin:$PATH" timeout 60 bash "$which" 999 feature \
    --checkout "$tmp/wt" --agent test --foreground "$@" > "$tmp/out-$label.txt" 2>&1
  echo $? > "$tmp/rc-$label"
  set -e
}
saw() { grep -cE "$2" "$tmp/out-$1.txt" | head -1; }
rc()  { cat "$tmp/rc-$1"; }

echo "== SCENARIO: pre-check deferred, then the rebase is a no-op =="
export SCEN_ORIG=bbbb222 SCEN_HEAD=bbbb222 SCEN_MAIN=aaaa111 SCEN_ANCESTOR=1
unset SCEN_JIT_RC || true

run nodischarge "$root/ci-hub/landing/land-pr-nodischarge.sh"
echo " control, discharge removed -- must reproduce the defect:"
check "refuses for the missing record" 2 "$(saw nodischarge 'no rebase record for result')"
check "never reaches just-in-time validation" 0 "$(saw nodischarge 'just-in-time exact-head CI\(')"
check "abandons" 4 "$(rc nodischarge)"

run fixed "$root/ci-hub/landing/land-pr.sh"
echo " production lander -- must discharge and clear:"
check "discharge fires" 1 "$(saw fixed 'no-op rebase after a deferred')"
check "just-in-time validation runs on the unchanged head" 1 "$(saw fixed 'just-in-time exact-head CI\(head=bbbb222\) rc=0')"
check "landability then clears" 1 "$(saw fixed 'post-JIT validation landability\(head=bbbb222\) rc=0')"
check "does not abandon for the missing record" 0 "$(saw fixed 'ABANDON.*no accepted validation landability')"
check "writes no rebase record" 0 "$(grep -cE 'rebase_wrapper.py record' "$tmp/calls-fixed.log")"
check "clears post-push authorization" 1 "$(saw fixed 'post-push validation landability\(head=bbbb222\) rc=0')"
check "reaches the merge gate" 1 "$(saw fixed 'merge-gate diagnostic only:')"

echo
echo "== SCENARIO: the rebase rewrites the head (provenance path preserved) =="
SCEN_ORIG=bbbb222 SCEN_HEAD=cccc333 run rewritten "$root/ci-hub/landing/land-pr.sh"
check "discharge does not fire" 0 "$(saw rewritten 'no-op rebase after a deferred')"
check "a rebase record is written" 1 "$(grep -cE 'rebase_wrapper.py record' "$tmp/calls-rewritten.log")"
check "clears post-push authorization at the NEW head" 1 "$(saw rewritten 'post-push validation landability\(head=cccc333\) rc=0')"
check "reaches the merge gate" 1 "$(saw rewritten 'merge-gate diagnostic only:')"

echo
echo "== SCENARIO: --no-rebase (the deferral is not taken at all) =="
SCEN_ORIG=bbbb222 SCEN_HEAD=bbbb222 run norebase "$root/ci-hub/landing/land-pr.sh" --no-rebase
check "deferral does not fire" 0 "$(saw norebase 'deferring to the post-push')"
check "discharge does not fire" 0 "$(saw norebase 'no-op rebase after a deferred')"
check "pre-check just-in-time path still reachable" 1 "$(saw norebase 'just-in-time exact-head CI\(head=bbbb222\)')"
check "clears post-push authorization" 1 "$(saw norebase 'post-push validation landability\(head=bbbb222\) rc=0')"
check "reaches the merge gate" 1 "$(saw norebase 'merge-gate diagnostic only:')"

echo
echo "== SCENARIO: --union (its own driver rebases and pushes) =="
SCEN_ORIG=bbbb222 SCEN_HEAD=bbbb222 run union "$root/ci-hub/landing/land-pr.sh" --union
# ⚠️ The union driver must actually RUN. An earlier revision of this file never
# supplied union-rebase.sh, so this scenario abandoned because the driver was
# missing and then "passed" on the absence of the discharge message -- a pass
# for a reason that had nothing to do with the property under test.
check "the union driver is invoked" 1 "$(grep -cE '^union-rebase ' "$tmp/calls-union.log")"
check "the union result is accepted" 1 "$(saw union '^\[land .*\] union:')"
check "discharge does not fire" 0 "$(saw union 'no-op rebase after a deferred')"
# --union is now EXCLUDED from the deferral, because nothing on the union path
# writes a rebase record: union-rebase.sh never calls rebase_wrapper, and the
# lander's own `record` is gated on plain_rebase, which --union leaves at 0.
# Deferring there promised something union cannot deliver. With the exclusion,
# the pre-check just-in-time path runs instead and can earn a real verdict.
check "the deferral is not taken on the union path" 0 "$(saw union 'deferring to the post-push')"
check "pre-check just-in-time validation runs instead" 1 "$(saw union 'just-in-time exact-head CI\(head=bbbb222\) rc=0')"
check "clears post-push authorization" 1 "$(saw union 'post-push validation landability\(head=bbbb222\) rc=0')"
check "reaches the merge gate" 1 "$(saw union 'merge-gate diagnostic only:')"

echo
echo "== SCENARIO: just-in-time validation produces no verdict =="
SCEN_ORIG=bbbb222 SCEN_HEAD=bbbb222 SCEN_JIT_RC=4 run jitfail "$root/ci-hub/landing/land-pr.sh"
check "refuses naming the real condition" 1 "$(saw jitfail 'just-in-time exact-head CI produced no qualifying verdict')"
check "abandons rather than landing" 4 "$(rc jitfail)"
unset SCEN_JIT_RC

echo
echo "== SCENARIO: the head moves between step 2 and step 3 =="
SCEN_ORIG=bbbb222 SCEN_HEAD=dddd444 run concurrent "$root/ci-hub/landing/land-pr.sh"
check "discharge does not fire on a changed head" 0 "$(saw concurrent 'no-op rebase after a deferred')"
# ⚠️ DELIBERATELY NOT asserting that production WOULD authorize a concurrently
# replaced head. The stub authorizes any result once a record exists, whereas
# the real rebase_wrapper replays source onto base and requires the recomputed
# tree to match. What this scenario can honestly prove is that the lander
# re-reads the moved head and evaluates authorization AT THAT HEAD rather than
# at the one it started from, which is the property the discharge must not
# disturb.
check "authorization is evaluated at the MOVED head" 1 "$(saw concurrent 'post-push validation landability\(head=dddd444\)')"
check "not at the head it started from" 0 "$(saw concurrent 'post-push validation landability\(head=bbbb222\)')"

echo
echo "== SCENARIO: exit 4 held only by evidence readable on another host =="
# https://github.com/rrnewton/dev-hermit/issues/395: validate-status exits 4
# (NEEDS-RERUN) when a relevant ledger row names evidence that exists only on
# its producing host. That row stays relevant after any rerun, so the lander
# must not spend a just-in-time validate (about 46 minutes) on it. The JSON is
# read with the host jq; the default jq stub prints nothing.
unverifiable_json='{"verdict":"NEEDS-RERUN","unresolved_failure_obligations":[],"unverifiable_evidence":[{"run_id":"devbig030-1790000000-1","commit":"4c91b9520000000000000000000000000000abcd","producing_host":"devbig030","local_host":"devbig014","state":"foreign-host-artifact-absent","missing_artifact":"ignored/validate/artifacts/devbig030-1790000000-1/constructed-plan.json"}]}'
held='canonical exact-head status for bbbb222 is held by 1 relevant ledger row\(s\) whose validation evidence exists only on another host'
SCEN_REAL_JQ=$real_jq SCEN_VSTATUS_JSON=$unverifiable_json \
  run unverifiable "$root/ci-hub/landing/land-pr.sh" --no-rebase
check "names the foreign-host evidence once" 1 "$(saw unverifiable "$held")"
check "links the durable-fix issue" 1 "$(saw unverifiable 'dev-hermit/issues/412')"
check "never calls pr-head-ci" 0 "$(grep -cE '^pr-head-ci ' "$tmp/calls-unverifiable.log" || true)"
check "never reports a just-in-time run" 0 "$(saw unverifiable 'just-in-time exact-head CI\(')"
check "abandons at the pre-check it could not clear" 1 "$(saw unverifiable 'validation landability refused PR head bbbb222')"
check "abandons" 4 "$(rc unverifiable)"

SCEN_REAL_JQ=$real_jq SCEN_VSTATUS_JSON=$unverifiable_json \
  run unverifiable_noop "$root/ci-hub/landing/land-pr.sh"
echo " the deferred no-op discharge goes through the same function:"
check "discharge fires" 1 "$(saw unverifiable_noop 'no-op rebase after a deferred')"
check "names the foreign-host evidence once" 1 "$(saw unverifiable_noop "$held")"
check "never calls pr-head-ci" 0 "$(grep -cE '^pr-head-ci ' "$tmp/calls-unverifiable_noop.log" || true)"
check "post-push authorization still refuses" 1 "$(saw unverifiable_noop 'pushed head bbbb222 has no accepted validation landability')"
check "abandons" 4 "$(rc unverifiable_noop)"

echo " control, an EMPTY list read by the same jq still reaches the validator:"
SCEN_REAL_JQ=$real_jq \
SCEN_VSTATUS_JSON='{"verdict":"NOT_VALIDATED","unresolved_failure_obligations":[],"unverifiable_evidence":[]}' \
  run emptyunverifiable "$root/ci-hub/landing/land-pr.sh" --no-rebase
check "does not claim foreign-host evidence" 0 "$(saw emptyunverifiable 'exists only on another host')"
check "calls pr-head-ci once" 1 "$(grep -cE '^pr-head-ci ' "$tmp/calls-emptyunverifiable.log" || true)"
check "just-in-time validation runs" 1 "$(saw emptyunverifiable 'just-in-time exact-head CI\(head=bbbb222\) rc=0')"
check "reads hard green with the real jq" 1 "$(grep -cE '^ci-hub apply-local-label ' "$tmp/calls-emptyunverifiable.log" || true)"
check "reaches the merge gate" 1 "$(saw emptyunverifiable 'merge-gate diagnostic only:')"

echo
printf 'behavioural checks: %d passed, %d failed\n' "$pass" "$fail"
if [ "$fail" -ne 0 ]; then
  echo "FAIL: no-op rebase behavioural bracket" >&2
  exit 1
fi
echo "PASS: the deferred no-op discharges to just-in-time exact-head validation and clears post-push authorization; removing the discharge reproduces the abandon; the rewritten, --no-rebase, failed-validation and moved-head paths each reach their own expected milestone and termination, so none of them is merely missing the discharge message; --union no longer takes the deferral at all, because nothing on that path writes a record, and earns its verdict through the pre-check instead; an exit 4 held only by evidence on another host never reaches pr-head-ci, while an empty list still does"
